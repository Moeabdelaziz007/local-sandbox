# RFC: Interface Compatibility as the Local Sandbox Boundary

Status: historical draft; superseded for service-ownership planning by `self-contained-infrastructure.md` and `PER-DASHBOARD.md`  
Date: 2026-05-21

## Summary

`local-sandbox` should integrate with v0 by providing local implementations of the interfaces v0 already expects to consume, rather than asking v0 to learn local runtime internals.

The goal is not full compatibility with every upstream product. The goal is compatibility with v0's actual expectations at the seams it already uses: sandbox runtime, S3-compatible storage, Postgres/KV support, route URLs, and local service discovery.

This keeps product semantics in v0 while moving local substrate mechanics into this repository.

Update, 2026-05-22: later PER 3D planning sharpened the boundary from code ownership to service ownership. `local-sandbox` owns the local service surfaces v0 needs to boot in dev mode; v0 owns product schema, migrations, key names, and product policy. The Postgres/KV details in this draft should be read as historical pre-reframe notes.

## Motivation

The first version of this repository started by extracting local vfkit mechanics from v0. That is useful, but extraction alone can still leave v0 with a large adapter layer that knows too much about local substrate behavior.

The sandbox-compatible client façade points at a cleaner strategy:

- v0 already has a production-shaped `@vercel/sandbox` interface.
- `local-sandbox` can expose a local client shaped like that interface.
- v0 can select the local client in local mode instead of carrying a bespoke vfkit adapter.

This RFC generalizes that design principle to the other local dependencies v0 emulates today.

## Design principle

For every local dependency, prefer this shape:

1. Identify the interface v0 already expects.
2. Implement the subset v0 actually uses in `local-sandbox`.
3. Keep compatibility at the client/service boundary.
4. Keep v0 product policy in v0.
5. Allow small v0 refactors when they clarify URL selection, client creation, auth, or configuration.

Do not mirror upstream products bug-for-bug. Do not move chat/block/project semantics into this repository just to shrink a one-time v0 diff.

## Compatibility target

The target is **v0 expectation compatibility**, not upstream feature completeness.

A local interface is compatible when:

- v0 can call it through a stable, production-shaped seam.
- unsupported production-only behavior fails explicitly and predictably.
- local service URLs, credentials, and routes are provided from one substrate-owned place.
- contract tests cover the behavior v0 depends on.
- the v0 integration diff is centralized and deletes local substrate code instead of spreading local-mode conditionals.

## Initial interface map

| Interface             | v0 expectation                                                                    | Local-sandbox target                                                                                           | Likely v0 movement                                                                        |
| --------------------- | --------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Sandbox runtime       | `@vercel/sandbox`-style create/get/routes/commands/files/timeout/network/snapshot | Sandbox-compatible TypeScript client over the daemon                                                           | Select local client in local mode                                                         |
| S3-compatible storage | S3 clients for git bundles, commit backups, and local object storage              | Local S3 service lifecycle plus endpoint/bucket/credential/profile helpers                                     | Centralize S3 client creation and URL selection                                           |
| Postgres / KV         | PGlite local service plus Redis-shaped KV behavior for v0 state                   | Daemon-owned Postgres lifecycle/profile and a local KV/Redis-compatible service surface if v0 needs it to boot | Centralize local DB and KV service selection; keep product schema, keys, and policy in v0 |
| Routes and URLs       | localhost routes, preview URLs, guest/public endpoint distinctions                | Route registry/profile helpers that expose host URLs and guest-visible URLs                                    | Move URL construction behind small helpers                                                |
| Logs and diagnostics  | local CLI/status joins process, VM, route, service, cache, and KV state           | Substrate read model for services, sandboxes, routes, snapshots, and health                                    | Keep product-specific joins in v0 or optional adapters                                    |

## Sandbox runtime details

This is already underway.

`local-sandbox` should expose a TypeScript package shaped around the subset of `@vercel/sandbox` v0 uses:

- `Sandbox.create()`
- `Sandbox.get()`
- `sandbox.runCommand()`
- `sandbox.getCommand()`
- `sandbox.writeFiles()`
- `sandbox.extendTimeout()`
- `sandbox.stop()`
- `sandbox.updateNetworkPolicy()`
- `sandbox.snapshot()`
- `Command.logs()`
- `Command.output()`
- `Command.wait()`
- `Command.kill()`

The Rust core contract should stay substrate-shaped. The TypeScript façade absorbs production SDK compatibility.

## S3-compatible storage details

Recon should answer:

- Where does v0 create S3 clients?
- Which object operations does it actually use?
- Which buckets, prefixes, credentials, and env vars are assumed?
- Which URLs are consumed by server code, browser code, and guest VMs?
- Which local MinIO startup/config code becomes obsolete if `local-sandbox` provides a service profile?

Initial guess:

- `local-sandbox` owns MinIO lifecycle, health, logs, bucket bootstrap, and local credentials.
- `local-sandbox` exposes a profile containing endpoint, public endpoint, region, bucket, access key, and secret key.
- v0 owns product storage semantics such as git bundle naming, commit backup policy, and chat/project associations.
- v0 may need a small client-construction helper so production and local clients are selected the same way.

Open question: whether public URL behavior needs a compatibility helper or whether endpoint/profile data is enough.

## Postgres / KV details

