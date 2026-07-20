# PR20: End-to-end performance optimization

**Status:** committed V2 delivery after PR19 convergence.

**Depends on:** [02 store](02-store-crate.md), [04 projectors](04-projectors-crate.md),
[05 query](05-query-crate.md), [25 code indexing](25-code-intelligence-indexing-crate.md),
[12 migration/cutover](12-root-compatibility-migration.md),
[19 convergence](19-system-defragmentation-convergence-and-extensibility.md),
[15 search quality](15-search-quality-evaluation-and-retrieval-research.md),
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

## Current evidence and execution ledger

This ledger records the 2026-07-20 optimization audit and the implementation
decisions it triggered. Its three states are intentionally different:

- **Completed evidence** is a reproducible observation, not proof that the
  measured implementation is optimal.
- **Active near-term change** is implementation in the shared dirty V2
  worktree with focused evidence. It is not accepted until its aggregate
  all-feature and crash/restart gates pass on the reconciled tree.
- **Unaccepted PR20 candidate** is a research question whose mechanism and
  promotion margin remain open. Point estimates below prioritize measurement;
  they never select or promote a mechanism.

Changing a workload, tree, profile, page-cache state, concurrency, or corpus
creates a different comparison. The values below therefore remain attached to
their named artifacts or capture times and are not universal budgets.

### Completed audit and baseline evidence

- The repository audit snapshot contained 1,726 files, 72,432 nodes, and
  169,633 edges. The audited daemon used 12.52 GB RSS against a 374 MB graph
  database and 31.1 MB of indexed source: 33.5 times database bytes and 402
  times source bytes. A later live recapture at Unix time `1784513507`, while
  concurrent PR9 work had advanced the graph to 74,015 nodes and 172,589
  edges, measured 12,149,280,768 RSS bytes, 382,853,120 database bytes, and
  31,640,255 source bytes. The captures are separate strata; their drift is not
  a before/after effect.
- The 90-day audit contained 60,161 tool calls and 908 errors. Static
  source-to-test attribution covered 47 percent, leaving 5,468 reachable
  functions unattributed. These are coverage and prioritization evidence, not
  error-rate or test-quality targets.
- Session ingestion had 148 pending transcripts totaling 482,010 bytes. At
  the `1784513507` capture, last ingest progress was Unix time `1782983523`,
  about 17.7 days earlier, and transcript retrieval returned typed service
  unavailability. This is the baseline for the active stall-observability and
  fast-rejection change.
- The exact live file-descriptor capture for daemon PID 3465736 found 195
  numeric descriptors against a soft and hard `RLIMIT_NOFILE` of 1,048,576
  (0.019 percent). System allocation was 25,408 of 2,097,152
  (1.21 percent). Numeric descriptors included 60 database files, 9 WAL files,
  7 SHM files, 6 Unix sockets, 31 inotify instances with 833 watches, and zero
  deleted-open files. Twelve 10-second samples stayed between 195 and 196.
  This rules out immediate `EMFILE` pressure and an idle-growth leak in that
  two-minute window; it does not establish per-store high-water budgets under
  load.
- The accepted PR5/PR6 provider-observation artifact
  `benchmarks/pr5-observation/result-2026-07-16-00d3d73a.json` pins clean
  commit `00d3d73a06403480487207986506f9b3c4d1df43`, 3 warmups, and 30
  repetitions of 64 records. Pipeline p50/p95/p99 was
  623,357,379/644,226,286/646,779,174 ns at 102.786 records/s, with
  57,244 KiB peak RSS, 335,515,648 process-write bytes, and 139,818,560
  database-growth bytes. Exact no-op retry plus bounded replay had
  204,611/223,992/228,572 ns p50/p95/p99 and zero CPU, process-write bytes,
  database growth, observation delta, and coordinator work.
- PR7 memory evidence remains explicitly provisional because its subject tree
  is dirty and has no clean-commit attestation. In the latest provisional
  artifact, anchor creation over 8 records had
  54,467,633/73,394,773/312,921,791 ns p50/p95/p99 and wrote 26,640,384
  process bytes for 240 records. The p99 tail and write volume justify a PR20
  attribution experiment; they are not an accepted regression threshold.
