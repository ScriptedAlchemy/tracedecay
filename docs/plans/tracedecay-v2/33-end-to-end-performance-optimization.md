# PR20: End-to-end performance optimization

**Status:** committed V2 delivery after PR19 convergence.

**Depends on:** [02 store](02-store-crate.md), [04 projectors](04-projectors-crate.md),
[05 query](05-query-crate.md), [25 code indexing](25-code-intelligence-indexing-crate.md),
[12 migration/cutover](12-root-compatibility-migration.md),
[19 convergence](19-system-defragmentation-convergence-and-extensibility.md),
[26 observability](26-observability-accounting-and-usage.md), and
[35 daemon LSP gateway](35-daemon-lsp-gateway-and-universal-diagnostics.md),
[24 task/work graph](24-canonical-task-plan-graph-and-multi-agent-executor.md),
and [32 workflow runtime](32-dynamic-workflow-runtime-and-sdk.md).

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

- Before tuning or publishing results, review and freeze a concise versioned
  measurement record per path containing the supported decision and estimand,
  population/unit, baseline and candidate builds, corpus/generation and
  environment/oracle digests, platform/hardware class, cache preparation,
  arrival model (`open` or `closed` loop), distribution and bursts, scheduled
  arrival timestamp, timeout/retry/think-time policy, harness/clock revision,
  sample count, balanced/randomized run order, A/A noise floor, uncertainty
  method, stopping/outlier rule, named strata/support, and practical regression
  margin. Changing these inputs creates a new named comparison; this remains an
  artifact in existing observability, not a benchmark service or leaderboard.
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
- Measure PR17 work-graph event commit and projector lag, dependency/readiness
  recomputation, cross-project relation hydration, Kanban/DAG/timeline/causal/
  workload query and render latency, Plan 24-to-Plan 32 admission and
  cancellation/recovery latency, lease/attempt churn, model-routing evidence
  aggregation and recommendation latency, task-shape/impact feature
  extraction, decomposition comparison, independent-review/outcome
  attribution, calibration rebuild, live split/merge/resize/re-route proposal,
  abstention, policy fallback cost, auxiliary provider discovery/negotiation,
  lease-to-process/session start, context assembly, structured event ingestion,
  first heartbeat/progress, stdout/stderr/artifact throughput, cancellation and
  kill escalation, terminal receipt, reconnect/resume, and restart recovery.
  Measure native Claude Code CLI, Codex app-server, and explicit Codex CLI
  fallback separately. Report graph size/edge density/history horizon, selected
  scope, feature/evidence/cohort/censoring coverage, exact executable/protocol/
  model-version cardinality, concurrent executors, candidate/proposal count,
  stream/artifact volume and coverage, and view/result cardinality.
- Report peak and steady memory, separating anonymous/file-backed RSS, live
  heap, allocation churn, retained/fragmented bytes, SQLite cache, result/
  queue/generation bytes, and profiler overhead where supported, plus CPU
  time/utilization, database and generation bytes, temporary space, bytes
  read/written, and write amplification.
- Separate queue, lock, I/O, parse, projection, model, merge, hydration, and
  rendering time where the production trace can attribute them safely.
- Compare baseline and candidate with paired relative effect sizes,
  confidence/credible intervals, A/A noise floors, and predeclared practical
  margins. Randomize or interleave run order where valid and retain raw run
  aggregates. A p-value or point estimate alone never gates promotion.
  Missing, partial, sampled, capped, survivor-biased, or noisy evidence cannot
  claim a win.
- Developer-build workloads use stock Cargo commands with an explicit package,
  target, feature set, test target, toolchain, and fixed edit class: clean,
  exact no-op, private leaf/body, public signature/type, macro/proc-macro
  input, build-script/generated asset, feature/dependency/manifest, or
  integration-test edit. Record wall time, CPU time/utilization, peak memory,
  rebuilt/reused units, critical path, codegen/link time, build-script
  execution, and cache outcome when the toolchain exposes them.
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
- Daemon admission is class-aware and measured. Reserve capacity for
  health/doctor/diagnostics traffic so bulk load cannot make the daemon
  unobservable; report connection counts, admission latency, and per-class
  shed/reject rates under multi-fleet concurrency. PR7 dogfooding measured
  hundreds of capacity-shed events in minutes with diagnostics among the shed
  traffic; that transport-level shedding shape is a regression, not a bound.
- Bound per-host daemon connection counts through client-side multiplexing;
  many short-lived tool processes must not each cost a daemon socket.

### Projection, indexing, and caches

- Recompute only changed observations, files, symbols, dependents, documents,
  and vectors justified by versioned dependency evidence.
- Reuse compatible immutable generations and caches by complete content,
  schema, grammar/model, privacy, scope, and configuration identity.
- Distinguish OS page cache, SQLite page cache/mmap, prepared-statement/
  connection cache, immutable application/generation cache, and model/vector
  cache. “Cold” and “warm” are invalid labels without the exact per-layer
  preparation protocol.
- Every maintained view defines signed insert, update, delete, and retraction
  deltas plus watermark identity. Compare full, incremental, and batched
  recomputation across change fraction, fan-out, read/write ratio, state bytes,
  and freshness. Mixed deltas must equal clean recomputation; incompatible or
  over-break-even frontiers use explicit bounded recomputation rather than a
  new general incremental-view engine.
- Bound cache memory and disk, define admission/eviction and idle lifecycle,
  delete superseded generations only after authority and recovery checks, and
  prevent rebuild storms or mixed-generation reads.
