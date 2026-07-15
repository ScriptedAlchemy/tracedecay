# V2 store boundary

## Status / Role

PR5 production observation persistence is complete. `tracedecay-store` owns persistence
contracts and DTOs; the daemon-owned `GlobalDb` adapter owns live connections
and transactions. This boundary participates in vertical PRs and does not grow
into a second database implementation. See [the plan index](00-plan-set-index.md)
and [global ownership rules](README.md).
Each store slice records its production baseline; [PR20](33-end-to-end-performance-optimization.md)
owns measured cross-path database and write-amplification optimization.

## Outcome

All TraceDecay clients resolve one authoritative database path per mutable
shard: clients call the owning daemon, and that daemon reads and writes through
its already-open fenced authority. Committed data, receipts, and progress
cannot diverge after crashes or retries.

## Owns

- Store-facing records, batches, errors, and persistence traits.
- The transcript contract landed in PR4, including explicit physical transcript
  identity and separate opaque cursor identity.
- Shipped atomic append contract for sanitized observations, receipts, and offsets.
- Atomic projection-effect and checkpoint contracts added with each consuming
  view slice.
- PR9 canonical clean-generation diagnostic records and snapshots, including
  clearing and supersession evidence.
- Contract-level idempotency, compare-and-set, read-only, and recovery outcomes.

## Does not own

- Opening databases, selecting paths, holding production connections, or
  creating fallback writers; those remain in the daemon `GlobalDb` adapter.
- Parsing, sanitization, projection semantics, query planning, policy, HTTP,
  MCP, CLI, dashboard, hooks, or host workflows.
- A client-side, hook-side, source-adjacent, in-memory, recovery, or remote
  database authority.
- Unsaved LSP overlays, per-client document versions, or an analyzer-local or
  client cache database.
- Delivery metadata, speculative schemas, or a separate database per branch.
  Only code-graph indexes are branch/worktree scoped.

## Required behavior

- PR4 routes CLI, MCP, dashboard, hooks, analytics, LCM, and ingestion through
  the daemon authority; daemon unavailability fails closed.
- PR4 commits a transcript batch and its offset atomically. A failed write leaves
  both unchanged and the same writer remains usable after rollback.
- PR4 full-batch cursor compare-and-set is strict; compatible offset-only advance
  is idempotent and cannot create transcript rows.
- PR4 read-only audit paths do not create a missing database or become writers.
- PR5 commits the sanitized observation, sanitization receipt, and source offset
  in one authoritative transaction; acknowledgement follows commit.
- PR5 duplicate identity plus matching digest is a no-op. A conflicting digest
  fails without advancing progress or overwriting evidence.
- PR9 persists only canonical, sanitized diagnostics bound to a clean code
  generation, with clearing and supersession evidence, through daemon-owned
  store adapters. Unsaved overlays and client document versions remain
  ephemeral daemon session state and never become durable authority; see
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Projection slices commit all effects and their checkpoint together. A failed,
  partial, stale-owner, or dead-letter batch cannot advance the checkpoint.
- Project facts and sessions remain project-wide; user sessions remain
  profile-wide; code graphs retain exact repository/worktree/ref scope.
- Real Doctor, backup, integrity, and recovery operations use the same daemon
  authority and return typed findings/receipts. They never heal by opening an
  alternate writer.

## Acceptance

- PR4: `transcript_batch_survives_restart_and_replay_is_idempotent` passes.
- PR4: `late_cursor_failure_rolls_back_every_transcript_write_then_retries` and
  `invalid_batch_mutates_no_transcript_state` pass.
- PR4: concurrent full and offset-only batch tests prove convergence without
  split brain or partial writes.
- PR4: daemon-only writer, read-only no-create, and post-rollback writer-reuse
  regressions pass.
- PR5: kill-point tests around observation, receipt, offset, commit, and
  acknowledgement prove complete commit or safe retry.
- PR9 diagnostic persistence tests reject dirty overlays, mismatched content
  digests, and client-local authority while preserving explicit clears and
  supersession across restart.
- Each projection PR proves atomic effect/checkpoint rollback and deterministic
  restart before its view becomes queryable.
- Doctor tests prove diagnosis is read-only and every applied repair is
  authority-fenced, idempotent, and receipt-bearing.
