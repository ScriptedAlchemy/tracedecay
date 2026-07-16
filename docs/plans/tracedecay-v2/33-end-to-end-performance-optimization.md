# PR20: End-to-end performance optimization

**Status:** committed V2 delivery after PR19 convergence.

**Depends on:** [02 store](02-store-crate.md), [04 projectors](04-projectors-crate.md),
[05 query](05-query-crate.md), [25 code indexing](25-code-intelligence-indexing-crate.md),
[12 migration/cutover](12-root-compatibility-migration.md),
[19 convergence](19-system-defragmentation-convergence-and-extensibility.md),
[26 observability](26-observability-accounting-and-usage.md), and
[35 daemon LSP gateway](35-daemon-lsp-gateway-and-universal-diagnostics.md).

## Outcome

PR20 measures and optimizes the production database, synchronization,
projection, indexing, query, and repository-controlled developer-build paths as
one system. It preserves exact product semantics, privacy, authority,
durability, coverage, ordering, and recovery.
Performance work is complete only when representative end-to-end evidence shows
the improvement and the correctness gates remain green.

PR5–PR19 add bounded instrumentation and capture a representative baseline when
each path ships. PR20 owns cross-path optimization after V2 convergence; it
does not postpone an obvious unbounded queue, repeated no-op, or severe
regression discovered by an earlier slice.

## Measurement contract

- Before tuning or publishing results, review and freeze a versioned benchmark
  workload manifest per path containing the baseline build and commit, corpus and generation inputs,
  platform/hardware class, cold/warm preparation and warmup count, measured
  repetitions, variance method, concurrency/load schedule, and per-path
  regression thresholds. Changing it creates a new named baseline comparison.
- Pin workload, corpus/generation, schema, configuration, platform, hardware
  class, cold/warm state, concurrency, and coverage for every comparison.
- Report p50/p95/p99 latency and throughput for ingest, sync/catch-up,
  projection, index build/update, exact/temporal/graph/semantic query, and the
  representative end-to-end journeys.
- Measure [Plan 36](36-git-aware-change-context-and-index-transactions.md)
  status/diff/hunk preview and explicit index-transaction apply separately,
  including repository size, changed-path and hunk count, index-lock wait,
  bytes parsed/applied, and stale-preview rejection cost.
- Measure LSP cold and warm gateway/analyzer startup, workspace indexing,
  hover and navigation, edit-to-diagnostic and edit-to-context latency,
  request coalescing and cancellation propagation, cache-key hit/miss
  behavior, clean cache reuse and no-op work, concurrent isolated overlays,
  provider conflicts, analyzer duplication avoidance across hosts, bridge
  reconnect, and analyzer crash/recovery.
- Measure [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)
  one-shot per-trigger stage and total latency, budget consumption,
  dedupe/suppression, terminal outcome, bounded-render/truncation/expansion
  behavior, edit-to-durable-feedback latency per delivery adapter (LSP, hook,
  explicit diagnostics call), GitHub ingest/remap/surface latency, CI
  localization latency, and concurrent-agent proximity computation cost.
- Report peak and steady memory, CPU time/utilization, database and generation
  bytes, temporary space, bytes read/written, and write amplification.
- Separate queue, lock, I/O, parse, projection, model, merge, hydration, and
  rendering time where the production trace can attribute them safely.
- Compare baseline and candidate with repeated runs and visible variance.
  Missing, partial, sampled, capped, or noisy evidence cannot claim a win.
- Developer-build workloads use stock Cargo commands with an explicit package,
  target, feature set, test target, toolchain, and source change. Record clean,
  warm incremental, and exact no-op cases where applicable, including wall
  time, CPU time/utilization, peak memory, rebuilt units, codegen/link time,
  build-script execution, and cache outcome when the toolchain exposes them.
- Compare developer-build results on the same host and toolchain with equivalent
  source and build state. Local wrappers, target locations, concurrent-lane
  allocation, and Rust Analyzer processes are environmental context, not
  roadmap mechanisms or portable regression thresholds.

## Optimization requirements

### Database and synchronization

- Inspect production SQLite/libSQL query plans and measured hot statements;
  add or remove indexes from evidence, not table size or intuition alone.
- Bound transaction size, lock hold time, connection work, checkpoint cadence,
  WAL growth, vacuum/reclamation, and write amplification without weakening the
  daemon's sole-writer or atomic progress contracts.
- Coalesce equivalent sync/frontier requests, batch safely, preserve fair
  progress across sources, and make unchanged input perform bounded no-op work.
- Bound queues, workers, concurrency, retry state, and memory. Backpressure and
  overload are explicit typed outcomes; one project or client cannot starve the
  daemon.

### Projection, indexing, and caches

- Recompute only changed observations, files, symbols, dependents, documents,
  and vectors justified by versioned dependency evidence.
- Reuse compatible immutable generations and caches by complete content,
  schema, grammar/model, privacy, scope, and configuration identity.