- The PR9 packet `benchmarks/pr9-code-index/result-provisional.json` is also a
  Linux-only provisional baseline: page-cache state is uncontrolled, with 5
  warmups and 30 measured closed-loop repetitions. Current-scale clean
  indexing had 192,910,680/204,016,239 ns p50/p95 for 9 files and 1,505
  chunks; current no-op had 57,364,959/69,893,548 ns, parsed zero files,
  reused all 1,505 chunks, made zero projection calls, and reported zero
  process read/write bytes. At 10x, clean p50/p95 was
  2,057,220,764/2,104,263,140 ns with 232,378,368 p95 RSS bytes; a warm
  one-file edit was 800,224,697/833,982,443 ns with 1 file parsed and 15,049
  chunks reused; no-op was 716,573,291/748,836,732 ns with zero files parsed
  and zero projection calls; incompatible rebuild was
  2,591,553,329/2,681,591,433 ns with 353,034,240 p95 RSS bytes. PR9 owns the
  deterministic extraction/chunk/generation semantics and records this
  baseline; PR20 may compare implementations without changing PR9 or PR10
  retrieval semantics.

### Active near-term changes and acceptance

The following packets fix measured waste before PR20 rather than deferring an
obvious problem. Plan 26 owns their measurement/event schemas. The named
product plan owns behavior, and Plan 33 consumes accepted comparisons.

1. **Same-worktree daemon sharing, fair admission, read coalescing, and
   telemetry.** Owner: Plans 09/21 daemon and MCP runtime. Dependencies:
   Plan 02 single-writer store authority, Plan 20 resolved configuration, and
   Plan 26 telemetry. The owner preserves the existing physical engine key per
   canonical graph store and worktree generation, keeps
   per-client admission leases lightweight, reserves health traffic, and
   coalesces only identical in-flight reads keyed by graph database, authorized
   scope, tool, and arguments. The implementation adds an eight-request
   per-client bulk cap and retained/coalescing telemetry; it never shares
   across scope, privacy, profile, branch/worktree generation, authorization,
   or incompatible configuration. Focused fairness and sharing tests have
   passed, but candidate RSS and the aggregate gate have not. Acceptance:
   `cargo test --lib per_client_`,
   `cargo test --lib read_coalescing`, then
   `cargo check --all-features`.
2. **LCM status batching.** Owner: Plan 23 session/LCM query. Dependencies:
   Plan 02 store authority and Plan 21's MCP session binding. The owner
   replaces per-provider N+1 status work with four
   provider-independent status queries and one payload-health scan while
   preserving deep/shallow, missing-store, JSON, and Markdown output.
   Acceptance: `cargo test --lib aggregate_status_`,
   `cargo test --test session_suite lcm_query::status`, and
   `cargo check --all-features`.
3. **Stalled retrieval observability and fast failure.** Owner: Plan 23
   temporal refresh with Plan 09 daemon orchestration. Dependencies: Plan 14
   health semantics and Plan 26 coverage events. The owner exposes last
   progress, backlog, blocker, retry class, and typed unavailable reason, and
   rejects unavailable reads before expensive retrieval. Heartbeats do not
   synthesize progress, and restart/idempotency/privacy semantics remain
   unchanged.
   Acceptance: `cargo test --lib refresh_worker_`,
   `cargo test --features test-transport --test mcp_suite message_search`, and
   `cargo check --all-features`; the unavailable path must remain below its
   focused 100 ms test bound.
4. **Redundancy fingerprint batching.** Owner: Plan 05 query execution and the
   existing fingerprint store. Dependencies: Plan 15 result-quality contracts
   and Plan 21 binding parity. The owner performs one candidate-bounded bulk
   read, validates exact source spans, and parses only misses. Result ordering,
   thresholds, coverage, and bytes remain unchanged, so this does not alter
   PR10 semantic retrieval or ranking. Acceptance:
   `cargo test --lib fingerprints`,
   `cargo test --features test-transport --test mcp_suite redundancy`, and
   `cargo check --all-features`, with cold/partial/warm call-count and
   1,024-candidate work-proxy parity.
