# PER 3D.KV-3b Node Package Compatibility

Date: 2026-05-22

## Boundary

KV-3b proves real Node package compatibility against the daemon-owned key-value service introduced in KV-3a. It is not v0 integration and does not move product key names, schema, rate-limit policy, auth/session behavior, or local boot selection into `local-sandbox`.

The daemon profile currently exposes Redis RESP endpoints only. It does not advertise an Upstash REST endpoint. Because the v0-observed packages are REST-shaped, this PER adds a test-local Upstash REST bridge whose only job is to translate package HTTP command payloads to the profile-derived Redis endpoint.

## Packages Tested

| Package              |  Version | Transport used in test                                          | Classification                        |
| -------------------- | -------: | --------------------------------------------------------------- | ------------------------------------- |
| `@upstash/redis`     | `1.34.0` | Harness-only Upstash REST bridge -> profile `redis://` endpoint | supported for tested subset           |
| `@vercel/kv`         |  `3.0.0` | Harness-only Upstash REST bridge -> profile `redis://` endpoint | supported for tested subset           |
| `@upstash/lock`      |  `0.2.1` | `@upstash/redis` over bridge -> profile `redis://` endpoint     | supported                             |
| `@upstash/ratelimit` |  `2.0.3` | `@upstash/redis` over bridge -> profile `redis://` endpoint     | unsupported under `lua_scripts=false` |

## Transport Finding

`GET /profile` for `services.key_value` advertises host and guest Redis endpoints with `protocol=redis` and `purpose=key_value`. The package clients tested here expect Upstash REST URL/token configuration, not a raw Redis TCP URL.

The compatibility test asserts that no HTTP/HTTPS key-value endpoint is advertised, then starts a narrow bridge inside the test process. The bridge is explicitly harness-only; it is not a product or daemon API. This keeps the result honest: current profile facts are enough to drive Redis-compatible clients and a local REST adapter, but they are not yet a profile-advertised Upstash REST surface.

## Coverage

`@upstash/redis@1.34.0` coverage:

- `ping`
- strings: `set`, `get`, `mget`, `setex`, `ttl`, `set` with `nx`/`ex`
- hashes: `hset`, `hget`, `hgetall`
- lists: `rpush`, `ltrim`, `lrange`
- sets: `sadd`, `sismember`, `srem`
- sorted sets: `zadd`, `zremrangebyrank`, `zrange`
- scan/type: `scan`, `type`
- pipelines mixing string, list, and read commands

`@vercel/kv@3.0.0` coverage:

- `createClient({ url, token })`
- `set`/`get`
- pipeline with `set`, `get`, `zadd`, and `zremrangebyrank`
- `scanIterator` over the tested key prefix

`@upstash/lock@0.2.1` coverage:

- acquire with `SET NX PX`
- competing acquire fails while the lock is held
- extend through its targeted Lua-compatible script path
- release through its targeted Lua-compatible script path
- second owner can acquire and release after release

`@upstash/ratelimit@2.0.3` coverage:

- fixed-window smoke attempts to execute the package's generic Lua script path.
- The test asserts an explicit unsupported error because `services.key_value.key_value.capabilities.lua_scripts=false`.

## Interpretation

KV-3a's targeted lock-script support is sufficient for the real `@upstash/lock@0.2.1` package path tested here. That was the highest-risk capability prediction from prepare.

`@upstash/ratelimit@2.0.3` is not supported by the current advertised capability profile because it requires generic Lua (`scriptLoad`/`evalsha`) for fixed-window limiting. This PER does not implement general Lua and should not claim ratelimit compatibility.

The remaining transport question is product/daemon API design: if v0 wants package clients to consume local-sandbox profile facts directly without a v0-side adapter, a real profile-advertised Upstash REST-compatible endpoint is a separate implementation task.

## Validation

- `pnpm install`
- `pnpm test`
- `pnpm typecheck`

The full requested Rust validation suite was also run in the KV-3b execute pass after this document was added.
