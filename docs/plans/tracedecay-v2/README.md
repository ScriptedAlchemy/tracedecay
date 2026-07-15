# TraceDecay V2 rewrite

Status: active product rewrite. PR5 is complete, PR6 is next, and PR #421
remains open.

The authoritative delivery order is [00-plan-set-index.md](00-plan-set-index.md).
The next executable slice is [NEXT.md](NEXT.md). Numbered plans define component
requirements and boundaries, not separate crate-first work queues.

## Current product foundation

- `tracedecay-domain` contains the first executable V2 domain contracts.
- `tracedecay-store` defines canonical transcript persistence while the
  already-open `GlobalDb` remains the physical connection and transaction
  authority.
- Transcript ingest, startup catch-up, restart recovery, daemon, MCP, and
  dashboard paths use that authority without a fallback writer.
- Transcript batches atomically update messages, projections, durable cursors,
  and monotonic offsets. Replay and exact retries are idempotent.
- Transcript and LCM mutations use fresh RAII transactions. Failure or
  cancellation rolls back database rows and newly created payload files.
- Direct tests cover Claude, Cursor, Cline-like input, partial records, replay,
  rollback, restart, concurrency, and Windows behavior.
- Existing Doctor, daemon, storage, hooks, MCP, and CLI remain product code.
- Claude production capture now emits path-independent sanitized observations,
  typed receipts, durable cursors, and deterministic searchable projections.
- Observation, receipt, cursor, enqueue, projection effects, and checkpoints
  preserve atomic restart/retry behavior; exact no-op replay performs no writes.
- The committed PR5 workload and clean-commit acceptance artifact record the
  production parse/sanitize/commit/project/replay baseline for PR20.

## Storage and authority

- Before remote delivery, one local daemon is the sole mutable SQLite
  authority; PR16 preserves exactly one fenced daemon authority per mutable
  shard. Hooks, clients, workers, MCP servers, dashboard handlers, and remote
  nodes send typed operations to the owning authority.
- Project facts and project session/LCM data live in one canonical project-wide
  store shared across branches and worktrees.
- Profile-wide user activity lives in the user/profile store.
- Only code indexes are branch/worktree/snapshot scoped.
- Worktrees resolve their project through the project registry and Git common
  directory. Missing or ambiguous authority fails closed.
- No path may create a worktree-local, source-adjacent, in-memory, recovery, or
  direct-database fallback writer.

## Delivery rules

- Ship executable product behavior and direct tests in every PR.
- Prefer one end-to-end vertical slice over broad scaffolding.
- Component plans may contribute to the same PR. A plan name does not require a
  new crate, generator, registry, or standalone implementation phase.
- One typed kernel owns each mechanism. Public names and compatibility aliases
  are bindings, never alternate query, edit, storage, rendering, health, or
  workflow implementations.
- Preserve stock Cargo compatibility. Developer-local build wrappers and cache
  layouts are never repository or CI requirements.
- Use explicit cancellation and typed progress for long operations. Do not add
  an automatic rewrite, workflow, agent, or no-progress timeout.
- Keep privacy, recovery, concurrency, cross-platform, migration, and deletion
  gates with the product behavior they protect.
- Instrument each production path when it ships and retain a representative
  baseline for [PR20 performance optimization](33-end-to-end-performance-optimization.md).
- PR #421 merges only after PR20 completes and aggregate verification is stable.

## Removed permanently

- compatibility and architecture inventory implementations;
- plan Markdown parsers, PR-ID normalizers, slice DAGs, completion ledgers,
  progress trackers, next-ready controllers, and rewrite executors;
- generated plan views, owner maps, baseline packets, and planning-artifact CI;
- large agent checklists or Claude workflow JavaScript for executing the rewrite;
- parallel YAML/JSON/Markdown models that generate product declarations.

Real product generation remains legal only when it removes duplicate product
authorities and follows [RUST-METAPROGRAMMING.md](RUST-METAPROGRAMMING.md).
Real dynamic workflows are daemon-owned typed product operations. They never
parse or execute this roadmap.

## Release

V2 library crates publish through the workspace release flow while the root
package owns the Git tag and GitHub release. A new crate's first crates.io
publication may require one-time trusted-publisher or token bootstrap; this is a
release setup step, not an alternate development workflow.