Recon should answer:

- Which v0 code talks to PGlite directly?
- Which code expects Redis/Upstash-shaped KV behavior?
- Which local KV/Redis-compatible service surface does v0 need to boot without carrying local service code?
- What schema/bootstrap does local mode require?
- Which readiness checks and stale-state cleanup are substrate facts vs v0 warm-pool policy?

Initial guess:

- `local-sandbox` owns Postgres/PGlite lifecycle, health, logs, and connection profile.
- `local-sandbox` should own the local KV/Redis-compatible service surface if v0 needs it to boot; v0 owns Redis-shaped key schemas and product policy.
- Bootstrap helpers may live in `local-sandbox` if they are only service readiness/schema setup and contain no chat/block/project concepts.

Open question: whether to expose a Postgres-compatible service only, or also a KV-compatible API.

## Route and URL details

This may be the hidden hard part.

Recon should answer:

- Where are local preview URLs created?
- Where are production subdomain/token assumptions rewritten for local mode?
- Which URLs are host-visible vs guest-visible?
- Which services need both private and public endpoints?
- Which helpers could make v0 stop branching on local mode throughout product code?

Initial guess:

- `local-sandbox` should expose route facts and service endpoint facts.
- v0 should keep preview semantics, auth/session ownership, and product routing decisions.
- A small shared local profile object may be more valuable than separate ad hoc env vars.

## Local profile

A likely unifying abstraction is a local profile returned by the daemon or generated by the CLI.

Possible shape:

```json
{
  "daemon_url": "http://127.0.0.1:7331",
  "sandbox": {
    "provider": "vfkit"
  },
  "services": {
    "postgres": {
      "url": "postgres://...",
      "ready": true
    },
    "s3": {
      "endpoint": "http://127.0.0.1:9000",
      "public_endpoint": "http://127.0.0.1:9000",
      "region": "local",
      "bucket": "v0-local",
      "access_key_id": "...",
      "secret_access_key": "...",
      "ready": true
    }
  },
  "routes": {
    "host": "127.0.0.1"
  }
}
```

The exact shape should come after recon. The important property is that v0 asks one local substrate for local connection facts.

## Decision criteria

Proceed with an interface-compatible local service when:

1. The v0 diff is centralized around client/profile selection.
2. The change deletes or obsoletes more local substrate code in v0 than it adds.
3. The local-sandbox interface has a product-neutral name and contract.
4. Unsupported production behavior fails explicitly.
5. Contract tests can cover the behavior before full runtime parity exists.
6. The boundary still helps future local providers rather than encoding macOS/v0 accidents.

Stop or redesign when:

- v0 needs local-mode conditionals across product code.
- the proposed local-sandbox API contains chat/block/project concepts.
- compatibility requires emulating accidental bugs instead of clarifying a seam.
- service URL/auth behavior is more important than the API shape and has no clear owner.

## Recon plan

### Recon A: S3 expectations

Produce:

- operation inventory
- env/config inventory
- URL/public endpoint inventory
- v0 deletion candidates
- proposed `local-sandbox` S3 profile/service API

Likely v0 areas:

- local dev service startup
- S3 client construction
- git bundle storage
- commit backup storage
- deployment/git storage provider selection

### Recon B: Postgres/KV expectations

Produce:

- PGlite lifecycle inventory
- KV method inventory
- schema/bootstrap inventory
- readiness/stale cleanup split
- recommendation: Postgres-only vs daemon-owned KV-compatible local service

Likely v0 areas:

- KV abstraction
- local Postgres KV adapter
- local dev startup
- warm-pool readiness and cleanup
- dashboard read model joins

### Recon C: URL and client construction

Produce:

- all local-mode endpoint creation sites
- host-visible vs guest-visible URL table
- client creation seams that can absorb local/production choice
- proposed local profile shape

Likely v0 areas:

- env helpers
- vfkit route mapping
- local dev env injection
- readiness utilities
- frame/chat/web URL handling

## PER plan

After PR 2 merges:

### PER 3A: Compatibility recon

No implementation. Produce the inventories and proposed boundaries above.

### PER 3B: Local profile contract

Add a product-neutral profile shape to the daemon/client that can expose service endpoints and route facts.

### PER 3C: S3 service compatibility

Implement local S3 lifecycle/config/profile support and contract tests.

### PER 3D: Postgres/KV service ownership

Implement Postgres service profile first. Treat KV as a daemon-owned local service surface if v0 needs Redis/Upstash semantics to boot, while keeping product key schemas and policy in v0.

### PER 3E: v0 integration spike

Create a small v0 branch that selects local-sandbox interfaces and measures the resulting diff. Use deletion of superseded v0 local substrate code as the success signal.

## Consequences

Benefits:

- smaller v0 migration diff
- clearer ownership boundary
- easier review story for `local-sandbox`
- better contract tests
- less bespoke local-mode code in v0

Costs:

- requires careful recon before implementing each compatibility surface
- may need small v0 refactors around client creation and URL selection
- risks hiding product semantics in substrate if boundaries are not policed
- local profile shape could become a dumping ground unless kept product-neutral

## Current recommendation

Adopt interface compatibility as the guiding design principle, but do not rewrite the PER dashboard until PR 2 is merged and compatibility recon has tested the idea against S3, Postgres/KV, and URL construction.