5. **Safe post-open FTS repair.** Owner: Plan 02 store/lifecycle. Dependencies:
   daemon single-writer maintenance authority and Plan 14 typed health/
   recovery. The owner classifies FTS-only corruption after open and schedules
   the existing rebuild through exclusive daemon writer authority. It never
   rebuilds or replaces a whole-database corruption and cannot race a writer.
   Acceptance:
   `cargo test --test storage_suite corruption_test::`, then
   `cargo check --all-features`; search parity, concurrent-writer
   serialization, and whole-database preservation are mandatory.
6. **Affected-test frontier batching and ranking.** Owner: Plan 25's PR9
   code/Git adapter. Dependencies: Plan 36 native Git evidence and Plan 15
   deterministic retrieval coverage. The owner performs one query per
   breadth-first frontier and adds deterministic direct/near recommendations
   while retaining the exact exhaustive sorted `affected_tests` set. Plan 36
   remains native Git authority, and PR9/PR10 retrieval semantics do not
   change. Acceptance:
   `cargo test --lib affected_traversal`,
   `cargo test --features test-transport --test mcp_suite affected_`, and
   `cargo check --all-features`, including central daemon/MCP set-parity and
   deterministic-rank fixtures.
7. **Dispatch decomposition.** Owner: Plan 21 surface dispatch. Dependencies:
   the daemon admission/coalescing packet above and Plan 26 accounting parity.
   The CLI half has reduced `dispatch_command` from 394 to 12 lines and from 58
   to 8 branches through private command-family routers without changing
   parsing, output, or exit behavior. The MCP `handle_tools_call` half remains
   active and must
   preserve authorization, fair admission, coalescing, analytics, staleness
   sync, token accounting, errors, notifications, and hot-swap semantics.
   Acceptance: `cargo test --bin tracedecay dispatch_`,
   `cargo test --features test-transport --test mcp_suite`, and
   `cargo check --all-features`, plus the source complexity guards.

The reconciled near-term packet has one final gate:

```text
cargo check --all-features
cargo test --all-features
```

Focused passes from an unreconciled shared worktree are useful implementation
evidence but cannot substitute for this aggregate result.

### Unaccepted PR20 candidates and evidence gates

- **Retained-memory attribution:** Plan 33 owns the experiment and Plan 26 owns
  resource observations; daemon/store/index/cache owners expose attributed
  bytes. Freeze cold, warm, idle, concurrent-client, and post-eviction
  workloads and measure process-tree RSS/PSS, anonymous/file-backed RSS, live
  heap, allocator retention/fragmentation, SQLite page cache/mmap, immutable
  graph generations, result/coalescing buffers, queues, watchers, and profiler
  overhead. No allocator, cache, pool, or eviction change is admissible until
  these components explain the observed process high-water and a paired
  candidate interval clears its margin.
- **Observation write amplification:** the PR5 owner supplies the accepted
  observation workload and exact no-op oracle. PR20 first attributes statement,
  WAL, checkpoint, fsync, projection, and receipt bytes; only then may it test
  bounded transaction, statement, or checkpoint candidates. Exact no-op zero
  writes, provider fairness, atomic cursor/receipt/projection state, and
  crash/restart replay are hard guards.
- **Anchor-persistence tails:** the Plan 02/13 memory-and-anchor owners first
  recapture PR7 from a clean attested tree and attribute statements, lock hold,
  WAL/fsync, payload, and retrieval-anchor work. A batching or index candidate
  is eligible only if alias, cursor, idempotency, deletion, and restart
  semantics are identical and paired p95/p99 plus write-byte intervals clear
  frozen margins.
- **Build/test graph cuts:** Plan 19 owns physical boundary convergence and
  each touched product plan owns its dependencies; PR20 owns same-workload
  measurement. Freeze stock-Cargo clean, exact no-op, private-leaf,
  public-signature, macro/build-script, feature/manifest, and focused-test edit
  classes before testing crate, feature, dependency, or integration-test
  target cuts. Record rebuilt units and critical path. No machine-local target,
  lane, cache, shim, or Rust Analyzer policy enters the product plan.
- **Request-dispatch follow-through:** Plan 21 owns transport behavior. After
  the active structural split lands, profile authorization, admission,
  staleness sync, tool execution, rendering, analytics, and notification
  stages separately under mixed read/write/control load. Further caching or
  bypass is ineligible unless byte-identical responses, stable errors, token
  accounting, cancellation, fairness, hot swap, and crash/restart behavior
  remain exact.
