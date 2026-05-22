# RFC: Self-Contained Local Development Infrastructure

Status: draft  
Date: 2026-05-22

## Summary

`local-sandbox` should be a self-contained local development infrastructure server.

The daemon should provide the local infrastructure surfaces v0 needs by default: sandbox runtime, object storage, database, route/proxy, TLS, readiness, profile, and diagnostics. Developers should not have to install and coordinate separate local services before the daemon can provide those surfaces.

External binaries can still be useful, but they should be explicit implementation strategies, optional parity backends, or unavoidable provider dependencies. They should not be the default contract when an embedded implementation can satisfy the compatibility surface.

## Motivation

The initial extraction focused on moving local substrate mechanics out of v0. That was necessary, but it is not sufficient. If `local-sandbox` only orchestrates external tools, developers still inherit local setup drift:

- missing binaries on `PATH`
- unpinned local service versions
- per-machine configuration differences
- process lifecycle races
- CI gaps because real services are unavailable
- ambiguous ownership when readiness or compatibility fails

The stronger product promise is:

> A fresh checkout can run one local infrastructure daemon and get the local services needed for v0 development.

This does not mean `local-sandbox` owns product semantics. v0 still owns chat, block, auth, project, bucket/key naming, schema, and policy. `local-sandbox` owns local infrastructure capability and the product-neutral compatibility surfaces those product semantics rely on.

## Relationship to interface compatibility

This RFC builds on `docs/rfcs/interface-compatibility.md`.

Interface compatibility says: implement the subset of production-shaped interfaces v0 already expects.

Self-contained infrastructure adds: implement those local interfaces with embedded/default local backends when viable, instead of requiring users to install separate services.

The compatibility target remains v0's actual expectations, not full upstream product completeness.

## Design principles

### 1. Embedded by default

Prefer embedded implementations for default local services.

Examples:

- S3-compatible object storage should default to an embedded Rust S3 backend, not an externally installed MinIO binary.
- Postgres should default to an embedded or bundled Postgres-compatible backend if it satisfies v0's database expectations.
- HTTP, websocket, callback, and object public endpoint routing should be daemon-owned proxy infrastructure.
- Local TLS generation should be daemon-owned, following the pure-Rust pattern proven in `locald`.

### 2. External binaries are not the product boundary

External binaries may still be useful:

- `vfkit` can remain a provider dependency for macOS VM lifecycle.
- MinIO can remain a parity/reference backend while embedded object storage becomes default.
- Native Postgres can remain a fallback if embedded PGlite semantics diverge.

But the profile and client contract should name product-neutral infrastructure surfaces, not the initial backend implementation.

### 3. Latest stable Rust is acceptable

This project can track latest stable Rust when doing so unlocks materially better embedded local infrastructure.

The current Rust version should not block adopting an embedded backend like `pglite-oxide` if it is the right architecture. The version bump should still be explicit and validated, but we should not treat an older MSRV as a core compatibility requirement for this repository.

### 4. Profiles describe capabilities, not brands

The local profile should advertise product-neutral facts:

- service kind
- endpoints and visibility
- credentials
- health
- logs
- capability matrix

It should not force v0 to know whether object storage is backed by MinIO, `s3s-fs`, or another backend.

### 5. Contract tests define compatibility truth

Compatibility should be proven by client-visible behavior:

- AWS SDK against the object-storage profile
- Postgres clients against the database profile
- HTTP/websocket/browser probes against route/profile facts
- v0 integration diffs that delete local substrate code

Backend identity is secondary.

### 6. Do not own user workload tools

`local-sandbox` may help set up or diagnose workload tools, but it should not own them.

Out of scope as local infrastructure defaults:

- Node
- pnpm/npm/bun
- git
- app build tools
- language package managers

Those belong to the sandbox workload. `local-sandbox` should make them observable and diagnosable, not embed them.

## Surface inventory

### Object storage

Current state:

- PR 3C.2 made the daemon own MinIO lifecycle.
- PR 3C.3 adds an AWS SDK contract test using daemon profile facts, but the real MinIO path is ignored and env-gated.
- MinIO was a fast implementation strategy, not the desired long-term boundary.

Target:

- Default backend: embedded Rust S3-compatible server.
- Likely implementation: `s3s` plus `s3s-fs`.
- Normal CI should run the AWS SDK profile contract against the embedded backend.
- MinIO can remain an optional ignored parity backend.

Open questions:

- Does `s3s-fs` satisfy the exact advertised subset, including presigned GET/PUT through AWS SDK generated URLs?
- What backend seam should sit below `ObjectStorageManager`?
- Which MinIO-specific names should be renamed before they harden into the product boundary?

