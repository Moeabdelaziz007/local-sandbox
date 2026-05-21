# local-sandbox

`local-sandbox` is a Rust substrate for running production-shaped local sandboxes for v0 development and tests.

The repository owns local infrastructure concerns:

- vfkit VM lifecycle
- local service orchestration, including S3-compatible storage and Postgres
- file IO and command execution inside sandboxes
- port routes and readiness
- logs and diagnostics
- snapshots and restore flows
- CLI, TUI, and web UI surfaces for operators

v0 should integrate through a thin client/adapter. Product concepts like chats, blocks, design systems, and Vercel projects stay in v0.

## Workspace

- `local-sandbox-core`: provider-neutral API types and traits
- `local-sandbox-vfkit`: vfkit provider implementation boundary
- `local-sandbox-services`: local S3/Postgres/service orchestration boundary
- `local-sandbox-daemon`: HTTP API daemon for adapters and tools
- `local-sandbox-cli`: command-line interface
- `local-sandbox-tui`: terminal UI
- `local-sandbox-web`: web UI/API helpers

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Contract

The substrate API should remain product-neutral:

- create/destroy sandbox
- status
- write/read files
- exec command
- start/stop local service
- route a port
- read logs
- create/restore snapshot
- health/readiness