- **File-descriptor budget:** the exact idle snapshot above establishes low
  current limit pressure and no two-minute idle-growth leak. It does not
  justify raising limits or changing pools. Before any FD optimization, freeze
  same-worktree and multi-worktree idle/load/restart/connect-disconnect
  workloads and report numeric descriptors and high-water by canonical store,
  DB/WAL/SHM pool, watcher, socket, payload/model/artifact, and deleted-open
  class. Promotion additionally requires stable post-drain counts, reserved
  health/recovery capacity, and injected `EMFILE` recovery without authority
  loss. Without exact class attribution and paired evidence, disposition is
  `insufficient_evidence`.

Every candidate above uses the measurement contract below: A/A noise first,
paired relative effects and intervals, protected worst strata/tails/resources,
Linux and Windows, and crash/restart correctness. There is no combined
performance score.

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

### Concrete measurement types and instrumentation

[Plan 26](26-observability-accounting-and-usage.md) owns
`PerformanceMeasurementDescriptorV1`, `BenchmarkRunAggregateV1`,
`PairedEffectEstimateV1`, `OperationResourceObservedV1`,
`NoProgressObservedV1`, `BenchmarkAttestationV1`, `EvidenceGradeV1`, and
`PerformanceDispositionV1`. This plan sets and evaluates path-specific budgets
through those types; it does not fork their schema. Plan 15 remains authority
for relevance labels and search-quality promotion, Plan 24 for work/outcome
identity and legal transitions, and Plan 32 for actual scheduling, leases,
effects, cancellation, and runtime receipts.

`PerformanceMeasurementDescriptorV1` freezes the operation and stratum,
supported decision, estimand, population/unit, `BaselineCapture |
CandidateComparison` kind, subject tree identity, optional accepted-baseline
attestation reference required only for a candidate comparison, workload/
corpus/environment/oracle/configuration/harness/clock digests, platform and
hardware class, every cache-layer preparation, open- or closed-loop arrival
process, bursts/concurrency, timeout/retry/think-time policy, sample and
independent-block floors, quantile algorithm, randomized AB/BA order for
candidate comparisons, A/A noise method, interval estimator, stopping/
exclusion rule, protected strata, coverage and expected-tail-support floors,
practical margin, and correctness/resource guardrails before candidate
results are visible. Neither a missing numeric baseline nor a not-yet-measured
path is filled with a sentinel. A baseline capture can be admitted without a
prior baseline; a candidate comparison cannot.

The root span is `tracedecay.operation`. Required closed child
`SpanStageV1` values are `AdmissionQueue`, `StoreLock`, `IndexLock`, `Io`,
`Parse`, `Projection`, `Model`, `Rank`, `Merge`, `Hydration`, `Synthesis`,
`Render`, `Persist`, `ProviderDiscovery`, `ProviderNegotiation`,
`ProviderLeaseToStart`, `ProviderContextAssembly`, `ProviderEventIngestion`,
`ProviderFirstProgress`, `ProviderCancellation`, `ProviderTerminal`,
`ProviderReconnect`, and `ProviderResume`. Repeated spans are accumulated with
count and min/max/sum into at most 32 rows. Attributes are closed operation,
scope, queue/lock, requested/actual cataloged provider/backend/model/protocol,
outcome, revision, and coverage values. Paths, prompts, source, symbols,
argv/stdin, provider output, environment, errors, and secrets are forbidden.

`OperationResourceObservedV1` reports scheduled-arrival-to-terminal and service
latency; offered/admitted/started/completed/cancelled/timed-out/shed/retried
counts and rates; p50/p95/p99 and throughput; queue age and saturation;
baseline, peak, and steady process-tree RSS/PSS, anonymous/file-backed RSS,
and separately named container high-water evidence; live heap, allocation
churn, retained/fragmented, SQLite-cache, queue/result/generation bytes;
user/system CPU and core-seconds; database/generation/temporary space, bytes
read/written, and write amplification; input/output/reasoning/cache-read/
cache-write tokens; cost/currency/pricing revision; and attempted, committed,
reconciled, unknown, prevented-duplicate, and retried effects. Tokens and costs
state `ProviderReported | LocallyMeasured | Estimated | NotApplicable |
Unknown`. Estimated required resource evidence makes an attestation
provisional; unknown is never zero.