### Postgres

Current v0 state:

- v0 local development is already using PGlite through `pglite-server`.
- `dev:local` starts `pglite-server --db chat/.pglite --port 5433 -m 10`.
- v0 injects `PG_LOCAL_URL=postgres://localhost:5433/postgres` into local chat/frame environments.
- Drizzle and `pg` connect through the normal Postgres wire URL.
- There is no pgBouncer today; concurrency is handled by `pglite-server` plus app-side `pg.Pool`s.

Candidate default:

- `pglite-oxide` is the best architectural fit if its current release satisfies v0's Postgres expectations.
- It exposes a normal local Postgres URL through `PgliteServer` and supports standard clients like SQLx and `tokio-postgres`.
- Latest `pglite-oxide` currently requires a newer stable Rust than this repo uses. That is acceptable if the backend proves out.

Fallback:

- `postgresql_embedded` can manage bundled/downloaded native Postgres.
- This improves over “user installed Postgres,” but still leaves the daemon managing a native process and binary artifacts.
- It is a fallback for fidelity, not the preferred default if PGlite works.

Open questions:

- Does v0 rely on Postgres features not supported by PGlite/PGlite proxy?
- What connection concurrency does v0 need under realistic local load?
- Do we need a bouncer/proxy layer inside `local-sandbox`, or is `PgliteServer` sufficient?
- Which bootstrap steps are product-neutral infrastructure vs v0 schema/product policy?

### KV / Redis-shaped state

Current v0 state:

- Production v0 uses Upstash Redis clients.
- Local mode swaps exported Redis clients to a `PgLocalKV` instance.
- `PgLocalKV` is not a Redis wire server. It is a TypeScript compatibility shim backed by local Postgres/PGlite.
- It implements a meaningful Redis-like subset: strings, hashes, lists, sets, counters, scan, pipeline subset, sorted-set-ish counters, and the Lua subset needed by `@upstash/lock`.
- Current evidence did not find live `rpush` or `ltrim` usage, but v0 does use list semantics such as `lpush`, `llen`, `lrange`, `lrem`, and `rpop`.

Interpretation:

- The surface is bespoke at the API boundary, but the semantics are still Redis/Upstash-shaped.
- That means an embedded Redis-compatible service may be valuable if it lets v0 use a standard Redis client path and delete local `PgLocalKV` behavior.
- However, we should not assume Redis wire compatibility is the right substrate boundary until recon proves the v0 diff improves.

Options:

1. Keep KV in v0 and only provide embedded Postgres.
2. Move a product-neutral Redis-subset compatibility helper into `local-sandbox`, backed by embedded Postgres/PGlite.
3. Provide an embedded Redis-compatible server or RESP endpoint for the subset v0 uses.
4. Use an embedded key-value engine only if v0 can consume a product-neutral local KV API without increasing bespoke local-mode code.

Open questions:

- Would exposing a Redis/RESP endpoint reduce v0 local-mode branching more than a Postgres-backed helper?
- Are Upstash-specific methods and pipeline semantics better modeled at the TypeScript client layer or at a local server boundary?
- Which KV keys/schemas are product-owned and must remain in v0?
- Can we prove atomic queue/lock semantics better with an embedded Redis-compatible implementation than with `PgLocalKV`?

### Routes and proxying

Current v0 state:

- Local vfkit routes are mostly `http://localhost:<hostPort>` proxies from host ports to guest ports.
- v0/frame code has local shortcuts such as `ldev<port>` and `localhost:<port>` target rewriting.
- Browser iframe origins use local wildcard-style hostnames like `vm-${token}.localhost:3001` in dev.
- Production and preview paths use `vusercontent.net` and branch-specific frame proxying.

Target:

- The daemon owns local route registry, HTTP proxying, websocket proxying, callback endpoints, and endpoint visibility facts.
- v0 asks profile/client helpers for route facts rather than constructing local production-shaped URLs in scattered code.
- Route/profile contracts distinguish host-visible, guest-visible, browser-visible, websocket, callback, and public object-storage endpoints.

Open questions:

- Which route URL shapes must remain product-owned?
- Which helpers are enough to delete local URL rewrites in v0?
- How should the route proxy expose guest-visible services vs browser-visible public routes?

### HTTPS / local trust

Prior art:

- `locald` already solved much of this in Rust.
- It uses `rcgen` for local Root CA and leaf certificate generation.
- It uses `rustls::server::ResolvesServerCert` to generate/cache leaf certificates on SNI.
- It uses system trust installation with platform-specific handling and helper/shim support.
- It treats trust installation as an explicit admin/setup workflow, not a hidden side effect.

