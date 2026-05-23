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
- Confirmed Postgres lifecycle/profile belongs in `local-sandbox`, while chat DB schema, migrations, `kv_store` key schemas, and KV product policy stay in v0. The Redis-shaped `PgLocalKV` shim is evidence about the local service surface, not the boundary to preserve.
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

### PER 3D — embedded Postgres / KV service ownership

Status: 3D.0 reframed; 3D.1 spike complete; Postgres and KV tracks split

North star:

- `local-sandbox` owns the local service surfaces v0 needs to boot in dev mode.
- v0 owns product schema, migrations, key names, and product policy that use those services.
- PER 3D is an umbrella. It decides and implements daemon-owned local Postgres and KV-compatible services through smaller proof points, not one broad implementation pass.

Prepare:

- Define PGlite/Postgres lifecycle and profile requirements.
- Prove current v0 PGlite usage, connection concurrency, and bootstrap expectations.
- Evaluate `pglite-oxide` on latest stable Rust as the default embedded Postgres-compatible backend.
- Inventory live Redis/Upstash method usage before choosing a KV implementation shape.
- Compare daemon-owned Redis-compatible endpoint, Redis-subset helper, and Postgres-backed compatibility helper options.

Execute:

- 3D.0 reframed the boundary from code ownership to service ownership: if v0 dev mode needs a local service to boot, `local-sandbox` should provide that service surface.
- 3D.0 split the remaining work into Postgres and KV tracks so we can push through concrete proof points.
- 3D.1 bumped the workspace to Rust 1.95 stable, removing the old 1.88 constraint. `pglite-oxide 0.5.0` only requires Rust 1.92+, so MSRV is no longer a blocker.
- 3D.1 added an isolated executable spike at `docs/spikes/per-3d-pglite-oxide` rather than adding `pglite-oxide` to the main workspace. In the daemon graph, `pglite-oxide 0.5.0` conflicts with the existing embedded S3 dependency line because `s3s 0.13.0-alpha.3`/`0.13.0` pins `sha2 =0.11.0-rc.5` while `wasmer-types` requires stable `sha2 ^0.11.0`.
- 3D.1 profile-derived Postgres spike findings: a v0-shaped no-user URL timed out; an explicit `postgres://postgres@127.0.0.1:<port>/postgres` URL connected with `tokio-postgres`, created a table, inserted exact rows, selected exact rows, and shut down cleanly for a single standard client.
- 3D.1 pool findings: `deadpool-postgres` with `max_size=2` failed to open the first pooled client within 2 seconds; `max_size=1` queued the overlapping second query until the first client closed. Shutdown after pool scenarios was not consistently clean; runs observed `read frontend socket: Connection reset by peer`.
- 3D.1 stopped before implementing a daemon manager because the spike exposed unresolved integration questions, not because embedded Postgres is out of scope. `local-sandbox` should still own the local Postgres service; the next step is to choose the implementation path deliberately.
- KV should be treated as a local infrastructure service that `local-sandbox` ultimately owns if v0 needs it to boot locally. The PER 3D.1 evidence only says not to port the current incomplete `PgLocalKV` shim verbatim; follow-up should evaluate a complete Redis/Upstash-compatible surface, including `rpush`/`ltrim`, and compare a daemon-owned Redis-compatible endpoint against a Postgres-backed helper.

Review:

- v0 should consume service profile facts while retaining chat DB schema, migrations, key names, and product KV policy.
- A fresh checkout should not require separately installed Postgres or Redis-compatible services for local development once the relevant PER 3D tracks land.
- Follow-up should either prove `PgliteServer` behind daemon-owned profile constraints, add a daemon-side multiplexing/bouncer layer in front of `PgliteServer`, or switch the PER 3D default candidate to `postgresql_embedded` for fidelity.
- KV follow-up should start from the service boundary v0 needs, not from code ownership: if v0 dev mode needs local Redis/Upstash semantics, `local-sandbox` should provide them through a daemon-owned service or compatibility endpoint.