Linux process-isolated runs report process-tree RSS and PSS, plus cgroup v2
`memory.peak` separately under that exact name; cgroup peak is never labeled
RSS. The cgroup is recreated per phase and the manifest freezes page-cache and
swap policy. Windows uses the manifest's equivalent process-tree RSS/PSS and
job/container high-water methods; other platforms declare equivalent
boundaries or mark the dimension partial. The 100 ms samples describe baseline
and steady-state RSS/PSS shape, with steady state the median after the warm-up
frontier and before drain. Profiler-on/off A/A pairs quantify overhead.
Missing child-process coverage prevents a memory promotion claim.

Instrumentation enters through
`src/application/observability/{record,performance}.rs` and
`src/runtime_telemetry.rs`; the canonical store/projector and query boundaries
are `crates/tracedecay-store/src/observation/telemetry.rs`,
`crates/tracedecay-store/src/observation/telemetry_projection.rs`, and
`src/application/observability/query.rs`. Owning paths add spans at
`src/application/retrieval/pipeline.rs`,
`src/query/retrieval/{exact,lexical,semantic,graph,temporal,task_session,diagnostic,fusion,dedupe,diversity,rerank,hydrate}.rs`,
`src/daemon/transport.rs`, `src/daemon/scheduler.rs`,
`src/automation/runner.rs`, and the Plan 32 workflow/provider modules. The
instrumentation path is bounded and non-blocking. Each producer maintains a
saturating in-memory atomic drop count plus one reserved control-lane slot;
the next accepted envelope and shutdown flush carry the count, and a
`TelemetryDropObservedV1` uses the reserved slot. The drop signal never depends
on capacity in the full data lane and is not another durable counter store.

### Retrieval, planner, and outcome performance

Each Plan 15 query comparison reports per canonical `RetrieverKind`
requested/consumed
candidate budget, raw/eligible/deduplicated/returned count, fixed rank buckets,
unique and final-top-k contribution, source freshness/coverage/denial,
retrieval/rank/model duration, and labeled marginal Recall@K/nDCG@10 where an
oracle exists. Every metric and promotion guardrail is also stratified by
Plan 26 `RetrievalQueryFamilyV1`, with `Unknown` shown separately and never
pooled into a passing family. Planner selection/queue, requested/admitted/deferred fan-out,
fan-out wait, merge/dedupe/rerank/hydration/render/synthesis, critical-path,
and total latency are separate spans. Exact, lexical, graph, temporal,
task/session, diagnostic, and semantic lane ablations receive the same frozen
total candidate budget; unused budget is not silently reassigned. Reranker
off/on is a separate composition-stage ablation over byte-identical saved
pre-rerank candidates. Candidate oracle Recall@N is reported before reranker
quality, and exact flat-vector scan remains the ANN oracle. Denied candidates
produce no count, rank, trace, cache, or aggregate influence; denial telemetry
is operation-level only.

Context comparisons pin the same work/acceptance identity and report required,
included, independently verified relevant, irrelevant, stale, truncated, and
unknown authorized anchors plus operation-level denial without candidate or
anchor cardinality; Precision@1/3/5 and required-anchor coverage;
context bytes/tokens; assembly latency; time to first valid action; rediscovery
reads/searches/tests/tokens; independently accepted correctness; rework; and
unknown/censored outcomes. Plan 24 packet count and token/byte distributions
use Plan 26's fixed buckets by closed work class and fixture-size stratum. The
required ablations are no-context/no-auxiliary,
bounded retrieval manifest, handoff/recall enabled, and the production
profile. They preserve Plan 24 first-pass and parent-normalized identity and
never infer quality from Plan 32 `Completed`, low latency, token reduction, or
worker self-report.

Plan 24 planner and Plan 32 runtime measurements record requested and actual
topology, route, fan-out, concurrency, provider/backend/model, queue, effect,
and terminal state separately. A scheduler speedup that increases rejected,
unknown, censored, duplicate-effect, review, integration, or rework outcomes
is a regression even if completed-attempt throughput rises.