Target:

- Borrow or adapt the `locald` architecture where local HTTPS is needed.
- Generate certificates in Rust.
- Terminate TLS in the daemon proxy.
- Make trust setup explicit and diagnosable.
- Avoid requiring `mkcert`, `openssl`, or ad hoc user setup as default runtime dependencies.

Open questions:

- Can `local-sandbox` reuse code or patterns from `locald` directly?
- Is HTTPS required for v0 local browser flows, or is HTTP localhost enough for the first embedded route/proxy milestone?
- What is the right trust UX for this repo: optional, required, or profile-advertised capability?

### VM provider

Current state:

- `vfkit` is an external binary dependency for macOS VM lifecycle.

Decision:

- Keep `vfkit` as an acceptable provider dependency for now.
- Improve acquisition, diagnostics, and profile reporting before considering a direct Virtualization.framework provider.
- Treat direct framework integration as a later provider reliability PER, not part of the service-embedding push.

Rationale:

- The service layer has clearer immediate reliability wins.
- Direct VM-provider implementation is larger and platform-specific.
- Helping download/install/check `vfkit` may satisfy the self-contained developer experience for now.

## Proposed PER updates

### PER 3C.4: Embedded object storage

Default object storage should move from MinIO process management to embedded `s3s`/`s3s-fs` or equivalent.

Success:

- daemon starts object storage without external binaries
- profile-derived AWS SDK contract passes in normal CI
- MinIO is optional parity/reference only

### PER 3D: Embedded Postgres and KV decision

This should be reframed from “Postgres profile / KV decision” to “embedded Postgres and KV compatibility decision.”

Prepare should prove:

- current v0 PGlite usage and concurrency needs
- whether `pglite-oxide` satisfies those needs
- whether the repo should bump to latest stable Rust to adopt it
- whether KV remains v0-owned, becomes a `local-sandbox` Redis-subset helper, or becomes a Redis-compatible local endpoint

Success:

- daemon can expose a Postgres profile backed by an embedded/default implementation
- v0 can centralize `PG_LOCAL_URL` selection around the local profile
- KV ownership is decided with evidence from actual v0 usage

### PER 3E: Embedded route/proxy and HTTPS strategy

Routes and local HTTPS should be treated as daemon-owned local infrastructure.

Prepare should compare current v0 local URL construction with the `locald` proxy/TLS model.

Success:

- local route/proxy behavior has explicit profile facts
- browser/host/guest endpoint classes are separated
- HTTPS generation/trust plan is explicit, even if trust installation remains optional at first

### PER 3F: v0 integration probe

Only after embedded S3 and the Postgres/KV decision have enough shape, run the deletion-pressure v0 spike.

Success:

- v0 deletes local service startup/env wiring
- local profile/client selection is centralized
- product semantics stay in v0

## Decision criteria

Embed by default when:

- the interface is part of local infrastructure, not user workload behavior
- a Rust implementation can satisfy the bounded compatibility surface
- normal CI can prove the contract without external installation
- developer setup gets simpler
- diagnostics and readiness get clearer

Use bundled or managed binaries when:

- the embedded Rust implementation cannot satisfy required semantics
- upstream fidelity matters more than install simplicity
- the binary can be acquired, pinned, diagnosed, and managed by the daemon/CLI

Leave external or workload-owned when:

- the tool belongs to the user's application runtime
- embedding would make `local-sandbox` own product policy
- compatibility is too broad or accidental to encode product-neutrally

## Risks

- Embedded backends can diverge from production semantics.
- Raising Rust stable may affect contributors or CI images.
- Capability matrices can drift if tests are not profile-derived.
- KV can accidentally absorb product semantics if we move too much from v0.
- TLS trust installation can become surprising if it is not explicit.

## Current recommendation

Proceed in this order:

1. Finish PR 8 as S3 profile contract coverage and planning documentation.
2. PER 3C.4: make object storage embedded by default with `s3s`/`s3s-fs`.
3. PER 3D: evaluate `pglite-oxide` on latest stable Rust and decide KV compatibility ownership with actual v0 usage in hand.
4. PER 3E: move route/proxy/TLS planning toward the daemon, borrowing from `locald` where appropriate.
5. PER 3F: run the v0 deletion-pressure integration spike.

The main shift is from “daemon-owned lifecycle for external tools” to “daemon-owned local infrastructure surfaces, embedded when viable.”