- Bound cache memory and disk, define admission/eviction and idle lifecycle,
  delete superseded generations only after authority and recovery checks, and
  prevent rebuild storms or mixed-generation reads.
- Cancellation, disk-full, stale input, and concurrent rebuilds publish one
  complete verified generation or leave the prior generation authoritative.

### Query execution

- Use measured selectivity and costs to prune shards/candidates, avoid repeated
  hydration/parsing, reuse compatible prepared or derived state, and stop work
  at declared budgets and cancellation boundaries.
- Preserve deterministic order, exact-match tiers, temporal truth, stable
  cursors, coverage, explanations, and lexical fallback byte-for-byte where
  their owning contracts require it.
- Bound cross-project fan-out, graph traversal, reranking, result buffering,
  and per-client concurrency with explicit partial or unavailable coverage.

### Git intelligence and index transactions

- Set reviewed workload-specific p95 latency, peak-memory, and bytes-read
  budgets for Plan 36 read-only queries and preview; set bounded index-lock hold
  and apply/revalidation budgets for explicit mutations.
- Reuse native Git object, diff, patch, and index behavior plus the canonical
  graph/query caches. Do not build a second repository graph or retain patch
  content as a performance cache.
- Optimization cannot weaken `HunkRef` preconditions, preview revalidation,
  index-lock ownership, atomicity, receipts, or rejection of autonomous
  branch/worktree/ref/history mutation.

### LSP gateway and analyzers

- Attribute gateway, queue, bridge, upstream analyzer, indexing, merge, and
  publication latency and resource use without exposing private content.
- Share analyzers, clean generations, and caches only when complete identity
  matches and client overlay isolation remains exact.
- Coalesce equivalent in-flight requests and propagate cancellation to
  superseded work without dropping a response still needed elsewhere; cache
  keys cover the complete provider identity tuple so no distinct input aliases
  onto another's cached result.
- Process reduction, including avoiding duplicate per-host analyzer processes,
  is a resource optimization; it never justifies stale or cross-session
  results, weakened cancellation, incomplete provenance, or disclosure of
  unsaved content.

### Developer build and verification

- Reduce the frequently touched compilation graph by enforcing product crate
  ownership, removing obsolete dependency and feature edges, and keeping heavy
  grammars, model runtimes, providers, transports, dashboard assets, and
  test-only support out of unrelated focused package checks.
- Measure root-package fan-in and test-target compilation. Split an oversized
  integration-test binary only when representative focused workflows improve
  after accounting for additional codegen and linking.
- Keep build scripts deterministic, declare narrow rerun inputs, and skip
  generation work when the relevant source assets and enabled feature are
  unchanged.
- Portable Cargo manifest, configuration, profile, feature, and build-setting
  changes are valid optimization levers when repeated same-workload evidence
  shows a benefit. Verify their clean, incremental, test, release, CI, and
  published-package effects separately rather than assuming one profile serves
  every workload.
- Use narrow package/target/feature commands for inner-loop evidence while
  retaining the owning PR's relevant broader workspace, all-target, or
  all-feature correctness gates before handoff.
- Do not solve repository build cost by pausing analyzers, prescribing a local
  cache wrapper, hard-coding machine-specific target locations, reproducing the
  local shim's lane policy, or serializing independent developer operations.

## Benchmark and regression gate

- Use sanitized realistic small, current, large, and 10x corpora with skewed
  projects, long sessions, many worktrees, incremental edits, no-op refreshes,
  cold starts, warm steady state, concurrent clients, and sustained ingestion.
- Exercise Linux and Windows production code paths. Record platform-specific
  exclusions explicitly; one platform's improvement cannot hide regression on
  another.
- Include crash/restart, daemon reconnect, WAL/checkpoint interruption,
  projector replay, generation publication, cache loss, cancellation, and
  overload while load is active.
- Include concurrent conflicting overlays, bridge reconnect, upstream analyzer
  crash/restart, clean diagnostic cache hits, and no-op LSP sessions.
- Publish concise aggregate benchmark artifacts through the existing
  observability contracts. No private corpus, prompt, source payload, separate
  telemetry database, benchmark service, or performance-only product path.
- Gate material regressions in p95/p99 latency, throughput, memory, CPU, disk,
  write amplification, no-op work, and startup/recovery time using reviewed
  workload-specific thresholds rather than one universal score.
- Gate material regressions in representative same-host clean, warm
  incremental, no-op, and focused-test compilation. Reuse a matching PR7–PR19
  baseline where one exists and establish a PR20 baseline before optimization
  otherwise. Publish the command and workload identity with the result; do not
  turn one developer machine's absolute duration into a cross-platform limit.

## Done

PR20 is complete when measured production bottlenecks across database, sync,
projection, indexing, query, and repository-controlled developer builds have
bounded implementations; realistic Linux and Windows comparisons meet reviewed
regression gates; crash/restart and concurrency tests remain correct; and no
optimization weakens product semantics, privacy, scope, durability, coverage,
ordering, or daemon authority.
No LSP process-sharing or cache optimization may trade correctness or privacy
for lower process count or resource use.
