use std::ops::Range;

use super::pool::IO_URING_QUEUE_LENGTH;
use super::{IoUringFile, IoUringRuntime};
use crate::ext::aligned_vec::ACow;
use crate::generic_consts::AccessPattern;
use crate::universal_io::{ReadPipeline, Result, UniversalIoError, UserData};

pub struct IoUringPipeline<'file, U> {
    runtime: IoUringRuntime<'file, U>,
}

impl<'file, U> ReadPipeline<'file, U> for IoUringPipeline<'file, U>
where
    U: UserData,
{
    type File = IoUringFile;

    fn new() -> Result<Self> {
        let runtime = IoUringRuntime::new()?;
        Ok(Self { runtime })
    }

    fn can_schedule(&mut self) -> bool {
        let squeue = self.runtime.io_uring.submission();
        self.runtime.in_progress + squeue.len() < IO_URING_QUEUE_LENGTH as _
    }

    /// # Safety
    ///
    /// The caller must ensure that the `fd` will not outlive the pipeline.
    fn schedule<P: AccessPattern>(
        &mut self,
        user_data: U,
        file: &'file IoUringFile,
        range: Range<u64>,
        align: usize,
    ) -> Result<()> {
        // SAFETY:
        // `file` outlives the pipeline (`'file`), so `file.fd()` will not
        // outlive any in-flight operation scheduled here.

        let mut squeue = self.runtime.io_uring.submission();

        if self.runtime.in_progress + squeue.len() >= IO_URING_QUEUE_LENGTH as _ {
            return Err(UniversalIoError::QueueIsFull);
        }

        let entry = self
            .runtime
            .state
            .read(user_data, file.fd(), range, align, file.direct_io);

        unsafe {
            squeue.push(&entry).expect("submission queue is not full");
        }

        Ok(())
    }

    fn wait(&mut self) -> Result<Option<(U, ACow<'file>)>> {
        let next = self.runtime.completed().next();

        let enqueued = self.runtime.enqueued();

        if next.is_some() && enqueued > 0 {
            self.runtime.submit_and_wait(0)?;
        } else if next.is_none() && enqueued + self.runtime.in_progress > 0 {
            self.runtime.submit_and_wait(1)?;
        }

        let Some(result) = next.or_else(|| self.runtime.completed().next()) else {
            return Ok(None);
        };

        let (user_data, resp) = result?;
        Ok(Some((user_data, ACow::Owned(resp.expect_read()))))
    }
}
