# Storage retention, size, and efficiency

## Final authority

Owner-profile storage must remain proportional to live, retrievable value.
Final V2 creates every store in its final shape. An incompatible TraceDecay
store returns `ResetRequired` and may be cleared by an explicit profile-reset
journey; it is never read, migrated, relinked, backfilled, consolidated, or
dual-written.

Historical information from supported agent hosts is not legacy TraceDecay
state. It is acquired through ordinary bounded host capture and projected into
the final authorities.

## Placement

- SQLite owns relational records, immutable events, content locators,
  receipts, leases, watermarks, configuration, and transactional state.
- One embedded Grafeo store per canonical project owner shard owns graph
  topology and vector indexes for code, Git, sessions/LCM, Work, workflows,
  and typed cross-domain references.
- Holographic fact content and FHRR operations remain in the project-wide
  memory authority. Grafeo may store typed edges and vectors keyed by canonical
  fact identifiers, never a second fact payload authority.
- Branch, ref, worktree, commit, pull request, session, and agent identity are
  provenance and query selectors. They never own a database or fact shard.

All stores are opened only by the daemon/application composition. Clients,
hosts, hooks, SDKs, and the dashboard receive no raw path or writer handle.

## Reachability and retention

Retention is based on reachability and owner policy, not a universal age or
size threshold.

- Exact LCM source content remains recoverable while any authorized message
  occurrence, summary-source edge, evidence anchor, fact reference, active
  task/run, or retention hold reaches it.
- A summary is a derived navigation node and never replaces its sources.
- Duplicate inline bytes may be released only after the canonical
  content-addressed payload is durable and every authorized hydration path
  still returns the exact source.
- Derived indexes and superseded generations may be collected only after no
  live worktree snapshot, cursor, receipt, vector generation, graph reference,
  or rollback hold reaches them.
- Deleting a branch or worktree cannot delete or relocate a project-wide fact.
- Corruption and interrupted-operation debris is quarantined with typed
  metadata. It is never left as an unowned sibling file.

## Maintenance authority

The daemon maintenance application owns bounded, off-hot-path retention,
checkpoint, compaction, and quarantine operations for SQLite and Grafeo. Every
effect requires exact owner identity, authorization, a sync lease,
compare-and-swap preconditions where relevant, cancellation semantics, and a
durable receipt.

Foreground admission and exact/lexical/graph/ordinary retrieval do not wait for
historical convergence, semantic model acquisition, rebuilds, retention, or
compaction. Background work yields to foreground traffic and exposes typed
progress, saturation, failure, and retry state.

Doctor is read-only. It reports storage health and may name a separate
authorized maintenance operation, but it never opens a write transaction,
repairs, collects, checkpoints, compacts, vacuums, refreshes, or dispatches the
operation.

## Observability

Bounded telemetry covers:

- logical and physical SQLite/Grafeo size;
- reclaimable space and quarantine/debris size;
- active and retained generation/reference counts;
- WAL/checkpoint and Grafeo checkpoint/compaction state;
- retention backlog, oldest reachable/unreachable age, and progress;
- writer-lane occupancy, wait, cancellation, and saturation;
- last successful receipt and typed failure/partial/unavailable coverage.

Metrics never fabricate zero for unreadable state and never use
project/path/task/session/fact identity as high-cardinality labels. Owner
budgets and horizons are configuration, not release-wide hard-coded gates.

## Backup and restore

A profile backup captures a fenced SQLite snapshot and matching Grafeo
checkpoint under the same project/profile/store identity and watermark. A
restore validates both authorities before activation and cannot expose a mixed
generation. Graph/vector projections may be rebuilt from their final-V2 source
authorities; irreplaceable relational events, receipts, fact content, and LCM
source payloads must be restored, not inferred.

No backup, restore, or test reads the operator's real profile unless the user
explicitly selected that profile.

## Required production evidence

- Fresh-profile startup mounts the final SQLite and Grafeo authorities through
  one daemon owner identity with no client-side fallback.
- Linked worktrees share the project store while retaining exact snapshot and
  generation identity.
- Restart and crash injection preserve journals, watermarks, receipts, and
  active graph/vector generations across both stores.
- Lossless LCM tests page summary sources and hydrate exact content from each
  owning store after deduplication/offload.
- Retention refuses the last reachable LCM source, fact reference, graph
  generation, receipt, or rollback hold.
- Project-fact tests prove branch/worktree deletion cannot delete, move, or
  shard facts.
- Backup/restore proves a consistent SQLite/Grafeo generation and rejects
  identity or watermark mismatch.
- Read-only Doctor completes while the writer lane is occupied and produces no
  state change.
- Maintenance tests prove denial, stale lease, compare-and-swap conflict,
  cancellation, late failure, restart replay, and rollback.
- Measured workload comparisons report distributions and regressions without
  weakening semantics or raising timeouts.

## Rejected designs

- branch databases, branch-local facts, archive-merge fact movement;
- old TraceDecay store readers, migrations, backfills, dual writes, or
  compatibility schemas;
- SQLite graph/vector shadow authorities or Grafeo adapter-only mounts;
- lossless-source deletion because a summary exists;
- Doctor apply/repair/GC modes;
- client-side database access, fixed test counts, source-shape scans, or
  hard-coded performance gates.
