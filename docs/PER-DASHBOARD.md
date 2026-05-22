# local-sandbox PER Dashboard

This dashboard tracks the extraction of v0 local sandbox substrate into `vercel-labs/local-sandbox`.

## North Star

Build a self-contained Rust local-development infrastructure server that owns local VM/service/runtime mechanics while v0 keeps product-specific chat/block/project integration.

`local-sandbox` should not require developers to assemble local infrastructure from external services before it can provide the compatibility surfaces v0 needs. The daemon should own lifecycle, readiness, profile facts, diagnostics, and default local implementations. External binaries are acceptable only as explicit parity/reference backends or when embedding would distort the product-neutral contract.

## Boundary

### In this repository

- vfkit VM lifecycle
- embedded S3-compatible storage
- local Postgres orchestration
- local process/service health
- sandbox file IO and command execution
- route/port readiness and proxy contract
- logs, diagnostics, and snapshot materialization
- sandbox-compatible client façade over the daemon
- CLI, TUI, and web UI

### In v0

- chat/block/design-system state
- VM state mapping and product lifecycle
- package-manager policy that is v0-specific
- auth/session/project semantics
- preview UI semantics
- product wiring that selects the local sandbox-compatible client

## PER Arc

### PER 0 — Repository foundation

Status: complete

Prepare:

- Create Rust workspace with crate boundaries matching the substrate/UI split.
- Establish CI, formatting, clippy, tests, and dependency policy.
- Create GitHub repository and branch protection.

Execute:

- Scaffold workspace and CI.
- Push initial commits to `vercel-labs/local-sandbox`.
- Configure branch protection/rules.

Review:

- CI passes on main.
- Branch protection requires CI before merge.
- Crate boundaries make v0 adapter work possible without product leakage.

### PER 1 — Contract and adapter seam

Status: complete

Prepare:

- Extract the minimal sandbox contract from current v0 `VfkitProvider` and `@internal/sandbox` usage.
- Define stable Rust API types and daemon routes.

Execute:

- Implemented typed contract in `local-sandbox-core`.
- Added provider trait and in-memory provider for adapter and daemon tests.
- Added JSON/HTTP routes in `local-sandbox-daemon` using injectable provider state.
- Added core serde, provider lifecycle, command, file, timeout, snapshot, and daemon error tests.

Review:

- v0 adapter should be expressible as a thin client without chat/block concepts in this repo.
- Review requested adapter-surface fields for create-time routes, snapshot restore, runtime, resources, userland spawning, and detached commands; these are represented in the core contract.

### PER 2 — vfkit lifecycle substrate

Status: complete in PR #2

Prepare:

- Model create/start/stop/status/cleanup and failure modes.
- Define readiness semantics independent of v0 UI.
- Keep Linux portability explicit: future Linux support should be a separate provider behind `SandboxProvider`, not conditionals in core or daemon code.

Execute:

- Added `VfkitConfig` plus a `VfkitProvider` implementation of the core `SandboxProvider` contract.
- Added fakeable disk, process, agent, route, state, and time seams inside `local-sandbox-vfkit`.
- Implemented deterministic create/get/delete/timeout lifecycle behavior with cleanup on failed create.
- Routed command, file, and snapshot operations through an in-memory guest-agent seam for PER 2 rather than wiring a real guest agent yet.
- Added a macOS-gated ignored vfkit integration-test placeholder.

Review:

- Deterministic lifecycle tests cover simulated process-start failure, cleanup, and status transitions.
- Confirm no v0 product concepts leaked into the vfkit crate.
- Confirm Linux provider portability remains a separate provider implementation, not a vfkit branch hidden in core.

### PER 2.5 — guest-agent protocol seam

Status: complete in PR #2

Prepare:

- Port the existing Rust guest-agent wire protocol from v0 before wiring real vfkit boot.
- Keep deterministic provider tests working while introducing the real protocol boundary.
- Treat snapshot creation as a two-step substrate concern: guest `Sync` first, host disk clone later.

Execute:

- Added `local-sandbox-protocol` with length-prefixed JSON frames and request/response types for commands, command polling, command kill, file writes, route `Connect`, readiness `Ping`, and filesystem `Sync`.
- Added a protocol-backed guest-agent client in `local-sandbox-vfkit` that can register a Unix socket, wait for `Ping`, send command/file/sync requests, and map responses back into the core contract.
- Kept the default vfkit path compatible with PER 2 by routing no-socket processes to the in-memory agent and socket-backed processes to the protocol client.

Review:

- Protocol tests prove frame shape, base64 file payloads, route/connect messages, kill, ping, sync, omitted default fields, and frame-size rejection.
- Workspace tests pass without a real vfkit boot image.
- Next runtime PER should replace `NoopProcessSpawner` with real vfkit process start that supplies the guest-agent socket path.