### Quantile, threshold, and no-progress methodology

One paired baseline/candidate block is the experimental unit. Run order is
randomized or interleaved AB/BA on the same host and prepared state. The
empirical quantile algorithm and paired cluster-bootstrap 95% interval freeze
in the descriptor; requests remain clustered within their run. Report
eligible, observed, completed, each terminal outcome, independent block count,
and `expected_tail_support = terminal_observations * (1 - quantile)`.

A quantile may gate only with at least 20 independent paired blocks and 100
expected observations in its tail: p50 therefore needs at least 200 terminal
observations, p95 2,000, and p99 10,000 per protected stratum. Lower support
may be reported with `insufficient_evidence` but cannot pass or fail a
quantile gate. The descriptor also freezes a maximum interval half-width and
requires the last two predeclared checkpoints to change that half-width by no
more than 10%; otherwise evidence is provisional. Tail observations must span
all 20 blocks and no block may contribute more than 10% of tail support.
Every offered request enters
the scheduled-arrival-to-terminal distribution at its actual completion,
cancellation, timeout, shed, or failure timestamp. Retries are separate linked
attempts. Cancellation, timeout, shed, retry, failure, unknown, and censoring
rates each have frozen non-inferiority guardrails, so a candidate cannot win by
discarding its slowest work. Predeclared exclusions retain reason and count.

Each descriptor runs A/A before A/B. The frozen regression margin is
the owner-predeclared practical margin; measurement never enlarges it. A/A is
eligible only when `2 * p95_absolute_AA_paired_effect` is no greater than that
margin. Otherwise the run is provisional and `insufficient_evidence`. A
claimed improvement requires its paired 95% interval to clear the frozen
improvement margin. Every guardrail passes only when its one-sided harm bound
stays within the frozen regression margin. A p-value, point estimate,
aggregate mean, or transferred external threshold never gates.

Plan 32's `MonotonicRunDeadline`,
`ConcurrencyPolicyV1.no_progress_timeout`, and `ProgressFrontier` are the only
workflow deadline and stall authority. PR20 measures queue wait, remaining
monotonic budget, configured no-progress timeout, last committed frontier,
stall duration, timeout, and cancel/interrupt/terminate/kill results without
changing them. A heartbeat alone cannot reset the timeout; unproved provider
termination remains `Partial` or `EffectUnknown`. Tests prove bulk work cannot
starve heartbeat, cancellation, Doctor, or diagnostics classes.

### Implementation, dashboard, and test map

The PR20 harness and checked-in protocol are:

- `benches/pr20_e2e_performance.rs` and
  `benches/pr20/{workloads,runner,metrics,attestation}.rs`;
- `scripts/run-pr20-performance-benchmark.sh`;
- `benchmarks/pr20-performance/README.md`,
  `benchmarks/pr20-performance/workload-v1.json`, and
  `benchmarks/pr20-performance/measurement-v1.json`;
- sanitized PR20 comparison output resolved by
  `BenchmarkArtifactLayoutV1::comparison_dir(comparison_id)` under
  `benchmarks/pr20-performance/results/`, containing exactly `README.md`,
  `workload-v1.json`, `attestation-v1.json`, `aggregate-v1.json`, and
  `evidence-index.json`; and
- authorized raw `manifest.json`, `runs.jsonl`, profiles, and private-oracle
  references resolved by
  `ProjectStoreLayout::benchmark_run_dir(BenchmarkSuiteId::EndToEndPerformance,
  attestation_id)`.

The Observatory and Costs backend views are
`src/dashboard/{observatory_api,costs_api}.rs`. The rendered views are
`dashboard/observatory/src/{Performance,Retrieval,Attestation}Panel.tsx` and
`dashboard/costs/src/CostsPage.tsx`. They show descriptor revision, baseline/
candidate and attestation identity, scope/stratum, p50/p95/p99 and support,
paired interval and frozen margin, queue/lock/provider spans, RSS/CPU/I/O,
tokens/cost source, effects, no-progress outcomes, coverage, and disposition;
they contain no client-side formula.

Direct suites are:

