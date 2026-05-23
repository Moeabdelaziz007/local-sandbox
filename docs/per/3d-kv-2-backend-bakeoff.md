# PER 3D.KV-2 Backend Bakeoff

Date: 2026-05-22

## Boundary

This bakeoff compares Redis-compatible local service candidates against the PER 3D.KV-1 command matrix. It does not choose the final daemon implementation and does not move product key names, rate-limit policy, auth/session semantics, or cleanup rules into `local-sandbox`.

The tested shape is a daemon-owned Redis-compatible endpoint that standard Redis clients can use. The contract is intentionally service-level: commands, expiry behavior, atomicity, pipelines, scripts, lifecycle, and shutdown.

## Candidates Tested

| Candidate           | Mode                                                  | Why tested                                                                                                                          |
| ------------------- | ----------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `tinyredis 1.0.0`   | Embedded library API via `tinyredis::server::serve`   | Best first candidate from the inspected crates.io set: MIT, broad command list, RESP2, async Tokio server, optional persistence.    |
| `feox-server 0.7.0` | External binary built from crate source for the spike | Second candidate: Apache-2.0, Redis-compatible server with persistence and transactions, but heavier and less obviously embeddable. |

Candidates considered but not tested in this spike:

- `mini-redis`: intentionally incomplete example server; only supports a tiny command set.
- `radixox`: interesting Redis-compatible store, but Linux/io_uring/monoio shape is a poor first fit for macOS local dev.
- `hero_redis`: feature-rich persistent server, but more auth/encryption/vector oriented than this first compatibility bakeoff needs.

## Results Summary

| Candidate           | Passed | Failed | Total | Read                                                                                              |
| ------------------- | -----: | -----: | ----: | ------------------------------------------------------------------------------------------------- |
| `tinyredis 1.0.0`   |     38 |     10 |    48 | Promising baseline for a patch/prototype track, with several v0-relevant gaps still to prove out. |
| `feox-server 0.7.0` |     18 |     30 |    48 | Deprioritized for the first implementation track pending a FeOx-specific investigation.           |

Full row output is in `docs/per/3d-kv-2-backend-bakeoff-results.tsv`.

## tinyredis Findings

`tinyredis` passed most core service semantics in the matrix:

- readiness: `PING`
- strings: `SET`, `GET`, `MGET`, `DEL`, `EXISTS`, `SET NX EX`, `SET NX PX`, `EXPIREAT`, `TYPE`
- hashes: `HSET`, `HMSET`, `HGET`, `HMGET`, `HGETALL`, `HDEL`
- lists: `LPUSH`, `RPUSH`, `LLEN`, `LRANGE`, `RPOP`, `LREM`, `LTRIM`, concurrent `RPOP` uniqueness
- sets: `SADD`, `SMEMBERS`, `SISMEMBER`, `SREM`
- sorted sets: `ZADD`, `ZSCORE`, `ZINCRBY`, `ZCARD`, `ZRANGE`, `ZREM`
- scan/type: `SCAN MATCH COUNT`
- pipeline basics: `INCR + EXPIRE NX`
- transactions: `MULTI/EXEC`
- lifecycle: clean embedded shutdown

Failures that matter for v0 compatibility:

| Family         | Command                          | Why it matters                                                                                                    |
| -------------- | -------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| strings/expiry | `SETEX`                          | Active surface uses explicit `setex`; README claims support, but crate returned unknown command.                  |
| strings/expiry | `TTL -2/-1/N`                    | Failed because the test uses `SETEX`; `ttl` itself still needs isolated proof after fixing `SETEX`.               |
| strings/expiry | `EXPIRE NX`                      | Returned `(1, 1)` instead of `(1, 0)`, so NX option semantics are wrong for rate-limit style usage.               |
| strings/expiry | `SET EXAT`                       | Returned syntax error; web `@vercel/kv` usage includes absolute expiry forms.                                     |
| sorted sets    | `ZREMRANGEBYRANK`                | Missing; web usage-code cleanup uses rank trimming.                                                               |
| pipelines      | mixed expiry/hash/zset pipelines | Failures are driven by missing `SET EXAT` and `ZREMRANGEBYRANK`; pipeline support itself works for simple cases.  |
| scripts        | `EVAL`, `SCRIPT LOAD`, `EVALSHA` | Missing; active app code does not call these directly, but `@upstash/lock` compatibility needs explicit handling. |

Interpretation: `tinyredis` is close enough to justify a patch/prototype track. It is not drop-in yet, but the currently observed gaps are concentrated in command implementation/semantics rather than lifecycle or embedding.

## feox-server Findings

`feox-server` passed basic connectivity and some core commands (`PING`, `SET`, `GET`, `MGET`, `DEL`, `EXISTS`, some hashes, basic lists, concurrent `RPOP`, `SET EXAT`, `MULTI/EXEC`). But many commands returned unexpected EOF, and shutdown via `SIGTERM` surfaced as a failure in the spike. Because the spike had to build/run it as an external binary and many command failures closed connections, it is not a good first daemon-embedded candidate.

This does not prove `feox-server` is unusable. It does mean KV-2 should not start there unless `tinyredis` patching fails or a later FeOx-specific investigation explains the EOF behavior. The shutdown failure is partly a harness interpretation: the spike sends SIGTERM and currently treats signal exit as a failed shutdown.

## Recommendation

Use `tinyredis` as the first patch/prototype candidate for a daemon-owned Redis-compatible local KV service. The patch size is not proven yet, especially for script/lock compatibility, so KV-3 should first prove the missing commands and semantics can be added cleanly.

Next concrete work:

1. Fork/patch or locally vendor a tinyredis candidate in a spike. **Done in the follow-up patchability pass; the patched copy now lives at `vendor/tinyredis`.**
2. Add/fix `SETEX`, `EXPIRE NX`, `SET EXAT`, `ZREMRANGEBYRANK`, and script/lock compatibility strategy. **Done for the known KV-2 matrix; targeted script compatibility is not general Lua.**
3. Re-run the matrix and add direct `@upstash/redis`/`@vercel/kv` client checks where possible. **Matrix rerun passed 48/48 for patched tinyredis; package-level Node validation remains KV-3b work.**
4. If the patched matrix is green enough, proceed to PER 3D.KV-3 daemon service design. **KV-3a implemented the daemon service prototype.**
5. If patching is invasive, evaluate a minimal RESP service backed by an embedded engine instead of adopting a third-party server wholesale.

## Validation

- `cargo +1.95.0 run --manifest-path docs/spikes/per-3d-kv-bakeoff/Cargo.toml`
- `cargo +1.95.0 check --manifest-path docs/spikes/per-3d-kv-bakeoff/Cargo.toml --target aarch64-apple-darwin`
- After KV-3a moved the patched backend to `vendor/tinyredis`, the full bakeoff matrix was rerun with `cargo +1.95.0 run --manifest-path docs/spikes/per-3d-kv-bakeoff/Cargo.toml`.
