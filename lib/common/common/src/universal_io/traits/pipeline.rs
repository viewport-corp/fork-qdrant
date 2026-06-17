//! Read pipelines.
//!
//! Workflow:
//! 1. Push read operations using `schedule()`.
//! 2. Pull the completed results using `wait()`.
//! 3. Interleave steps 1 and 2 as needed.
//!
//! Each backend implements a single [`ReadPipeline`]: the *borrowed* variant,
//! where every read is scheduled against an externally-held `&'file File` and
//! [`wait`](ReadPipeline::wait) yields data bounded by that `'file`.
//!
//! The owned variant – a pipeline that *owns* its file so it can live in a
//! long-lived structure independent of any file borrow – is provided generically
//! by [`OwnedPipeline`] on top of the same impl. It is the only place where
//! self-referential `unsafe` is used.
//!
//! ## On the `'file` lifetime
//!
//! For backends whose reads borrow from the file (mmap, disk cache) `'file`
//! genuinely bounds the returned slice. For backends whose reads produce *owned*
//! buffers (`io_uring`, object store) `'file` never bounds returned data — it is
//! a pure *file-safety guard* ensuring the file (and any fd or runtime referencing it)
//! outlives all in-flight operations.

use std::borrow::Cow;
use std::mem::ManuallyDrop;
use std::ops::{Deref as _, DerefMut, Range};

use super::{Item, UniversalRead};
use crate::ext::aligned_vec::ACow;
use crate::generic_consts::{AccessPattern, Sequential};
use crate::universal_io::{Result, UserData};

/// File-borrowing read pipeline.
///
/// Reads are scheduled against an externally-held `&'file File`. Results from [`wait`]
/// are bound by `'file`, i.e. they may outlive the pipeline itself, but not the file.
///
/// See module for the meaning of `'file` across backends, and [`OwnedPipeline`]
/// for the file-owning variant.
///
/// [`wait`]: Self::wait
pub trait ReadPipeline<'file, U>: Sized
where
    U: UserData,
{
    type File: 'file;

    fn new() -> Result<Self>;

    fn can_schedule(&mut self) -> bool;

    /// Schedule a read operation.
    ///
    /// An implementation might add it to an internal queue, but not actually execute it
    /// until [`wait`] is called.
    ///
    /// Should be called only when [`can_schedule`] is `true`.
    /// Returns [`UniversalIoError::QueueIsFull`] otherwise.
    ///
    /// [`wait`]: Self::wait
    /// [`can_schedule`]: Self::can_schedule
    /// [`UniversalIoError::QueueIsFull`]: crate::universal_io::UniversalIoError::QueueIsFull
    fn schedule<P: AccessPattern>(
        &mut self,
        user_data: U,
        file: &'file Self::File,
        range: Range<u64>,
        align: usize,
    ) -> Result<()>;

    /// Block until any scheduled operation completes and consume its result.
    fn wait(&mut self) -> Result<Option<(U, ACow<'file>)>>;

    #[inline]
    fn wait_bytemuck<T: Item>(&mut self) -> Result<Option<(U, Cow<'file, [T]>)>> {
        let Some((user_data, bytes)) = self.wait()? else {
            return Ok(None);
        };

        let items = bytes
            .try_cast_bytemuck()
            .expect("data has compatible layout");

        Ok(Some((user_data, items)))
    }
}

/// File-owning adapter over a [`ReadPipeline`].
///
/// Owns the file so the pipeline can be held in long-lived structures with no file borrow.
///
/// `OwnedPipeline` implementation reborrows file with `'static` lifetime, which is sound,
/// because [`wait`] returns `ACow<'_>` bound to `&mut self`, which guarantees
/// that `OwnedPipeline` (and file that it owns) can't outlive returned `ACow`s.
///
/// [`wait`]: Self::wait
pub struct OwnedPipeline<F, U>
where
    F: UniversalRead + 'static,
{
    pipeline: ManuallyDrop<F::ReadPipeline<'static, U>>,
    file: ManuallyDrop<F>,
}

impl<F, U> OwnedPipeline<F, U>
where
    F: UniversalRead + 'static,
    U: UserData,
{
    pub fn new(file: F) -> Result<Self> {
        let pipeline = F::ReadPipeline::new()?;

        let pipeline = Self {
            pipeline: ManuallyDrop::new(pipeline),
            file: ManuallyDrop::new(file),
        };

        Ok(pipeline)
    }

    #[inline]
    pub fn can_schedule(&mut self) -> bool {
        self.pipeline.can_schedule()
    }

    pub fn schedule<AP: AccessPattern>(
        &mut self,
        user_data: U,
        range: Range<u64>,
        align: usize,
    ) -> Result<()> {
        // SAFETY:
        //
        // `wait` returns `ACow<'_>`, which borrows `&mut self`, and so `OwnedPipeline` could only
        // be dropped after all returned `ACow`s are dropped (or converted into owned `AVec`).
        //
        // And our explicit `Drop` implementation guarantees that `file` is always dropped after
        // `pipeline`, and so `pipeline` will complete (or cancel) any pending operations that
        // might rely on `file`.

        let file = unsafe { (self.file.deref() as *const F).as_ref_unchecked() };
        self.pipeline.schedule::<AP>(user_data, file, range, align)
    }

    /// Like [`schedule`](Self::schedule), but reads the entire file (byte-aligned).
    pub fn schedule_whole(&mut self, user_data: U, from: u64) -> Result<()> {
        let length = self.file.len::<u8>()?;
        self.schedule::<Sequential>(user_data, from..length, 1)
    }

    #[inline]
    pub fn wait(&mut self) -> Result<Option<(U, ACow<'_>)>> {
        // SAFETY:
        //
        // `pipeline` returns `ACow<'static>`, but we shorten its lifetime to `ACow<'_>`, which
        // borrows `&mut self`, and so `OwnedPipeline` could only be dropped after all returned
        // `ACow`s are dropped

        self.pipeline.wait()
    }

    #[inline]
    pub fn wait_bytemuck<T: Item>(&mut self) -> Result<Option<(U, Cow<'_, [T]>)>> {
        let Some((user_data, bytes)) = self.wait()? else {
            return Ok(None);
        };

        let items = bytes
            .try_cast_bytemuck()
            .expect("data has compatible layout");

        Ok(Some((user_data, items)))
    }

    pub fn into_inner(self) -> F {
        let mut this = ManuallyDrop::new(self);

        let Self { pipeline, file } = this.deref_mut();

        unsafe {
            ManuallyDrop::drop(pipeline);
            ManuallyDrop::take(file)
        }
    }
}

impl<F, U> Drop for OwnedPipeline<F, U>
where
    F: UniversalRead + 'static,
    U: UserData,
{
    fn drop(&mut self) {
        // SAFETY:
        //
        // Drop `pipeline` before `file`, so that `pipeline` can complete (or cancel) any pending
        // operations that might rely on `file`

        let Self { pipeline, file } = self;

        unsafe {
            ManuallyDrop::drop(pipeline);
            ManuallyDrop::drop(file);
        }
    }
}