### PER 3D.0 — service-boundary reframe

Status: implemented

Prepare:

- Correct the PER 3D boundary after the initial pglite/KV spike.
- Preserve the useful spike evidence without over-reading it as a decision to keep Postgres or KV out of `local-sandbox`.
- Make the remaining sequence explicit so the project can keep moving.

Execute:

- Reframed PER 3D around local service ownership: `local-sandbox` owns the local service surfaces; v0 owns product semantics.
- Promoted PER 3D to an umbrella with Postgres and KV subtracks.
- Recorded that `pglite-oxide` is blocked by integration questions, not by project scope.
- Recorded that KV remains in scope as daemon-owned local infrastructure, while the current `PgLocalKV` shim should not be ported verbatim.

Review:

- The docs should no longer say or imply that KV is “v0-owned” just because the current implementation lives in v0.
- The next work should be executable proof points: Postgres dependency/service shape and KV surface/backend shape.

### PER 3D.PG-1 — Postgres dependency unblock

Status: planned

Prepare:

- Verify the `pglite-oxide`/`s3s` Cargo conflict precisely.
- Identify whether the conflict can be solved by upgrading, patching, forking, feature selection, or isolating a backend boundary.
- Keep `postgresql_embedded` as the fidelity fallback candidate.

Execute:

- Build minimal dependency graphs for `s3s`/`s3s-fs` plus `pglite-oxide`.
- Test available `s3s` versions and feature combinations.
- Try a small local patch only if the dependency change is mechanical and low-risk.
- Document whether direct adoption, a carried patch, process isolation, or fallback is the next viable route.

Review:

- We should know whether the dependency conflict is a short packaging task, an upstream/fork task, or a reason to switch backend candidates.

### PER 3D.PG-2 — Postgres service-shape proof

Status: planned

Prepare:

- Define the daemon-owned Postgres profile contract: service key, host/guest database endpoints, explicit username/password/database, backend, data dir, and connection guidance.
- Decide what v0 should consume from the profile instead of hardcoding `PG_LOCAL_URL`.
- Define realistic client behavior to test, including `pg.Pool`-equivalent concurrency.

Execute:

- Build a harness around the proposed profile URL, not the inherited no-user URL shape.
- Test standard clients, explicit pool constraints, overlapping queries, and shutdown with clients open/closed.
- If needed, spike a daemon-side single-flight/bouncer layer in front of `PgliteServer`.
- Run the same harness against `postgresql_embedded` if `PgliteServer` cannot satisfy the service contract.

Review:

- Decide whether `pglite-oxide` is viable under a service contract v0 can reasonably consume.
- If not, choose the fallback before implementing the manager.

### PER 3D.PG-3 — daemon-owned Postgres manager

Status: planned

Prepare:

- Map the object-storage manager pattern to Postgres lifecycle, readiness, profile facts, diagnostics, and shutdown.
- Decide which profile fields are generic database facts and which need a Postgres-specific profile struct.

Execute:

- Implement Postgres config, manager, service runner, readiness probe, and profile generation.
- Wire daemon boot and shutdown.
- Add profile-derived Postgres contract tests using a standard Postgres client.

Review:

- A fresh checkout can start `local-sandbox-daemon` and expose local Postgres without separately installed Postgres.
- v0 can consume profile facts instead of owning local Postgres startup/wiring.

### PER 3D.KV-1 — Redis/Upstash surface inventory

Status: execute complete; ready for review

Prepare:

- Treat KV as a local infrastructure service surface, not a TypeScript shim ownership question.
- Define the command inventory needed for v0 to boot locally.

Execute:

