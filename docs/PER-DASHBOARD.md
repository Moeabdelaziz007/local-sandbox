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
- CLI, TUI, and web UI

### In v0

- chat/block/design-system state
- VM state mapping and product lifecycle
- package-manager policy that is v0-specific
- auth/session/project semantics
- preview UI semantics
- adapter from v0 provider interface to this substrate

## PER Arc

### PER 0 — Repository foundation

Status: in progress

Prepare:
- Create Rust workspace with crate boundaries matching the substrate/UI split.
- Establish CI, formatting, clippy, tests, and dependency policy.
- Create GitHub repository and branch protection.

Execute:
- Scaffold workspace and CI.
- Push initial commit to `vercel-labs/local-sandbox`.
- Configure branch protection/rules.

Review:
- CI must pass on the initial PR/main setup.
- Branch protection must require CI before merge.
- Crate boundaries should make v0 adapter work possible without product leakage.

### PER 1 — Contract and adapter seam

Status: planned

Prepare:
- Extract the minimal sandbox contract from current v0 `VfkitProvider` and `@internal/sandbox` usage.
- Define stable Rust API types and daemon routes.

Execute:
- Implement typed contract in `local-sandbox-core`.
- Add JSON/HTTP representation in `local-sandbox-daemon`.
- Add mock/in-memory provider tests.

Review:
- v0 adapter should be expressible as a thin client without chat/block concepts in this repo.

### PER 2 — vfkit lifecycle substrate

Status: planned

Prepare:
- Model create/start/stop/status/cleanup and failure modes.
- Define readiness semantics independent of v0 UI.

Execute:
- Implement vfkit lifecycle boundary.
- Add macOS-gated integration tests where feasible.

Review:
- Deterministic lifecycle tests cover boot failure, cleanup, and status transitions.

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
- Identify the smallest v0 PR that replaces in-repo local substrate internals with client calls.

Execute:
- Add generated/typed client or CLI bridge in v0.
- Keep v0 provider interface stable.

Review:
- v0 CI failures should classify as adapter/product integration rather than substrate lifecycle uncertainty.

## Decision Log

- 2026-05-20: CLI, TUI, and web UI belong in the Rust repo because they operate on substrate facts and improve diagnosability outside v0.
- 2026-05-20: v0 product concepts stay out of the Rust repo; integration happens through a product-neutral sandbox contract.
