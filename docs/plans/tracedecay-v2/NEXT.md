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

The canonical delivery and root-breakup execution authority is
[`docs/superpowers/plans/2026-07-28-v2-delivery-root-crate-breakup.md`](../../superpowers/plans/2026-07-28-v2-delivery-root-crate-breakup.md).
Publish and measure a clean integration `BASE_SHA`, create
`codex/v2-root-breakup` from that exact pushed commit, then execute Plan 12
leaves-first extraction. Query and code-index Gate A work may precede and then
run alongside the remaining PR12/PR13 closure; speculative adapter/runtime
crates do not block it. Core Work/Plans 24+32 and desktop-first dashboard
acceptance are PR14; residual advanced workflow capability is PR17.

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

PR14 remains blocked until its named Plan 11 gaps are closed and PR12/PR13
production contracts, direct tests, and normal CI are stable.

The 2026-07-25 PR14 integration checkpoint is nevertheless implemented and
must not be replanned as missing: the real `app-dist` application serves `/`
and the legacy placeholder is isolated at `/legacy`; real Settings capability
state, storage budget/unreadable-role findings, truthful graph failure states,
the Explorer coordinator and source-local/LCM read path, Loom time boundaries,
Delivery, Doctor, storage telemetry, asset serving, and feedback observation
wiring are present.

As of 2026-07-27 the Rust `dashboard_api_test` suite completes successfully
(58 run, 58 passed, twice, with `--all-features`), so Settings CAS, Delivery,
Explorer routes, Doctor, storage telemetry, Loom, and asset serving are no
longer blocked on it. This is still not acceptance: PR14 owes the Plan 11
performance budgets, renderer parity/fallback measurement, SSE-churn sustain
runs, real-Chrome visual review, manual assistive-technology completion, and
the usability study. See
[`GAP-LEDGER-PR8-PR14.md`](GAP-LEDGER-PR8-PR14.md) for the closed items.

**Operational status (2026-07-27; not green).**

- `cargo dogfood` still exits nonzero. Doctor reports
  `authority_audit_unavailable`, and Cursor Core has a component-ownership
  conflict. Plan 09 owns the Doctor diagnosis; Plan 27 and this PR12/PR13 slice
  own the host lifecycle/ownership repair.
- Semantic search is disabled by an invalid configuration snapshot. Plan 20
  owns snapshot validity/forward repair and Plan 31 owns activation; exact,
  lexical, and graph search must continue unchanged.
- The live profile was observed 237 minutes stale; during this reconciliation
  the index reported that refresh had begun after 285 minutes of staleness.
  Plan 25 and this slice own cadence/freshness diagnosis. Serving the previous
  complete generation during refresh is delivered behavior, not a fix for a
  refresh that fails to start promptly.
- Roughly eight known test failures remain, and roughly 4,169 tests have not
  been measured because no full suite has completed. Focused green runs and the
  failure-fix clusters landed today do not satisfy the aggregate gate.
- CI has not run since 01:24 UTC. PR #421 has been conflicting since 05:13 UTC,
  and `pull_request` workflows cannot build a merge ref for a conflicting pull
  request, so roughly 60 commits — including the whole 2026-07-27 night batch —
  carry scoped local verification only. Restoring a mergeable PR #421 is a
  precondition for any aggregate claim about this slice.
- Seven independent lanes found the same failure family on 2026-07-27: a gate
  reporting success without exercising what it names, including a Windows job
  whose nextest filter matched zero tests, a reachability test asserting that a
  symbol name appears in source text, and an accessibility gate reporting zero
  violations across five workspaces it never visited. Because libtest exits 0 on
  an empty name filter, a dangling `cargo test --exact` is silently vacuous
  forever. Gate review for this slice must prove a filter selects a nonempty set,
  and must read a green gate's scope against the surface it claims to cover. The
  ledger records all seven.
- The deleted-test restoration from `9e3ca9fd2` is complete as of 2026-07-28.
  All five priority groups landed (`be09be406`, `51cfedaf1`, `aec84a3ba`,
  `a221b4c1e`, `d01d48f25`), each with its key test proven falsifiable by a
  reverted production-side mutation probe. One correction travels with that:
  the Plans 18/23 temporal-privacy coverage was never lost — it had been
  migrated in-crate, and only two legs were dropped, both now restored. One
  residual gap stays open: the reopen test's generation-rebuild leg. The ledger
  carries the detail, the correction, and the placement finding that this
  coverage belongs in-crate rather than in `tests/session_suite`.

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
   currently reparses the admitted snapshot from canonical bytes. A future
   owner may re-evaluate tree reuse from measurements, but there is no retained
   cache scaffold to complete.
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

0. Publish the clean integration baseline, materialize the canonical/child
   plans, and bootstrap the exact-SHA `codex/v2-root-breakup` worktree. Extract
   domain DTO/RequestContext, query, and code index under Gate A before
   speculative adapter extraction; continue PR12/PR13 closure after Gate A.
