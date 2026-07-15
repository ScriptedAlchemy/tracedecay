# PR20: End-to-end performance optimization

**Status:** committed V2 delivery after PR19 convergence.

**Depends on:** [02 store](02-store-crate.md), [04 projectors](04-projectors-crate.md),
[05 query](05-query-crate.md), [25 code indexing](25-code-intelligence-indexing-crate.md),
[12 migration/cutover](12-root-compatibility-migration.md),
[19 convergence](19-system-defragmentation-convergence-and-extensibility.md),
and [26 observability](26-observability-accounting-and-usage.md).

## Outcome

PR20 measures and optimizes the production database, synchronization,
projection, indexing, and query paths as one system. It preserves exact product
semantics, privacy, authority, durability, coverage, ordering, and recovery.
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
- Report peak and steady memory, CPU time/utilization, database and generation
  bytes, temporary space, bytes read/written, and write amplification.
- Separate queue, lock, I/O, parse, projection, model, merge, hydration, and
  rendering time where the production trace can attribute them safely.
- Compare baseline and candidate with repeated runs and visible variance.
  Missing, partial, sampled, capped, or noisy evidence cannot claim a win.

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
- Publish concise aggregate benchmark artifacts through the existing
  observability contracts. No private corpus, prompt, source payload, separate
  telemetry database, benchmark service, or performance-only product path.
- Gate material regressions in p95/p99 latency, throughput, memory, CPU, disk,
  write amplification, no-op work, and startup/recovery time using reviewed
  workload-specific thresholds rather than one universal score.

## Done

PR20 is complete when measured production bottlenecks across database, sync,
projection, indexing, and query have bounded implementations; realistic Linux
and Windows comparisons meet reviewed regression gates; crash/restart and
concurrency tests remain correct; and no optimization weakens product semantics,
privacy, scope, durability, coverage, ordering, or daemon authority.
