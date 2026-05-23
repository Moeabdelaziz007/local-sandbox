# PER 3D.KV-3c v0 Integration Shape Proof

Date: 2026-05-22

## Boundary

KV-3c is a v0 integration shape proof, not a daemon implementation. It asks whether v0 can consume current `local-sandbox` key-value profile facts through a small local-mode adapter/bridge before choosing between a v0-side adapter and a daemon-advertised Upstash REST endpoint.

`local-sandbox` still owns daemon lifecycle, readiness, Redis endpoint profile facts, and the embedded Redis-compatible service. v0 keeps product key names, schemas, rate-limit policy, and local-mode adapter selection.

## v0 Target Selection

Target selected: `/Users/wycats/Code/v0-lima-rfc`.

Reason:

- `/Users/wycats/Code/v0` is active main-style code with direct REST env clients in `web/data/kv/index.ts` and no `dev:local` local-sandbox path.
- `/Users/wycats/Code/v0-lima-rfc` is the branch that contains `dev:local`, local sandbox commands, `VM_PROVIDER=vfkit` local-mode selection, and `chat/lib/kv.ts` swapping chat clients to `PgLocalKV` under local mode.
- That branch is the only inspected checkout with a centralized local KV adapter seam and the product-local rate-limit adapter already keeping `@upstash/ratelimit` generic Lua out of the local path.

The proof did not modify v0 and does not prove a full v0 local boot. It used the target branch shape to choose the commands and transports to exercise from `local-sandbox`.

## Proof Harness

Added `docs/spikes/kv-3c-v0-shape-proof.mjs`.

The harness:

1. Builds and starts `local-sandbox-daemon` with temporary object-storage and key-value data directories.
2. Reads `GET /profile` through `LocalSandboxClient`.
3. Extracts the `services.key_value` host Redis endpoint.
4. Starts a narrow test-local Upstash REST bridge backed by that Redis endpoint.
5. Exercises v0-shaped REST env clients through `@upstash/redis` and `@vercel/kv`.
6. Exercises raw Redis pub/sub directly through the profile Redis URL.
7. Prints a pass/classification table.

The bridge is deliberately harness-only. It is not a daemon endpoint and is not product integration code.

## Command and Transport Results

| Area | Classification | Evidence |
| ---- | -------------- | -------- |
| Profile Redis endpoint | pass | `services.key_value` advertised `host_redis` with `protocol=redis`, `visibility=host`, `purpose=key_value`, and implementation `tinyredis`. |
| V0-shaped REST env transport | pass through local bridge | The proof maps `V0CHAT_KV_REST_API_URL`, `V0CHAT_KV_RATELIMIT_REST_API_URL`, `V0_EPHEMERAL_KV_REST_API_URL`, and `KV_REST_API_URL` to the local bridge URL/token shape. |
| Raw Redis env transport | pass for direct clients | The proof maps a raw local value such as `LOCAL_SANDBOX_REDIS_URL=redis://127.0.0.1:<port>` from the profile endpoint. |
| Core chat/web commands | pass | Strings, `mget`, `setex`, `ttl`, `expire`, `incr`, `del`, hashes, lists including `rpush`/`ltrim`, sets, sorted sets, `scan`, and `type` pass through the bridge. |
| Pipelines | pass | Ordered `@vercel/kv` pipeline exec covers anonymous-message-limit style `incr`/`expire NX`, hash writes, reads, and perf-trace list trim for the tested commands. |
| Locks | pass | Real `@upstash/lock` acquire, contention, extend, release, and reacquire pass through targeted lock-script support. |
| `@upstash/ratelimit` | unsupported | Fixed-window still uses generic Lua script loading/evalsha and fails under `lua_scripts=false`. v0-lima-rfc's product-owned local rate-limit adapter remains the right local path. |
| Raw Redis pub/sub | unsupported | `SUBSCRIBE` fails with `ERR protocol error: unknown command 'SUBSCRIBE'` against current `tinyredis`. |

## Interpretation

The current `local-sandbox` KV profile is enough for a v0-side adapter/bridge proof. v0 can derive host Redis from `/profile`, start or select a product-local bridge for Upstash REST-shaped clients, and keep product choices in v0.

A daemon-owned Upstash REST endpoint is not required to unblock the next integration experiment. It may become useful later if multiple products need the same REST compatibility surface, but KV-3c found no evidence that local-sandbox must advertise that endpoint now.

Raw Redis pub/sub is a blocker if v0 local integration requires Redis pub/sub over the profile endpoint. The inspected v0-lima-rfc active KV usage did not show app-level raw Redis pub/sub calls for the local-mode target, so this is a classification to carry forward, not a reason to stop the bridge proof.

## Recommendation

Proceed with a v0-side local adapter/bridge for the next v0 integration spike.

That keeps:

- Product env names and client selection in v0.
- Product rate-limit policy in v0.
- `local-sandbox` profile unchanged for now.
- The daemon surface honest: Redis RESP endpoint with `lua_scripts=false` and `targeted_lock_scripts=true`.

Loop only if the next v0 integration spike discovers broad scattered product changes, required raw Redis pub/sub, or a second product that needs the same Upstash REST compatibility surface from `local-sandbox`.

## Validation

Run `pnpm build` before invoking the spike directly; the harness imports the built `LocalSandboxClient` from `packages/local-sandbox-client/dist`.

Baseline before changes:

- `cargo test -p local-sandbox-daemon --features kv-contract --test kv_contract -- --nocapture` — passed.
- `LOCAL_SANDBOX_RUN_KV_PACKAGE_COMPAT=1 node --test packages/local-sandbox-client/test/kv-package-compat.test.mjs` — passed.

KV-3c harness:

- `node docs/spikes/kv-3c-v0-shape-proof.mjs` — passed and printed classifications; raw Redis pub/sub classified unsupported.

Workspace validation after changes:

- `node --check docs/spikes/kv-3c-v0-shape-proof.mjs` — passed.
- `pnpm test` — passed with 9 passing tests and 1 skipped env-gated KV package test.
- `pnpm typecheck` — passed.
