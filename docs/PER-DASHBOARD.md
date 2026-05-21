# local-sandbox PER Dashboard

This dashboard tracks the extraction of v0 local sandbox substrate into `vercel-labs/local-sandbox`.

## North Star

Build a robust Rust substrate that owns local VM/service/runtime mechanics while v0 keeps product-specific chat/block/project integration.

## Boundary

### In this repository

- vfkit VM lifecycle
- local S3-compatible storage
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

### PER 3 — local services substrate

Status: planned

Prepare:

- Define local S3/Postgres/service dependency model and health checks.

Execute:

- Implement service orchestration in `local-sandbox-services`.
- Add health/readiness tests.

Review:

- Services can be started/stopped independently and surfaced through daemon status.

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