- One-shot backfills and repairs are marker-gated: ensure/open paths perform
  bounded no-op work on every start, never a repeated full-table scan (a PR7
  open-path backfill re-scanned two tables on every startup until gated).
- Resolve store handles and application state once per authority scope and
  reuse them; per-request database open or schema-ensure on hot routes is a
  regression class, not an implementation choice.
- Cancellation, disk-full, stale input, and concurrent rebuilds publish one
  complete verified generation or leave the prior generation authoritative.

### Database query performance

General discipline for every SQL surface, not only the search path:

- Review `EXPLAIN QUERY PLAN` for each measured hot statement; full scans and
  temp b-trees on hot paths need either a supporting index or a recorded
  justification. Indexes are added and removed from measured evidence, and
  each shipped index names the statements it serves.
- Batch per-row lookups: a page or collection assembled by issuing one query
  per element is a named regression class (PR7's fact-list path hydrated each
  projection with a separate per-id query). Prefer one set-based statement,
  `IN`/join pushdown, or a single pass over a cursor.
- Reuse prepared statements and connections on hot paths; per-request
  prepare, open, or schema-ensure work is the regression class recorded
  above.
- Push filters, ordering, and limits into SQL rather than fetching wide and
  filtering in Rust; pagination executes as indexed range scans on the cursor
  ordering, never OFFSET walks.
- Multi-join projection counts (e.g. status backlog counts) carry measured
  budgets; when a count is hot and its joins are stable, maintain it
  incrementally instead of recomputing the join per read.
- Durable-write paths batch fsyncs deliberately: group commits within one
  transaction where atomicity allows, and never issue per-item sync in a loop
  a single transaction could cover (PR7's spool measurements: wall time was
  `items x ambient fsync latency` under load).

### Query execution

- Use measured selectivity and costs to prune shards/candidates, avoid repeated
  hydration/parsing, reuse compatible prepared or derived state, and stop work
  at declared budgets and cancellation boundaries.
- Preserve deterministic order, exact-match tiers, temporal truth, stable
  cursors, coverage, explanations, and lexical fallback byte-for-byte where
  their owning contracts require it.
- Cache raw extraction separately from cohort normalization and optional
  calibrated outcome models; invalidate by source, edge, descriptor, cohort,
  and model digest. Exact vector scan remains the oracle and a valid production
  candidate; ANN is admitted only after a measured break-even and must report
  average/tail/minimum recall and zero-recall queries. Expensive community or
  centrality views are asynchronous with explicit stale/partial state.
- Bound cross-project fan-out, graph traversal, reranking, result buffering,
  and per-client concurrency with explicit partial or unavailable coverage.
- Incremental task-readiness, critical-path, history, and workload projections
  must avoid full-graph rebuilds on one event while preserving deterministic
  results. Kanban is not a separate cache or query engine, and route
  recalibration cannot block lease heartbeats, cancellation, or deterministic
  fallback.
- Task-shape, model-capability, outcome, and calibration projections update
  only affected work/cohort/version horizons. Live proposal generation is
  coalesced, bounded, cancellable, deduplicated by pinned input digest, and
  lower priority than lease heartbeats, explicit runtime controls, and
  deterministic fallback. Optimization cannot coarsen model-version identity,
  omit censored/negative outcomes, hide coverage, or change a reviewed
  estimator's result.
- Provider-adapter optimization preserves typed argv/stdin, capability
  negotiation, exact executable/protocol/model identity, sandbox/approval/
  environment boundaries, ordered structured events, cancellation escalation,
  lease fencing, and resume proof. Process pooling or app-server reuse requires
  exact scope/privacy/configuration identity and cannot retain another
  attempt's context or secrets. Lower startup latency never permits hidden
  CLI fallback, shell execution, PID-only adoption, dropped terminal outcomes,
  or recursive auxiliary dispatch.

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
- Learned indexes, allocator changes, crate splits, and concurrency policies
  from individual papers remain experiments. Promotion requires same-host,
  workload-stratified evidence plus correctness and recovery parity; no paper
  headline or benchmark rank selects a production mechanism.

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
- Open-loop overload fixtures measure latency from scheduled arrival and report
  offered, admitted, started, completed, cancelled, timed-out, shed, and
  retried counts, queue age, saturation, recovery, and survivor bias so the
  harness cannot hide coordinated omission.
- Include large multi-repository task DAGs, deep/fan-out/fan-in dependencies,
  long attempt history, overlapping saved projections, stale lease receipts,
  route-policy version changes, sparse/shifted model cohorts, bounded
  exploration, human overrides, and deterministic fallback under load.
- Include missing/stale provider executables, executable/protocol/model
  upgrades, native Claude Code and Codex fake/native streams, app-server
  saturation and reconnect, explicitly allowed CLI fallback, malformed and
  oversized output, missing heartbeats, cancellation/kill escalation, daemon
  restart/resume, secret canaries, and concurrent auxiliary attempts across
  isolated worktrees.
- Include concurrent conflicting overlays, bridge reconnect, upstream analyzer
  crash/restart, clean diagnostic cache hits, and no-op LSP sessions.
- Publish concise aggregate benchmark artifacts through the existing
  observability contracts. No private corpus, prompt, source payload, separate
  telemetry database, benchmark service, or performance-only product path.
- Gate material regressions in p95/p99 latency, throughput, memory, CPU, disk,
  write amplification, no-op work, and startup/recovery time using reviewed
  workload-specific practical margins and intervals rather than one universal
  score, default threshold, or transferred paper result.
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
