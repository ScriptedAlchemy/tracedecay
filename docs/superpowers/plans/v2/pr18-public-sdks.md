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
> source-only/internal branch scaffolding. Compatibility begins before package
> publication when an independently deployed dogfood client or host
> installation may retain an API/schema revision, and remains until the
> authorized installed-client/host census proves absence. Potentially persisted
> cursors, idempotency keys, journals, checkpoints, and receipts keep
> backward-read/recovery until the registered-store/profile census proves
> absence.

**Goal:** Publish Rust and TypeScript SDKs for accepted PR12–PR17
operations without inventing lifecycle semantics. (The originally planned
Python SDK was dropped: delivery is TypeScript-first plus a retained Rust
SDK for native consumers, with no Python package.)

## Historical file and interface inventory

- Rust workspace SDK crate and generated wire types.
- TypeScript package root, generators, conformance fixtures,
  package metadata, examples, and release CI.
- Bind local daemon and PR16 remote transports to one operation catalog.

Generated types cover wire schemas. Handwritten façades cover authentication,
`RequestContext`, paging/cursors, SSE reconnect, cancellation, resume,
idempotency, typed errors, operation receipts, `TaskHandoffToken`, and host
handoff tokens. Names freeze only after each operation's production journey is
accepted.

## Historical ordered slices

1. Freeze accepted operation/schema manifest.
2. Generate Rust/TS wire models deterministically.
3. Implement lifecycle façades and local transport.
4. Implement remote transport with identical semantics.
5. Add examples and cross-language golden conformance.
6. Package/install/publish dry runs and compatibility policy.

## Product outcome contributed

The work contributed Rust and TypeScript SDK façades over one operation
catalog with equivalent authentication, scope, lifecycle, paging/SSE,
cancellation, idempotency, and typed outcomes. Current direct behavior and
acceptance live in the applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

Before first publication, generated schemas change in place only when they
have not potentially reached an independently deployed dogfood client or host
installation. Potentially deployed schemas remain negotiated until an
authorized installed-client/host census proves absence. After an evidenced
package release, schemas follow the accepted major-version compatibility
policy. Rollback unpublishes or yanks a package release according to registry
policy but never changes server semantics. Measure generation, package size,
startup, paging/SSE overhead, and conformance duration. Delete private client
wrappers and aliases only after two-language (Rust and TypeScript)
local/remote conformance,
examples, package/install gates, semver review, and normal CI pass.
