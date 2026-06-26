# qdrant — Viewport deploy overlay

Deploy overlay for **qdrant** (vector database) on the Viewport infrastructure.
This is the `viewport/deploy` branch of the `viewport-corp/fork-qdrant` fork; it adds
**only** a deploy overlay — upstream source is untouched.

- Sub-issue: viewport-ops **#413** (Part of **#404**)
- Department project: **Data AI and Research**
- Upstream: https://github.com/qdrant/qdrant
- Process: LOCKED #363 fork → clone → upstream → overlay → PR

## Image + version

- Image: `qdrant/qdrant` (official, Docker Hub)
- Pinned tag: **`v1.18.2`** — latest stable, published 2026-06-04 (never `:latest` for a persistent DB)
- Base: `debian:13-slim`, amd64 + arm64 (VPS is amd64)
- A non-root hardened variant `v1.18.2-unprivileged` exists (uid/gid 1000:1000 — would
  require the data volume chowned to 1000:1000). Default image runs as root. Decision deferred to Sam.

## Ports

| Port | Purpose | Exposed |
|------|---------|---------|
| 6333 | HTTP REST API + dashboard + health/metrics | internal only |
| 6334 | gRPC API | internal only |
| 6335 | P2P consensus (cluster mode only) | NOT used — single node |

No Dokploy Domain, no `:80`/`:443`, no DNS. Internal + health verification only (per guardrails).

## Persistence

- `/qdrant/storage` — vectors, collections, WAL. **MUST** be a local block/POSIX volume.
  Qdrant does **not** support NFS or S3-backed filesystems for storage.
- `/qdrant/snapshots` — snapshots / backups.
- Both are Docker **named volumes** (not bind mounts) so Dokploy Volume Backups work and
  they survive redeploys.

## Configuration

Configured purely via env vars (double-underscore → YAML path), no mounted config file:

| Variable | Purpose |
|----------|---------|
| `QDRANT__SERVICE__API_KEY` | Admin API key. **Required** — the OSS image ships auth OFF and binds `0.0.0.0`. Value supplied as a Dokploy secret; name only in git. |
| `QDRANT__TELEMETRY_DISABLED` | `true` — opt out of anonymized telemetry. |

Optional (not set here): `QDRANT__SERVICE__READ_ONLY_API_KEY`, `QDRANT__SERVICE__JWT_RBAC=true`.

No external dependencies — qdrant is a standalone single binary (no DB/cache/broker), needs no egress.

## Deploy (Dokploy, new engine)

- Dokploy: `http://194.163.153.171:3001/` (engine socket `/var/run/docker-viewport.sock`)
- Project: **Data AI and Research**
- Recommended path: **Create Service → Compose**, using `deploy/docker-compose.yml`.
  Set the API key value in the Environment tab as `QDRANT_API_KEY` (a Dokploy secret).
- Do **not** add a Dokploy Domain (that wires Traefik/`:443`, which is gated/out of scope).

## Health verification (internal only)

These endpoints respond without auth even when an API key is set:

- `GET http://<container>:6333/livez`  → 200
- `GET http://<container>:6333/readyz` → 200
- `GET http://<container>:6333/healthz` → 200
- `GET http://<container>:6333/` → JSON welcome (title + version)
- `GET http://<container>:6333/metrics` → Prometheus metrics

Note: the qdrant binary has **no** `--healthcheck` CLI flag and the image ships no
curl/wget, so there is no compose-level healthcheck — health is checked via the HTTP
endpoints above from the internal network / `docker exec`.

## Sources (docs-first)

- GitMCP on `qdrant/qdrant`: default config, `Dockerfile`, `src/main.rs` (CLI flags)
- Official qdrant.tech Installation (Networking) + Security docs
- Official Dokploy docs (Compose service, named volumes / Volume Backups)
- Live VPS verification of the new-engine Dokploy + latest release tag
