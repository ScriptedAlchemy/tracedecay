# PR12/PR13: production integration and incremental indexing

**Status:** active execution slice.

PR8 is complete. PR9 and PR10 have callable code-index, lexical/graph, vector-
generation, FastEmbed, exact-flat, calibration, and fallback implementations;
their direct quality/resource evaluation is now recorded (2026-07-23) in the
single benchmarks/search-quality record — an offline direct evaluation with no
PR acceptance receipts or locked gates. PR11's application,
policy, catalog, configuration, Git, and feedback-cycle core is implemented.
The current delivery slice closes PR12/PR13 production reachability, host
delivery, all-feature distribution, and the incremental indexing behavior
required to keep those surfaces fast while repositories and worktrees change.

This file is contributor guidance only. TraceDecay never parses, imports,
schedules, or executes roadmap documents.

The authoritative PR8–PR14 plan-status adjudications and retractions live in
[`GAP-LEDGER-PR8-PR14.md`](GAP-LEDGER-PR8-PR14.md) as a companion, not a
replacement. This file remains the active PR12/PR13 execution slice.

## Current outcome

Ship one production path in which:

- CLI, MCP, HTTP, SSE, LSP, and supported host adapters reach the same
  application/catalog owners;
- post-edit diagnostics, impact, affected tests, GitHub review ingestion, CI
  localization, and agent proximity remain generation-bound and read-only;
- saved edits trigger bounded background code-index and semantic projection
  work without delaying project open or exact/lexical/graph search;
- only complete immutable generations become current through atomic
  publication; and
- release and beta distributions build, test, package, install, and execute
  with the default feature set equal to `--all-features`, including FastEmbed
  and bundled ORT.

PR14 remains blocked until PR12/PR13 production contracts, direct tests, and
normal CI are stable.

The 2026-07-25 PR14 integration checkpoint is nevertheless implemented and
must not be replanned as missing: the real `app-dist` application serves `/`
and the legacy placeholder is isolated at `/legacy`; real Settings capability
state, storage budget/unreadable-role findings, truthful graph failure states,
the Explorer coordinator and source-local/LCM read path, Loom time boundaries,
Delivery, Doctor, storage telemetry, asset serving, and feedback observation
wiring are present. This is not acceptance. The Rust `dashboard_api_test`
suite has not completed successfully, so Settings CAS, Delivery, Explorer
routes, Doctor, storage telemetry, Loom, and asset serving are implemented but
unverified.

## Worktree-aware incremental indexing contract

1. Resolve exact project, repository, checkout, worktree, ref, index, and
   captured-content identity before indexing. Paths and branch labels locate
   candidates but never provide identity or authorize cross-worktree reuse.
2. Use `gix` status/index/tree primitives to classify committed, staged,
   unstaged, untracked, deleted, and renamed paths. `gix` status is the sole
   truth; every hint is reconciled against it. Because TraceDecay's edits are
   agent-driven, host after-file-edit hooks are the primary hint source and
   require no standing filesystem watches. External or out-of-agent mutations
   are caught by a lazy three-tier freshness ladder evaluated on open, on hook
   receipt, and at query admission: (tier 1) a cheap per-query `.git` metadata
   fingerprint (`HEAD`, `index`, `packed-refs`) catches git-mediated changes
   immediately; (tier 2) a configurable bounded-staleness reconcile threshold
   re-checks `gix` truth for raw file writes, rsync, and other non-git
   mutations; (tier 3) identity re-resolution is the backstop, refusing any
   generation whose exact repository/worktree/ref identity no longer matches.
   A recursive `notify` watcher remains available as an off-by-default opt-in
   fallback for non-agent-driven setups; nothing depends on it being enabled,
   and overflow or dropped events resolve through the same bounded `gix`
   reconciliation rather than a guessed incremental update.
3. Cold discovery uses the existing ignore-aware parallel walker. Warm edits
   compare content and descriptor digests before parsing, so duplicate
   notifications and save-without-change perform zero parse, graph, lexical,
   or embedding work.