### PER 2.6 — sandbox-compatible client façade

Status: complete in PR #2

Prepare:

- Define the subset of `@vercel/sandbox` that v0 actually uses: create/get, routes, commands, file writes, timeout extension, network policy failure behavior, stop, and snapshot.
- Keep the Rust core contract product-neutral; compatibility belongs at the client/daemon edge, not in `SandboxProvider`.
- Decide which production-only fields are pass-through, translated, or explicitly unsupported in local mode.

Execute:

- Added `@vercel-labs/local-sandbox-client`, a TypeScript client package that exposes `Sandbox`/`Command`-shaped objects over the local daemon API.
- Mapped `Sandbox.create({ ports, timeout, runtime, resources, source, __userlandSpawning })` onto the daemon create route and map `source: { type: "snapshot" }` onto `snapshot_id`.
- Mapped `runCommand({ cmd, args, cwd, env, detached })`, `getCommand()`, `writeFiles()`, `extendTimeout()`, `stop()`, `updateNetworkPolicy()`, and `snapshot()` onto the substrate routes.
- Implemented `Command.logs()`, `Command.output()`, `Command.wait()`, and `Command.kill()` in terms of command polling until real streaming exists.
- Added façade contract tests against mocked daemon responses so v0 can depend on the compatibility shape before real vfkit boot lands.
- Added TypeScript workspace config and CI.

Review:

- v0 migration should reduce to selecting the local sandbox-compatible client rather than maintaining a large custom adapter.
- The façade should not introduce chat/block/project concepts or reshape the Rust core around production SDK history.
- Unsupported production features should fail with explicit, stable local errors.

### PER 3A — compatibility recon

Status: complete

Prepare:

- Test the interface-compatibility RFC against current v0 local substrate code.
- Inventory S3, Postgres/KV, and route/URL expectations before implementing service substrate.
- Use deletion of superseded v0 local code as a success signal, but do not move product semantics into this repo.

Execute:

- Confirmed S3 is a profile plus bounded operations subset: MinIO lifecycle, endpoint facts, credentials, bucket bootstrap, and health belong in `local-sandbox`; product bucket/key semantics stay in v0.
- Confirmed Postgres lifecycle/profile belongs in `local-sandbox`, while chat DB schema, migrations, `kv_store` key schemas, and the Redis-shaped `PgLocalKV` shim stay in v0 until proven product-neutral.
- Identified routes/URLs as the highest-risk compatibility surface: host-visible, guest-visible, browser-visible, websocket, callback, and S3 public endpoints need explicit taxonomy.
- Found local compatibility gaps to carry forward: guest presigned MinIO reachability is unproven, local log console URL composition appears suspect, and `PgLocalKV` lacks `rpush`/`ltrim` used by perf tracing.

Review:

- Interface compatibility is the right service-boundary strategy, but profile design must start with endpoint visibility classes rather than a single URL per service.
- S3 and Postgres should proceed profile-first; KV movement is deferred.
- Routes and URL compatibility deserve a dedicated PER before relying on S3 or v0 integration results.

### PER 3B — local profile contract

Status: implemented

Prepare:

- Define a product-neutral local profile shape for service endpoints, credentials, route facts, and health.
- Distinguish host-visible, guest-visible, browser-visible, websocket, callback, and public object-storage endpoints.

Execute:

- Added product-neutral profile contract types for daemon, sandbox, route, service, endpoint, health, and credentials.
- Added daemon `GET /profile` that returns fake/unknown service endpoints and health until S3/Postgres lifecycle lands.
- Added `LocalSandboxClient.getProfile()` plus exported TypeScript profile types.
- Added Rust serialization and daemon route tests plus a mocked TypeScript client profile test.

Review:

- v0 should be able to consume one local profile instead of scattered local service env constants.

### PER 3C — S3 service compatibility

Status: in progress — 3C.4 embedded object-storage backend implemented

Prepare:

- Define the S3-compatible operation subset v0 actually uses.
- Decide whether `PutBucketCors` and `ListObjectVersions` are supported locally or explicitly unsupported.
- Prove guest-visible object-storage endpoint behavior before relying on presigned URLs from guest VMs.
- Reframe `MinIO` as the initial backend, not the product boundary: `local-sandbox` should provide object storage by default without external installation.

Execute:

- 3C.1 adds product-neutral MinIO object-storage profile facts: host and guest API endpoints, credentials, region, path-style addressing, logs, and a bounded operation support matrix.
- 3C.1 adds fakeable MinIO binary/process/health seams and deterministic lifecycle tests that do not require a real MinIO binary.
- 3C.2 moves MinIO lifecycle ownership into the daemon: boot starts object storage, `GET /profile` reports not started/starting/ready/failed/stopped state, and graceful daemon shutdown stops the running service handle.
- 3C.2 adds product-neutral object-storage env config for binary, data dir, API host/port, console port, region, credentials, guest host, and log path.
- 3C.2 keeps default tests on fakeable seams and adds an ignored real-MinIO host lifecycle contract test.
- 3C.3 adds an ignored/env-gated real-MinIO daemon integration test that reads `GET /profile`, builds an AWS SDK S3 client from the daemon-owned object-storage endpoint, credentials, region, and path-style profile facts, then validates create bucket, put/get exact bytes, head, list, copy, delete object(s), presigned GET, and presigned PUT.
- 3C.4 replaces MinIO as the default object-storage runtime with an embedded Rust S3-compatible backend using `s3s` plus `s3s-fs`, so normal local development and CI do not require a `MinIO` binary.
- 3C.4 keeps the AWS SDK profile-derived contract as the compatibility truth: daemon starts object storage, `GET /profile` returns endpoint/credential/region/path-style facts, and a profile-derived SDK client validates the advertised host operation subset.
- 3C.4 demotes MinIO to an optional parity/reference backend selected with `LOCAL_SANDBOX_OBJECT_STORAGE_BACKEND=minio` or `LOCAL_SANDBOX_MINIO_BINARY`.
- Browser/CORS support, versioning support, guest presigned probing, v0-specific buckets/keys, and Postgres/KV remain out of scope for 3C.4.

Review:

- v0 should be able to delete MinIO lifecycle and local S3 env wiring while keeping product bucket/key semantics.
- If daemon boot cannot start object storage, the daemon still serves a failed object-storage health profile rather than failing the whole daemon.
- Host-side S3 compatibility now has normal embedded contract coverage through daemon profile facts; the ignored real-MinIO contract remains available for parity checks in environments with a `MinIO` binary.
- Guest-visible and browser-visible object-storage flows still need dedicated route/URL compatibility work.
- The success bar is now higher than MinIO lifecycle ownership: a fresh checkout should be able to run the daemon and get working local object storage without installing an external S3 service.

### PER 3C.4 — embedded object-storage backend

Status: implemented

Prepare:

- Evaluate `s3s`/`s3s-fs` as the default embedded S3-compatible backend for the bounded object-storage capability matrix.
- Identify whether the existing profile-derived AWS SDK host contract passes against the embedded backend, including presigned GET/PUT.
- Define a backend abstraction that keeps profile facts product-neutral and avoids naming the service after MinIO.
- Decide whether MinIO remains as an optional parity/reference backend or is removed from default daemon boot entirely.

Execute:

- Introduced an object-storage backend selection seam below `ObjectStorageManager` while keeping profile facts product-neutral.
- Implemented the default embedded backend as a daemon-owned Tokio task backed by `s3s`/`s3s-fs`.
- Added `LOCAL_SANDBOX_OBJECT_STORAGE_BACKEND=embedded|minio`; `LOCAL_SANDBOX_MINIO_BINARY` still selects the MinIO parity backend.
- Converted the S3 host contract to run in normal CI against the embedded backend.
- Kept the MinIO contract ignored/env-gated as an optional parity/reference check.

Review:

- A fresh checkout can run `local-sandbox-daemon` and expose object storage without installing MinIO.
- The profile-derived AWS SDK contract passes in normal CI for the advertised host S3 subset.
- The capability matrix stays honest about unsupported CORS, browser upload, versioning, guest presigned, and public object-storage flows.
- v0 deletion work should target local object-storage lifecycle/env wiring, not MinIO-specific code paths.
- MinIO can likely be removed once embedded object storage is accepted as the only compatibility target, but keeping it through this PER preserves a reference backend while the embedded backend is still new.

### PER 3D — embedded Postgres / KV decision

Status: planned

Prepare:

- Define PGlite/Postgres lifecycle and profile requirements.
- Prove current v0 PGlite usage, connection concurrency, and bootstrap expectations.
- Evaluate `pglite-oxide` on latest stable Rust as the default embedded Postgres-compatible backend.
- Inventory `PgLocalKV` method coverage and Redis/Upstash semantics before deciding whether KV compatibility belongs here.
- Decide whether KV remains v0-owned, moves as a product-neutral Redis-subset helper, or is exposed as a Redis-compatible local endpoint.

Execute:

- Bump the workspace to latest stable Rust if the chosen embedded backend requires it.
- Implement Postgres lifecycle/profile/readiness support with an embedded/default backend.
- Add profile-derived Postgres contract coverage using normal Postgres clients.
- Move KV only if recon proves a product-neutral compatibility boundary and the v0 diff improves.

Review:

- v0 should consume a Postgres profile while retaining chat DB schema, migrations, and product KV policy.
- A fresh checkout should not require a separately installed Postgres service for local development.

### PER 3E — embedded routes / proxy / HTTPS compatibility

Status: planned

Prepare:

- Trace sandbox route URL, preview iframe, log proxy websocket, lifecycle callback, S3 public endpoint, and guest endpoint flows.
- Decide which URL facts belong in local profile and which remain product routing/auth semantics.
- Compare current local route/proxy behavior with `locald`'s Rust proxy/TLS architecture: `rcgen`, `rustls`, SNI-based leaf certificates, and explicit trust setup.

Execute:

- Extend route/profile contracts with the endpoint facts proven necessary.
- Add compatibility tests for route URL helpers and local `subdomain` bridge behavior.
- Implement daemon-owned HTTP/websocket/callback proxying where v0 currently carries local URL rewrites.
- Add a TLS generation/trust plan before relying on browser HTTPS flows.

Review:

- v0 should stop composing production URL shapes from local `subdomain` values except through explicit compatibility helpers.
- HTTPS support should not require `mkcert`, `openssl`, or hidden trust-store mutation.

### PER 3F — v0 integration probe

Status: planned

Prepare:

- Identify the smallest v0 branch that selects `local-sandbox` profile/client surfaces and deletes superseded local substrate code.

Execute:

- Build a narrow v0 spike using local-sandbox interfaces for sandbox, profile, and service endpoints.

Review:

- Proceed only if the v0 diff is centralized and deletes more local substrate code than it adds.

### PER 4 — operator surfaces

Status: planned

Prepare:

- Define CLI/TUI/web workflows for diagnosing sandbox state.

Execute:

- CLI commands for create/status/logs/destroy.
- TUI for live sandbox/service state.
- Web UI for logs, routes, and health.

Review:

- Operator surfaces expose substrate facts without v0-specific concepts.

### PER 5 — v0 adapter migration

Status: planned

Prepare:

- Identify the smallest v0 PR that replaces in-repo local substrate internals with the sandbox-compatible local client.

Execute:

- Select the daemon-backed sandbox-compatible client in local mode.
- Keep v0 product lifecycle, warm-pool policy, chat/block state, and preview semantics stable.

Review:

- v0 CI failures should classify as product integration or substrate runtime issues rather than adapter-shape uncertainty.

## Decision Log

- 2026-05-20: CLI, TUI, and web UI belong in the Rust repo because they operate on substrate facts and improve diagnosability outside v0.
- 2026-05-20: v0 product concepts stay out of the Rust repo; integration happens through a product-neutral sandbox contract.
- 2026-05-20: PER 1 uses an in-memory provider as the daemon default so the HTTP contract and adapter seam can validate before vfkit lifecycle work exists.
- 2026-05-20: Network policy is part of the product-neutral contract, but the PER 1 in-memory provider returns a typed unsupported error until a real runtime provider can enforce it.
- 2026-05-20: PER 2 keeps vfkit assumptions inside `local-sandbox-vfkit`; future Linux support should be another provider behind the same `SandboxProvider` contract, not conditionals in core or daemon code.
- 2026-05-20: PER 2 command, file, and snapshot methods use an in-memory guest-agent seam. This validates the provider contract and cleanup behavior on Linux CI before a real guest-agent protocol exists.
- 2026-05-20: PER 2.5 ports the guest-agent protocol before real boot. The default agent keeps PER 2's in-memory path for no-socket processes and uses the protocol client when a future real process spawner supplies a Unix socket.
- 2026-05-20: Add a sandbox-compatible TypeScript client façade over the daemon. The Rust core stays substrate-shaped; the façade absorbs `@vercel/sandbox` compatibility so v0 does not grow a large custom adapter.
- 2026-05-21: PER 3A affirmed interface compatibility as the service-boundary strategy: `local-sandbox` owns local lifecycle/profile/readiness facts while v0 owns product bucket/key/KV schemas and routing/auth policy.
- 2026-05-21: Local profile contracts must distinguish host-visible, guest-visible, browser-visible, websocket, callback, and public object-storage endpoints. A single URL per service is not enough.
- 2026-05-21: `PgLocalKV` does not move into `local-sandbox` until a product-neutral KV contract is proven; Postgres lifecycle/profile moves first.
- 2026-05-22: Sharpened the north star from extracted substrate to self-contained local-development infrastructure server. Default local services should be embedded when viable; external binaries such as MinIO are implementation strategies or parity backends, not the product boundary.
- 2026-05-22: Latest stable Rust is acceptable for this project when it unlocks better embedded local infrastructure; MSRV should not block adopting a backend such as `pglite-oxide` if it proves to fit v0's Postgres expectations.