1. Clear root and SQLite runtime compile blockers without weakening contracts.
   *(Landed 2026-07-23: the rusqlite storage runtime is cut over and
   repository reads route through the daemon-owned runtime port; the
   outbox/inbox effects read port and the code/effects read-contract arms
   exist. Landed 2026-07-25: the daemon-side git-index actor routes its
   production reads through the typed code-family executor rather than a
   parallel direct-read path.)*
2. Finish PR12 application, transport, LSP, cancellation, streaming, and
   distribution reachability. *(In flight. Landed 2026-07-27: daemon shutdown
   cancellation now reaches startup transcript/provider ingest and code-index
   reconciliation, prevents cancelled finalization/backfill, and has focused
   regressions. This closes the startup/shutdown cancellation sub-slice, not
   PR12 transport/distribution acceptance.)*
3. Finish PR13 Hook V2, Context Scout, advisory authorities, host lifecycle,
   Cursor extension, and daemon project-open registration. *(In flight; hook
   wiring and FastEmbed acceptance wiring may be landing concurrently. Landed
   2026-07-29: the four Plan 27 host lifecycle/capability guards recorded as
   unreachable on 2026-07-26 are production-reached — competing-extension
   discovery now supplies the official component-set dry run and binds its
   claims to the confirmed plan digest, Cursor Cloud and the other unadmitted
   hosts return typed unavailable component sets instead of empty defaults,
   Cline-family routes come from the embedded digest-bound evidence packet
   rather than adapter-file resemblance, and the native edit/stop conformance
   matrix is consumed by the host-bundle Doctor report even with nothing
   installed. Focused local evidence only: `pr13_host_bundle_acceptance` 27
   passed, `agents::host_bundle_registry` 20 passed, `agent_cmd::tests` 18 of 19
   passed. That closes those four guards, not PR13 host acceptance — the
   lifecycle dogfood, cross-platform runs, host-by-host rollback, feedback
   rollback switch, Kimi Code/OpenCode conformance, and an end-to-end
   Cline-family route proof are still unrun, and the Cursor Core
   component-ownership conflict above stays open. The one failure inside the
   touched territory, the `agent_cmd` binary's
   `explicit_core_component_lifecycle_preserves_opencode_companions`, is a
   pre-existing isolation flake — it passes alone and fails in parallel because
   `which_tracedecay()` reads `PATH` and `CARGO_TARGET_DIR` while sibling tests
   mutate the environment under a `HOST_ENV_LOCK` it does not take — and it
   remains open until that lease is fixed separately;
   `deferred_kimi_refresh_does_not_block_maintenance` and a daemonless-init
   bootstrap test fail in untouched peer territory and are outside this slice.)*
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
   available. Landed 2026-07-27: a busy scheduler serves its prior complete
   immutable generation without blocking a foreground query, and shutdown
   cooperatively interrupts reconciliation. The bounded daemon scheduler mount
   and semantic wiring exist; remaining work is the measured cadence defect,
   semantic activation from a valid configuration snapshot, and aggregate
   acceptance.)*
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
   `SqliteStoreSizeTelemetryPort` at
   `crates/tracedecay-rusqlite-runtime/src/telemetry/store_size.rs` implements
   `StoreSizeTelemetryPort` through the retained read-only SQLite health
   reader. The dashboard exposes per-store size/free ratio and whole-store
   history; the daemon Doctor kernel emits `dbstat` table-growth samples only
   as tracing. They are not Doctor findings, dashboard payloads, or CLI Doctor
   output, so this path does not validate historical ad hoc per-table byte
   estimates. Owner-configured soft budgets remain inert when absent.)*
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
   minor. Current qualification 2026-07-27: roughly eight known failures remain
   and roughly 4,169 tests have not been measured in a completed full run.)*

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

**Status (2026-07-27).** The incremental-indexing verification items now have
landed direct regressions: no-op/save-without-change suppression, one-symbol
edit narrowing, deletion tombstoning, staged/unstaged/untracked/deleted
classification, and two-worktree shared-byte reuse without identity aliasing.
Project open and exact/lexical/graph search stay available while semantic work
loads — including non-git roots, where code-index mounting is skipped by
design — and a foreground query now returns the prior complete generation
while a refresh owns the scheduler. Cooperative startup/shutdown cancellation
and bundled-SQLite FTS self-heal also have direct regressions. Remaining
verification work: diagnose the observed stale-index cadence, restore semantic
activation from a valid configuration snapshot, run the full event-to-ready
p50/p95/p99/queue/amplification measurements, and complete the default and
all-feature release/full-suite gates.

## Done

This slice is complete when PR12/PR13 are production-reachable across supported
surfaces, incremental worktree indexing is bounded and measurably avoids
unrelated work, ordinary search remains available during indexing, only
complete compatible generations publish, and the all-feature distribution and
direct executed product/test gates pass with truthful pass/fail/pending status.