4. Retaining the prior Tree-sitter tree to apply `InputEdit` and narrow
   extraction via `changed_ranges` is an optimization only: canonical content,
   descriptor, sanitizer, and chunk digests remain product identity. This
   optimization is UNIMPLEMENTED BY DECISION (2026-07-23). Adversarial review
   found the saved-tree cache disconnected from extraction — never wired in and
   net-negative — so it was deleted; because this clause already treats tree
   reuse as optional, removing it stays contract-compatible. Warm parsing
   currently reparses the admitted snapshot from canonical bytes; the tree
   cache is retained only as a possible future optimization, not current
   behavior.
5. Rebuild only changed symbol chunks, enclosing structural ancestors,
   affected file-level chunks, and dependency/test-attribution closures whose
   evidence changed. Deletions produce tombstones. Rename/move reuse requires
   matching content and extraction inputs and still records explicit lineage
   evidence.
6. Keep immutable generations per exact worktree snapshot. Content-addressed
   parse/chunk/projection artifacts may be physically shared across worktrees
   only when repository content, descriptor, sanitizer, privacy domain, key
   epoch, and projection keys match. Shared bytes never merge worktree,
   occurrence, generation, authorization, or lineage identity.
7. Batch only added/changed semantic chunks through FastEmbed's local
   user-defined model API. A no-op performs zero inference; a projection-key
   change replays retained eligible chunks without reparsing. Semantic work is
   asynchronous, cancellable, resource-bounded, and lower priority than
   interactive exact/lexical/graph requests.
8. While code or semantic indexing is pending, ordinary search uses the latest
   compatible complete generation and reports freshness/coverage. The semantic
   lane is omitted until a complete compatible vector generation is atomically
   current. Partial, stale, failed, cancelled, or incompatible generations
   never affect ranking, caps, cursors, or fallback bytes.
9. Coalesce superseded edit batches by exact worktree and content frontier.
   Bound queue depth, bytes, parser workers, embedding sessions, and publication
   concurrency. Preserve fair progress across active worktrees and cancel work
   whose snapshot can no longer publish.

## Active implementation order

1. Clear root and SQLite runtime compile blockers without weakening contracts.
   *(Landed 2026-07-23: the rusqlite storage runtime is cut over and
   repository reads route through the daemon-owned runtime port; the
   outbox/inbox effects read port and the code/effects read-contract arms
   exist. Landed 2026-07-25: the daemon-side git-index actor routes its
   production reads through the typed code-family executor rather than a
   parallel direct-read path.)*
2. Finish PR12 application, transport, LSP, cancellation, streaming, and
   distribution reachability. *(In flight.)*
3. Finish PR13 Hook V2, Context Scout, advisory authorities, host lifecycle,
   Cursor extension, and daemon project-open registration. *(In flight; hook
   wiring and FastEmbed acceptance wiring may be landing concurrently.)*
4. Mount incremental code and FastEmbed workers behind daemon-owned bounded
   scheduling while keeping project open and ordinary search non-blocking.
   *(Partially landed 2026-07-23: worktree-aware incremental indexing is
   implemented to the contract — exact identity resolution, `gix`
   classification, the hook-driven primary hints, the three-tier freshness
   ladder with a cheap stat-signature tier-2 gate and loose-ref content
   fingerprint, opt-in watcher, and an extracted scheduler registry with
   non-serializing cross-worktree queries via `spawn_blocking`. Project open
   now skips code-index mounting gracefully for non-git roots — the code index
   has no identity without a repository, and every other surface stays
   available. Remaining: daemon-owned bounded worker mount and, if not yet
   landed, FastEmbed acceptance wiring.)*
5. Add worktree/edit/no-op/rename/delete/overflow/cancellation/restart
   regressions and current/10x performance evidence. *(Partially landed
   2026-07-23: identity isolation, byte-reuse-without-alias,
   staged/unstaged/untracked/deleted classification, deletion tombstoning, and
   non-serializing query regressions exist. Remaining: current/10x performance
   measurement evidence.)*
