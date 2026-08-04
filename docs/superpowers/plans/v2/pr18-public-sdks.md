# PR18 Public SDK Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.
> Historical version/compatibility/migration language cannot resurrect
> source-only/internal branch scaffolding. Only an actually independently
> released public API/schema revision may retain protocol compatibility.
> Persisted cursors, idempotency keys, journals, checkpoints, and receipts
> accept only their exact final shape; every other database, store, spool, file,
> or projection returns typed `ResetRequired` and requires explicit reset or
> recreation. No storage reader, migration, backfill, dual write, or census
> path exists.

**Goal:** Publish Rust and TypeScript SDKs for accepted PR12–PR17
operations without inventing lifecycle semantics. (The originally planned
Python SDK was dropped: delivery is TypeScript-first plus a retained Rust
SDK for native consumers, with no Python package.)

## Historical file and interface inventory

- Rust workspace SDK crate and generated wire types.
- TypeScript package root, generators, conformance fixtures,
  package metadata, examples, and release CI.
- Bind supported local daemon transports to one operation catalog.

Generated types cover wire schemas. Handwritten façades cover authentication,
`RequestContext`, paging/cursors, SSE reconnect, cancellation, resume,
idempotency, typed errors, operation receipts, `TaskHandoffToken`, and host
handoff tokens. Names freeze only after each operation's production journey is
accepted.

## Historical ordered slices

1. Freeze accepted operation/schema manifest.
2. Generate Rust/TS wire models deterministically.
3. Implement lifecycle façades and local transport.
5. Add examples and cross-language golden conformance.
6. Package/install/publish dry runs and compatibility policy.

## Product outcome contributed

The work contributed Rust and TypeScript SDK façades over one operation
catalog with equivalent authentication, scope, lifecycle, paging/SSE,
cancellation, idempotency, and typed outcomes. Current direct behavior and
acceptance live in the applicable numbered V2 plan.

## Historical release, reset, measurement, and deletion notes

Before first publication, generated schemas change in place. V2 branch-local
data uses the fresh-store cutover: only the exact final persisted shape is
accepted, and every other shape returns typed `ResetRequired` for explicit
reset or recreation. No storage reader, migration, backfill, dual write, or
census path survives. After an actual independent package release, public
schemas follow the accepted major-version compatibility policy. Rollback
unpublishes or yanks a package release according to registry
policy but never changes server semantics. Measure generation, package size,
startup, paging/SSE overhead, and conformance duration. Delete private client
wrappers and aliases only after two-language (Rust and TypeScript)
local conformance,
examples, package/install gates, semver review, and normal CI pass.
