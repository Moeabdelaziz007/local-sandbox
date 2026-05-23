# local-sandbox tinyredis patch

This directory vendors `tinyredis 1.0.0` for the PER 3D.KV-3a key-value service prototype.

## Why it is vendored

`tinyredis` was the best embedded Redis-compatible candidate in the KV-2 bakeoff, but the unmodified crate missed several v0-relevant Redis/Upstash semantics. The daemon prototype depends on this top-level vendor path so production code does not depend on a docs spike directory.

## Patch scope

Local patches cover the known KV-2 gaps:

- `SETEX`
- `SET EXAT` / `SET PXAT`
- `EXPIRE ... NX`
- `ZREMRANGEBYRANK`
- `SCRIPT LOAD`, `EVAL`, and `EVALSHA` for targeted lock scripts
- `sha1_smol` dependency for Redis-style script SHA1s

Script support is intentionally narrow. It supports constant-return scripts used by the bakeoff and the two `@upstash/lock` shapes used in v0 package code:

- compare-and-delete release script using `redis.call("get", KEYS[1])` and `redis.call("del", KEYS[1])`
- compare-and-extend script using `redis.call("get", KEYS[1])`, `redis.call("ttl", KEYS[1])`, and `redis.call("expire", KEYS[1], ...)`

This is not a Lua runtime and must not be advertised as general Lua compatibility.

## Validation

The service/profile contract is covered by:

- `cargo test -p local-sandbox-daemon --features kv-contract --test kv_contract -- --nocapture`

The original bakeoff/patchability matrix is under `docs/spikes/per-3d-kv-bakeoff` and points at this vendored copy.