- `tests/performance_suite/measurement_contract.rs` for descriptor freeze,
  baseline lineage, quantile support, A/A, paired intervals, stopping, and
  exclusions;
- `tests/performance_suite/retrieval.rs` for per-retriever candidate/rank/
  contribution, source state, equal-budget ablations, and context outcomes;
- `tests/performance_suite/overload.rs` for scheduled arrivals, all terminal
  counts, coordinated omission, fairness, queue age, saturation, and recovery;
- `tests/performance_suite/resources.rs` for process-tree RSS/PSS, separately
  named cgroup `memory.peak` and platform container high-water evidence,
  CPU/I/O, tokens/cost provenance, profiler overhead, and effect accounting;
- `tests/performance_suite/provider_deadlines.rs` for progress frontiers and
  every cancel/interrupt/terminate/kill outcome;
- `tests/performance_suite/attestation.rs` for clean/provisional/rejected
  grading, digest mutation, raw lineage, threshold freeze, privacy, and
  supersession;
- `tests/performance_suite/runtime_rollback.rs` for rollout hold, fallback,
  effect reconciliation, and exact pinned
  `prior_accepted_profile_id`/revision restoration; and
- `tests/dashboard_api_test/{observatory,costs}.rs` plus
  `dashboard/test/{observatory,costs}.vitest.tsx` for value/coverage parity.

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
- Same-worktree clients share only the heavy engine state whose complete store,
  worktree-generation, scope, privacy, authorization, and configuration
  identity matches. Per-client fair admission, reserved control capacity,
  in-flight read coalescing, and lease-aware idle eviction remain separately
  observable.
- Descriptor admission uses measured per-class high-water and preserves
  health/recovery reserve. Raising `RLIMIT_NOFILE` is not an optimization and
  a low idle count cannot substitute for multi-worktree load and restart
  evidence.

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
- Post-open FTS repair is limited to proved FTS-only corruption, enters through
  exclusive daemon writer/maintenance authority, and preserves a corrupt
  whole database untouched for typed recovery.

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
- Aggregate status and fingerprint collection use set-based reads with
  deterministic output parity. LCM provider status computes payload health
  once; redundancy analysis bulk-loads exact-span fingerprints and parses only
  cache misses.

### Query execution

- Use measured selectivity and costs to prune shards/candidates, avoid repeated
  hydration/parsing, reuse compatible prepared or derived state, and stop work
  at declared budgets and cancellation boundaries.
- An optimization may reduce candidate work only when the frozen Plan 15
  oracle proves protected quality parity. Per-retriever candidate, rank,
  unique/final contribution, source freshness/coverage/denial, and equal-budget
  ablation evidence remains present after pruning; an uninstrumented retriever
  or planner phase is not an optimization candidate.
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
- Affected-test traversal batches each breadth-first frontier and retains the
  exhaustive set while presenting deterministic distance-ranked
  recommendations. Ranking metadata cannot remove or relabel an affected test.
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
- Command and request dispatch is decomposed by existing ownership boundaries
  before profiling; private helper extraction cannot change parsing, wire
  output, errors, accounting, authorization, admission, cancellation,
  notifications, or runtime behavior.

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
- Every published baseline or comparison carries one
  `BenchmarkAttestationV1`. `BenchmarkBaselineAttestationV1` can accept an
  independently measured clean subject without a prior baseline.
  `BenchmarkComparisonAttestationV1` must reference that accepted compatible
  baseline. `EvidenceGradeV1::Clean` requires immutable clean subject trees,
  verified workload/corpus/environment/oracle/harness/schema/configuration/
  threshold digests, raw aggregate lineage, required platforms/strata/support/
  coverage, acceptable A/A noise, frozen thresholds, and intervals. Clean
  evidence may still disposition `reject` when a candidate correctness,
  privacy, recovery, or performance gate fails.
- `EvidenceGradeV1::Provisional` is structurally valid and privacy-safe but has
  a dirty tree, missing required platform or confirmation cohort, insufficient
  tail support, excessive censoring/noise, estimated required resource
  evidence, or partial coverage. Its only disposition is
  `insufficient_evidence`; it cannot promote, reject, or establish a baseline.