- Active v0 Redis/KV clients are split across `chat/lib/kv.ts` (`@upstash/redis` clients), `web/data/kv/index.ts` (`@vercel/kv` proxy with local patches), and `apps/marketing/lib/kv.ts` (marketing `@upstash/redis`).
- Active v0 command surface includes get/set/del, mget, setex, set NX with ex/px, hash methods, list methods, set methods, sorted-set methods, counters, exists, scan, ttl/expire, absolute expiry in web usage-code pipelines, and pipelines.
- Active v0 does not show direct application calls to `eval`, `evalsha`, `scriptLoad`, `multi`, `rpush`, or `ltrim` in the inspected scopes. Lock semantics are library-mediated through `@upstash/lock` in `chat/lib/lock.ts` and VM gateway code.
- Lima-rfc has `PgLocalKV` under `chat/lib/pg-local-kv.ts` and swaps chat Redis clients to it only when `VM_PROVIDER=vfkit`; active v0 does not contain that shim.
- Lima-rfc adds branch-only `rpush`/`ltrim` usage in `chat/app/chat/api/vm/perf-trace.ts`; this should not be claimed as active-main usage unless that code lands in the target branch.
- Current `PgLocalKV` covers a useful subset but misses or incompletely models active live surface including `ttl`, `type`, `hget`, `sismember`, `zrange`, `zcard`, `zrem`, `zremrangebyrank`, `expireat`, `exat`, broad pipeline command coverage, sorted-set ordering, and atomic list pop/claim behavior.
- Artifacts: `docs/per/3d-kv-1-redis-surface-inventory.md` and `docs/per/3d-kv-1-redis-call-sites.tsv`.

Review:

- Review should focus on the minimum local boot surface versus the full product surface: VM pool/state, locks, anonymous/web limits, frame/session tokens, credentials, screenshots, OAuth state, and bot/integration helpers are the highest-risk local flows.
- Review should decide whether KV-2 starts with a Redis-compatible endpoint contract, because v0 already consumes Redis/Upstash clients and PgLocalKV is not a complete compatibility target.
- Review should keep product key/schema/rate-limit policy in v0 while requiring `local-sandbox` to own lifecycle, readiness, endpoint/profile facts, diagnostics, expiry behavior, and atomic service semantics.

### PER 3D.KV-2 — KV backend bakeoff

Status: execute complete; ready for review

Prepare:

- Candidate shapes: embedded Redis-compatible/RESP server, daemon-owned Redis-subset service backed by Postgres/PGlite, or daemon-owned product-neutral KV API plus v0 adapter.
- Evaluation criteria: deletes v0 local-mode code, supports the live command surface, preserves atomic lock/queue behavior, is diagnosable, and avoids product-schema ownership.

Execute:

- Added an isolated bakeoff spike at `docs/spikes/per-3d-kv-bakeoff` and results at `docs/per/3d-kv-2-backend-bakeoff.md` plus `docs/per/3d-kv-2-backend-bakeoff-results.tsv`.
- Tested `tinyredis 1.0.0` as an embedded Tokio server using its library API. It passed 38/48 checks and clean shutdown.
- `tinyredis` passed the broad core surface: strings, hashes, lists including `rpush`/`ltrim`, sets, sorted sets, scan, simple pipelines, concurrent `rpop` uniqueness, and `MULTI/EXEC`.
- `tinyredis` failed bounded but important gaps: `SETEX`, `EXPIRE NX`, `SET EXAT`, `ZREMRANGEBYRANK`, mixed pipelines depending on those commands, and script commands (`EVAL`, `SCRIPT LOAD`, `EVALSHA`).
- Tested `feox-server 0.7.0` as an external binary built from crate source. It passed 18/48 checks, but many commands returned unexpected EOF. Shutdown surfaced as a signal failure under this harness, which sends SIGTERM and treats signal exit as failure.
- The bakeoff favors `tinyredis` as the first patch/prototype candidate if we are willing to investigate the missing Redis/Upstash semantics.

Review:

