# TraceDecay V2 rewrite

Status: active product rewrite.

## What exists

- `tracedecay-domain` contains the first executable V2 foundation: versioned domain and research contracts.
- `tracedecay-store` owns the canonical transcript persistence contract and delegates production writes to the already-open `GlobalDb` authority.
- Transcript ingest, startup catch-up, restart recovery, and daemon/MCP/dashboard paths use that boundary without a fallback writer.
- Direct tests cover atomic batches, durable monotonic offsets, replay, partial-line deferral, rollback, and single-owner concurrency across Claude, Cursor, and Cline-like inputs.
- Transcript and LCM mutations use fresh RAII transactions owned by the authoritative `GlobalDb`; cancellation or failure rolls back database rows and newly created external payload files together.
- The root integration test keeps a small, direct research-anchor contract.
- Existing runtime Doctor, daemon, storage, hooks, MCP, and CLI behavior remain product code. They are not replaced by inventories or plan metadata.

## What was removed

- The compatibility-inventory binary and production module.
- Generated architecture views, policy generators, source/YAML parsers, snapshot envelopes, and receipt catalogs.
- Abandoned evidence/privacy-corpus infrastructure and scanner-specific CI lanes.
- Agent skills and large Markdown checklists for executing the rewrite plan.
- Plan parsers, workflow executors, and incremental-PR orchestration artifacts.

Those systems modeled the rewrite instead of delivering it. They are intentionally not part of V2.

## Storage scope

- Project facts and project session/LCM data live in one canonical project-wide
  store shared by every branch and worktree of that project.
- Account-wide user sessions live in the user/profile store, not in a project or
  worktree store.
- Only code-graph indexes are branch/worktree-scoped. A worktree resolves its
  canonical project through the project registry and Git common directory.
- If the required project or user-store authority cannot be resolved, the
  operation fails closed. It must not create or write a worktree-local fallback.

## Release

Release configuration publishes the V2 library crates while reserving the single Git tag and GitHub release for the root package. The first crates.io publication of each new crate requires a one-time token, manual bootstrap, or trusted-publisher setup; later releases can use the normal workspace release flow.

## Delivery rule

Each rewrite change must ship executable product behavior and direct tests of that behavior. Do not add a second metadata model of the product, generated plan views, or CI that validates planning artifacts.

Custom Rust macros and generators have a separate negative-code admission budget. See [RUST-METAPROGRAMMING.md](RUST-METAPROGRAMMING.md) before introducing one.

PR4's production store boundary is complete. See [NEXT.md](NEXT.md) for the next executable product slice.
