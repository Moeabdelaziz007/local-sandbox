# PER 3D.KV-2 Tinyredis Patchability Spike

Date: 2026-05-22

## Boundary

This is a patchability spike for `tinyredis 1.0.0`, not the final daemon implementation. The goal is to test whether the concrete gaps found in the KV-2 bakeoff are localized command/semantics gaps that we can plausibly patch.

The spike originally patched `tinyredis` under the bakeoff directory; KV-3a moved the patched copy to `vendor/tinyredis` so production code can depend on a non-docs path while keeping the patch visible. The isolated bakeoff crate now points at that same vendored copy.

## Patch Scope

The patch added targeted support for the gaps found in the initial matrix. The bakeoff harness is the validation artifact; the vendored tinyredis copy does not include separate unit tests for these patched commands:

- `SETEX` alias implemented via existing `SET ... EX` path.
- `SET EXAT` / `SET PXAT` parsing added to existing `SET` command.
- `EXPIRE ... NX` semantics added so an already-expiring key is not updated.
- `ZREMRANGEBYRANK` added using existing sorted-set index structures.
- Targeted script compatibility added:
  - `SCRIPT LOAD` returns Redis-style SHA1 and caches script bodies for the running process.
  - `EVALSHA` supports loaded scripts for the targeted lock shapes.
  - `EVAL` and `EVALSHA` support the compare-and-delete release script and compare-and-extend script used by `@upstash/lock` 0.2.1.
  - Constant-return scripts remain supported for the bakeoff harness.

This is intentionally not full Lua support. It is enough to test whether the local service can satisfy the known lock-script path without carrying a full scripting engine in the first prototype.

## Results

| Candidate           | Before | After |
| ------------------- | ------ | ----- |
| `tinyredis 1.0.0`   | 38/48  | 48/48 |
| `feox-server 0.7.0` | 18/48  | 18/48 |

Full patched matrix output is in `docs/per/3d-kv-2-tinyredis-patch-results.tsv`.

## Interpretation

The `tinyredis` gaps are patchable in a bounded spike. The failures were not lifecycle or embedding blockers. They were concentrated in command routing, expiry option semantics, one sorted-set command, and script compatibility strategy.

This does not mean the patch is production-ready. The next implementation pass should decide whether to:

1. vendor/fork `tinyredis`,
2. upstream small command fixes, or
3. reimplement a minimal Redis-compatible service using `tinyredis` as reference.

The biggest remaining design choice is script support. A targeted `@upstash/lock` compatibility path is much smaller than general Lua, but it must be documented as a local compatibility contract if chosen.

## Validation

- `cargo +1.95.0 check --manifest-path docs/spikes/per-3d-kv-bakeoff/Cargo.toml`
- `cargo +1.95.0 run --manifest-path docs/spikes/per-3d-kv-bakeoff/Cargo.toml`
- After KV-3a moved the patched backend to `vendor/tinyredis`, the full bakeoff matrix was rerun with `cargo +1.95.0 run --manifest-path docs/spikes/per-3d-kv-bakeoff/Cargo.toml`.