- Decide whether KV-3 should start with a patched `tinyredis` prototype or first evaluate another Redis-compatible crate.
- Review should focus on whether the missing `tinyredis` commands are bounded patch work and whether script/lock compatibility should be implemented as real Lua support, a targeted `@upstash/lock` compatibility path, or an explicit unsupported local behavior.
- Patchability spike vendored `tinyredis` locally and added targeted support for `SETEX`, `SET EXAT`, `EXPIRE NX`, `ZREMRANGEBYRANK`, `SCRIPT LOAD`, `EVALSHA`, compare-and-delete release scripts, and compare-and-extend lock scripts. The matrix improved from 38/48 to 48/48 for `tinyredis`; this validates known-gap patchability, not general Lua compatibility.

### PER 3D.KV-3a — daemon-owned KV service manager

Status: implemented; ready for review

Prepare:

- KV belongs beside object storage in `local-sandbox-services`, not in `SandboxProvider`.
- `local-sandbox` owns local service lifecycle, readiness, profile facts, and Redis-compatible endpoint shape; v0 keeps product schemas, key names, lock policy, rate limits, auth, and session semantics.
- Patched `tinyredis` is acceptable as the prototype backend, but the profile must not claim full Lua support. It should advertise targeted lock-script compatibility only.

Execute:

- Added `LocalKeyValueProfile` and `LocalKeyValueCapabilities` plus endpoint taxonomy values `purpose=key_value` and `protocol=redis`.
- Added `KeyValueConfig`, `KeyValueService`, and `KeyValueManager` in `local-sandbox-services`, patterned after object storage with fakeable runner/probe seams, TCP readiness, default host `127.0.0.1`, default port `16379`, guest host `host.internal`, optional password auth, and product-neutral env vars.
- Embedded the patched `tinyredis` vendor from `vendor/tinyredis` as the prototype backend. Persistence is not enabled in this KV-3a prototype.
- Wired daemon boot, profile aggregation, and shutdown to include `key_value` beside `object_storage`. S3-only contract helpers keep KV not-started so unrelated tests do not consume a Redis port.
- Added a feature-gated Rust `kv-contract` daemon test that reads `/profile`, builds a standard Rust `redis` client from the host Redis endpoint, and validates `PING`, strings/expiry (`SET`, `GET`, `SETEX`, `TTL`, `EXPIRE NX`, `SET EXAT`), hashes, lists (`RPUSH`, `LTRIM`, `RPOP`), sets, sorted sets (`ZREMRANGEBYRANK`), `SCAN`, pipeline, `MULTI`/`EXEC`, direct `EVAL` lock release, `SCRIPT LOAD` + `EVALSHA` lock release, and direct `EVAL` lock extension.
- Updated the TypeScript client profile types and mocked profile test for Redis endpoint/profile facts.

Review:

- Review should treat this as KV-3a local-sandbox service substrate only. v0 integration and real Node package/client validation remain KV-3b.
- The profile intentionally advertises `targeted_lock_scripts=true` and `lua_scripts=false`; this is not a general Lua runtime.
- Remaining open questions: whether persistence should be enabled/configurable and which KV diagnostics/log facts should be added before v0 consumes the surface.

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
- 2026-05-21: Do not port `PgLocalKV` into `local-sandbox` verbatim; first prove the product-neutral KV service surface. Postgres lifecycle/profile moves first, but KV remains in scope as local infrastructure if v0 needs Redis/Upstash semantics to boot.
- 2026-05-22: Sharpened the north star from extracted substrate to self-contained local-development infrastructure server. Default local services should be embedded when viable; external binaries such as MinIO are implementation strategies or parity backends, not the product boundary.
- 2026-05-22: Latest stable Rust is acceptable for this project when it unlocks better embedded local infrastructure; MSRV should not block adopting a backend such as `pglite-oxide` if it proves to fit v0's Postgres expectations.
- 2026-05-22: PER 3D.1 bumped the workspace to Rust 1.95 stable, but did not adopt `pglite-oxide 0.5.0` into the main workspace. The remaining blockers are the current S3 `sha2` prerelease dependency line, explicit username profile URLs, pooled-client failure/hangs beyond one connection, and inconsistent clean shutdown after pool scenarios. This is a call for narrower experiments, not a conclusion that Postgres or KV should stay out of `local-sandbox`.