6. Deliver storage retention/size/efficiency per
   [plan 38](38-storage-retention-size-and-efficiency.md): automatic
   branch-DB lifecycle, registry orphan detection/collection, session
   retention with raw/projected dedup, incident-debris ownership,
   compaction policy, and Doctor storage findings (measured driver: one
   dogfood profile reached 256 GB, reduced to ~75 GB by removing data the
   product should never have retained). *(Landed 2026-07-23: the Plan 38
   mechanisms are present — §1 branch lifecycle (verified pre-existing), §2
   registry orphan detection/collection, §3 session retention, §4
   one-content-copy machinery, §5 debris contract, §6 compaction policy types,
   and §7 telemetry read models with typed Doctor storage findings. Status
   correction 2026-07-26: §3/§4 are engaged by default. Projection-durable raw
   LCM payloads offload after 30 days and drop after 180 days, projected copies
   dedupe after 30 days, legacy session/raw pruning defaults to 180 days once
   durable summary lineage exists, and superseded/deleted observation payloads
   default to a 30-day release window. Superseded `source_cursor_advances` are
   reclaimed by the daemon-authorized retention transaction while preserving
   the current-frontier receipt and restoring the immutable delete trigger
   before commit. Daemon-owned GC/retention/compaction cadence runs under
   writer authority, retries failed passes without advancing the cadence, and
   reauthorizes each retention transaction; exact registry relink/retirement,
   durable incident-debris quarantine/collection, and real stale-branch and
   retention-backlog Doctor sources are wired.
   `SqliteStoreSizeTelemetryPort` implements `StoreSizeTelemetryPort` through
   the retained read-only SQLite health reader; Doctor consumes its store-size
   and `dbstat` table-growth reads. That product path does not validate
   historical ad hoc per-table byte estimates. Owner-configured soft budgets
   remain inert when absent.)*
7. Run focused crate tests, all-feature workspace checks, release builds,
   package/install checks, and normal Linux/macOS/Windows CI. *(Ongoing. The
   SQLite session-store parity harness proves 27 session-store tables across
   the temporal, summary, Fact (including retrieval anchors), Diagnostics, and
   Configuration families, with a column-coverage guard, exact-digest oracles,
   a shared fixture DDL, and compiler-enforced exhaustive daemon shape checks;
   its evidence is copied-bytes self-consistency, not byte identity against the
   retired libSQL implementation, and one known caveat is
   `configuration_entries.layer_id` being
   nullable-but-keyset-bearing. Config authority resolves-and-pins on demand at
   open — cold-cache/restart safe — and read-only opens of unseeded stores
   serve registry defaults. Dead-code allow remediation is complete and the
   unwired hooks v2 ready-guidance lookup is deleted; the V2 version bump is
   deferred to release-plz, with semver-checks declaring the release-type major
   as its vocabulary for a breaking change while the shipped bump stays a 0.x
   minor.)*

## Direct verification

- duplicate filesystem events and save-without-change cause zero durable or
  projection work;
- a one-symbol edit reparses one file and changes only the symbol, enclosing
  file chunks, and evidenced dependency/test closures;
- rename, deletion, branch switch, rebase, index-only edit, and dropped watcher
  events reconcile to the same manifest as a clean scan;
- two worktrees with shared blobs reuse physical parse/chunk/vector bytes but
  retain distinct snapshot, occurrence, generation, authorization, and
  publication identity;
- unsaved LSP overlays remain client-local and create no durable generation;
- project open and exact/lexical/graph search complete while FastEmbed loads or
  indexes, with semantic results absent until atomic activation;
- cancellation, crash, or incompatible inputs leave the prior compatible
  generation current and expose no partial state;
- measurements report event-to-ready p50/p95/p99, queue delay, files hashed and
  parsed, changed ranges, chunks reused/changed/deleted, invalidation fan-out,
  embedding batches/chunks, CPU, peak RSS, read/write amplification, and full-
  rebuild reasons; and
- default and explicit all-feature release artifacts pass build, test, package,
  install, host-bundle, LSP, PR12/PR13 surface, and FastEmbed smoke checks.

**Status (2026-07-23).** The incremental-indexing verification items now have
landed direct regressions: no-op/save-without-change suppression, one-symbol
edit narrowing, deletion tombstoning, staged/unstaged/untracked/deleted
classification, and two-worktree shared-byte reuse without identity aliasing.
Project open and exact/lexical/graph search stay available while semantic work
loads — including non-git roots, where code-index mounting is skipped by
design. Remaining verification work: the full event-to-ready
p50/p95/p99/queue/amplification performance measurements and the default and
all-feature release-artifact gates.

## Done

This slice is complete when PR12/PR13 are production-reachable across supported
surfaces, incremental worktree indexing is bounded and measurably avoids
unrelated work, ordinary search remains available during indexing, only
complete compatible generations publish, and the all-feature distribution and
direct executed product/test gates pass with truthful pass/fail/pending status.