- `EvidenceGradeV1::Rejected` identifies invalid evidence: placeholder or
  invalid digest, missing or fabricated baseline required by a candidate
  comparison, comparison-identity mismatch, absent raw lineage, post-result
  threshold change, coordinated omission/survivor bias, hidden protected
  stratum, or leakage from the measurement artifact itself. Rejected evidence
  never decides candidate quality, while an independently observed product
  safety violation still triggers the immediate rollback below.
- Gate material regressions in p95/p99 latency, throughput, memory, CPU, disk,
  write amplification, no-op work, and startup/recovery time using reviewed
  workload-specific practical margins and intervals rather than one universal
  score, default threshold, or transferred paper result.
- Gate material regressions in representative same-host clean, warm
  incremental, no-op, and focused-test compilation. Reuse a matching PR7–PR19
  baseline where one exists and establish a PR20 baseline before optimization
  otherwise. Publish the command and workload identity with the result; do not
  turn one developer machine's absolute duration into a cross-platform limit.

### Acceptance and rollback

Promotion requires one clean comparison attestation whose primary improvement interval
clears its frozen margin; every protected exact/no-answer/wrong-scope/stale/
privacy/language/repository/low-coverage and correctness/recovery stratum has
required support and coverage; every latency, throughput, RSS, CPU, disk,
PSS, separately named cgroup/container high-water, write-amplification, no-op,
startup/recovery, token/cost, and effect guardrail
stays within its one-sided harm margin; cancellation, timeout, shed, retry,
failure, unknown, and censoring rates stay within their frozen guardrails; and
Linux/Windows exclusions are explicit. If no compatible baseline exists, PR20
first records an accepted clean `BenchmarkBaselineAttestationV1` and starts a
separate candidate comparison; it never invents historical values.

`insufficient_evidence` holds the current implementation/profile and schedules
no automatic retry or rollout. A clean `reject` leaves an unpromoted candidate
disabled. For an activated optimization, rollback is immediate on semantic
divergence, privacy or authority violation, exact-tier demotion, wrong-scope
evidence, hidden provider fallback, duplicate observable effect, unreconciled
unsafe effect, secret canary, or deterministic-order/recovery failure.

`RuntimeRollbackPolicyV1` freezes non-overlapping 15-minute windows, resets
only on a profile activation epoch, and pins exact `candidate_profile_id`,
`control_profile_id`, `prior_accepted_profile_id`, profile revisions, baseline
attestation revision, and compatibility digest. The prior accepted profile is
the only rollback target; there is no "latest" ordering lookup. Side-effect-free
paths may use concurrent shadow control; other paths use policy-approved
deterministic canary/control assignment with the same workload strata and no
user, project, or payload label. A live performance
rollback requires three consecutive eligible matched windows, each meeting
the descriptor's support, independent-block, terminal-rate, and coverage
floors, whose one-sided harm interval exceeds the frozen margin. Without an
eligible control, rollout cannot continue: the canary reverts to the exact
pinned `prior_accepted_profile_id` and revision and records
`insufficient_evidence`. One breach of a separately configured hard resource/
deadline bound rolls back immediately.

Rollback activates only the exact pinned `prior_accepted_profile_id` and
revision after verifying the policy's compatibility digest,
preserves durable evidence, pins in-flight work until Plan 32 explicitly
fences or reconciles it, and never retries through `EffectUnknown`. Missing
telemetry, a full control lane, capped coverage, or excessive noise reverts an
active canary after one complete window; it is not proof of health. Threshold,
workload, or configuration changes create a new comparison and cannot
retroactively rescue a failed gate.

## Done

PR20 is complete when measured production bottlenecks across database, sync,
projection, indexing, query, and repository-controlled developer builds have
bounded implementations; realistic Linux and Windows comparisons meet reviewed
regression gates; crash/restart and concurrency tests remain correct; and no
optimization weakens product semantics, privacy, scope, durability, coverage,
ordering, or daemon authority.
No LSP process-sharing or cache optimization may trade correctness or privacy
for lower process count or resource use.
Completion additionally requires the concrete measurement, retrieval,
overload, resource, deadline, attestation, dashboard-parity, and rollback
suites above; clean Linux and Windows attestations for every promoted path;
and no provisional or rejected evidence presented as a baseline, improvement,
or release gate.
