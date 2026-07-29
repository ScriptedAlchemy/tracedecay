# PR20: End-to-End Performance Optimization

## Status / role

PR20 begins after PR19 convergence. It measures the production journeys shipped
by PR13–PR19, optimizes their demonstrated bottlenecks, retains only measured
improvements, and publishes one concise summary. Instrumentation belongs
to the production paths and existing observability system; PR20 does not create
a benchmark service, performance protocol, execution ledger, leaderboard, or
parallel product path. Production observability and simple Linux before/after
measurements are sufficient developer evidence.

Names and layouts of earlier benchmarks, baselines, soak packets, matrices,
scorecards, and profiling harnesses are historical evidence, not prerequisites
or artifacts that PR20 must recreate. Persisted measurement descriptors and
published performance profiles retain compatibility obligations; completion
otherwise follows Linux developer measurements, direct semantic/regression
tests, and normal cross-platform CI.

The canonical
[`docs/superpowers/plans/2026-07-28-v2-delivery-root-crate-breakup.md`](../../superpowers/plans/2026-07-28-v2-delivery-root-crate-breakup.md)
applies these rules to the Phase 0 no-op, query-private, code-index-private,
MCP-handler, focused-test, root-leaf, and domain-leaf baselines and to Gate A/B
retention decisions. Its child plans must record identical host, toolchain,
features, warmth, edit class, rebuilt units, and semantic oracle; package count
or line movement is never a performance claim.

## User outcome

TraceDecay's shipped workflows become materially faster or less
resource-intensive without changing their meaning:

- edit-to-diagnostic, impact, CI/review, and agent-proximity feedback;
- Brain investigation, health, settings, and legal remediation;
- authorized multi-root query and explicit Git operations;
- remote capture, sync, query, backup, restore, and failover;
- task/work graph updates, projections, routing, admitted provider execution,
  progress, cancellation, reconnect/resume, and terminal receipts;
- Rust, TypeScript, and Python SDK journeys; and
- startup, upgrade, migration, recovery, representative focused checks, and
  tests.

When a Linux comparison cannot distinguish a practical improvement from noise
or misses a required failure mode, PR20 reports `pending` or `fail` and keeps
the current implementation.

## End-to-end production path

1. Reproduce a real user journey through its supported entry point and
   canonical daemon/application path to its observable result. Use a sanitized
   realistic corpus and the production authorization, persistence, rendering,
   paging/streaming, cancellation, retry, and recovery behavior.
2. Trace the journey with existing observability and attribute queue, lock,
   I/O, parse, projection, provider/model, merge, hydration, rendering, and
   persistence time plus CPU, memory, disk, network, and write amplification
   where supported.
3. Select a bottleneck from that evidence. A candidate must name the production
   mechanism it changes and the practical user-visible effect it intends to
   improve; intuition, paper results, placeholder numbers, or a synthetic
   microbenchmark cannot select it.
4. Compare baseline and candidate on the same Linux host, source revision,
   workload, corpus, configuration, cache preparation, arrival model, and
   correctness oracle. Record raw samples and practical deltas.
5. Retain a candidate only when the Linux result improves materially and
   direct tests preserve semantic, project-isolation, authority, quality,
   lifecycle, resource, and recovery behavior.
6. Activate through a versioned performance profile that pins the exact prior
   profile. Runtime rollback returns only to that verified profile and
   preserves runtime receipts and in-flight effect fencing.
7. Publish a truthful `pass`, `fail`, or `pending` summary. Normal
   Linux/macOS/Windows CI verifies default-feature product support; it is not a
   benchmark matrix.

If no compatible baseline exists, PR20 first measures the real production
journey from an explicitly recorded source revision and environment. It does
not create a clean/content-addressed checkout snapshot or fill a baseline with
sentinel values.

### Measurement rules

- Record the baseline/candidate source revisions, Linux host/toolchain/runtime,
  workload/corpus/configuration, cache preparation, arrival model, timeout and
  retry policy, command, and correctness oracle. Real source/content digests
  may identify those inputs without creating a checkout snapshot.
- Use the same inputs for baseline and candidate, alternate execution order
  when practical, and retain raw samples. Report offered, completed,
  cancelled, timed out, shed, retried, failed, queue delay, and recovery where
  those states apply.
- Report only statistics supported by the sample count. p50/p95/p99, intervals,
  or memory claims that the run cannot support are `pending`, not inferred.
- Record wall time, CPU, process-tree RSS/PSS or Linux cgroup v2
  `memory.peak`, bytes, I/O, write amplification, and provider tokens/cost
  where observable. Name the measurement method and profiler overhead.
- Production telemetry remains bounded, non-blocking, and redacted. Paths,
  prompts, source, symbols, argv/stdin, provider output, environment, errors,
  and secrets are forbidden.

## PR20 implementation defaults

- Reuse existing `tracing` instrumentation, `sysinfo` process/resource
  diagnostics, and Criterion measurement support; use `psutil` only in the
  Python soak harness for process-tree observations. These replace custom
  telemetry collectors, resource sampling, and microbenchmark plumbing while
  retaining the real-journey oracle and production observability ownership.
- Keep the versioned Linux workload, same-input comparison, practical delta,
  cancellation/retry accounting, and semantic/recovery equivalence rules
  below. If process-tree or profiler-overhead coverage is missing, report the
  affected claim as `pending`.
- Do not create a benchmark service, performance protocol, execution ledger,
  or new measurement authority. A library or harness that cannot feed the
  existing bounded redacted diagnostics and reproducible comparison artifacts
  is rejected in favor of the current path.

## Implementation slices

### Measure shipped journeys

- Choose representative small, current, large, and stress strata from
  PR13–PR19 journeys, including cold start, warm steady state, no-op work,
  incremental changes, concurrent clients, overload, and bounded recovery.
- Measure scheduled-arrival-to-terminal and service latency, p50/p95/p99 where
  support is sufficient, throughput, queue/shed/cancel/retry/failure rates,
  peak and steady resources, bytes read/written, write amplification, and
  semantic/coverage outcomes.
- Keep authorization denials and private content out of traces. Missing,
  sampled, noisy, capped, survivor-biased, or partial evidence is explicit and
  cannot claim a win.

### Optimize measured bottlenecks

- Database/sync: tune measured hot statements and indexes, batch set-based
  reads and atomic writes, bound lock/transaction/WAL work, coalesce identical
  authorized requests, preserve fair source/client progress, and make unchanged
  input bounded no-op work.
- Projection/index/cache: recompute only changed evidence, reuse generations
  only under complete identity, publish one verified generation, bound
  admission/eviction, and prevent repeated startup backfills, rebuild storms,
  mixed generations, or per-request store/schema opening.
- Query/graph/task execution: prune or batch only when exact ordering,
  cursors, fallback, exhaustive affected sets, coverage, quality, proposal
  semantics, provider identity, legal actions, and terminal outcomes remain
  equivalent.
- Daemon/LSP/provider runtime: bound queues and concurrency, reserve
  health/Doctor/diagnostic/cancellation capacity, coalesce without crossing
  project scope, propagate cancellation, and isolate overlays,
  provider context, secrets, and attempts.
- Developer feedback: remove obsolete dependency/feature/build-script edges
  and split build or test boundaries only when repeated same-workload evidence
  improves the frequently touched graph after codegen/link cost.

#### Database and synchronization

- Inspect production SQLite/rusqlite plans for measured hot statements. The
  retired libSQL implementation is not a benchmark target or runtime to
  recreate. Add or remove indexes only for named statements with before/after
  evidence; table size or a full scan alone does not choose a change.
- Bound transaction size, lock hold, connection work, checkpoint cadence, WAL
  growth, vacuum/reclamation, temporary space, and write amplification without
  weakening sole-writer or atomic progress contracts.
- Coalesce only equivalent sync/frontier work, batch safely, preserve fair
  progress across sources, and keep unchanged input a bounded no-op.
- Bound queues, workers, retry state, memory, and per-client bulk admission.
  Reserve health, Doctor, diagnostics, heartbeat, and cancellation capacity so
  bulk load cannot make the daemon unobservable.
- Multiplex short-lived client connections and share heavy same-worktree engine
  state only under complete store/generation/scope/authorization/
  configuration identity. Fair leases, read coalescing, connection/file-
  descriptor high-water, reserved recovery capacity, idle eviction, restart,
  connect/disconnect drain, and injected resource-exhaustion recovery remain
  observable.
- Raising process limits is not an optimization and a low idle descriptor count
  cannot substitute for multi-worktree load/restart evidence.

#### Projection, indexing, caches, and query SQL

- Recompute only changed observations, files, symbols, dependents, documents,
  vectors, task/work horizons, and model cohorts justified by versioned
  dependency evidence.
- Reuse immutable generations/caches only by complete content, schema, grammar/
  model, project scope, and configuration identity. Distinguish OS page cache,
  SQLite page cache/mmap, statement/connection cache, immutable application
  generations, and model/vector cache; “cold” and “warm” require exact
  preparation.
- Maintained views preserve signed (positive/negative) insert/update/delete/
  retraction deltas and watermark identity; "signed" here is arithmetic, not
  cryptographic signing. Compare full, incremental, and batched recomputation
  across change fraction, fan-out, read/write ratio, state bytes, and
  freshness. Mixed deltas equal clean recomputation; incompatible or
  over-break-even frontiers use bounded rebuild rather than a new generic
  incremental engine.
- Bound cache memory/disk, admission/eviction, idle lifecycle, and generation
  deletion. Cancellation, disk full, stale input, and concurrent rebuilds
  publish one complete verified generation or leave the previous generation
  authoritative.
- Marker-gate one-shot backfills/repairs so startup performs bounded no-op work.
  Resolve store handles/application state once per authority scope; per-request
  database open/schema ensure is a regression.
- Post-open repair remains limited to proved derived-index corruption under
  exclusive writer/maintenance authority and never replaces a corrupt
  authoritative database.
- Review plans for every measured hot SQL statement. Avoid per-element lookup
  and hydration; use set-based joins/`IN` pushdown or one cursor pass. Push
  filter/order/limit into SQL, use indexed cursor range scans rather than
  OFFSET walks, reuse prepared statements/connections, and maintain hot stable
  counts incrementally only when evidence beats recomputation.
- Batch fsync deliberately inside transactions where atomicity permits.
  Aggregate status and fingerprint reads remain set-based, deterministic, and
  parse only exact-span cache misses.

#### Retrieval, graph, task, and outcome execution

- Attribute selection, queue, each retriever, rank/model, merge, dedupe,
  rerank, hydration, rendering, synthesis, and total critical path separately.
  Report requested/consumed budget, raw/eligible/deduplicated/returned counts,
  rank buckets, unique/final-top-k contribution, source freshness/coverage/
  denial, and labeled Recall@K/nDCG where an oracle exists.
- Exact, lexical, graph, temporal, task/session, diagnostic, and semantic
  ablations use the same versioned total candidate budget; unused budget is not
  silently moved. Reranker comparisons use the same recorded candidate input.
  Exact flat-vector scan remains the ANN oracle, and ANN reports average, tail,
  minimum recall, zero-recall queries, and measured break-even.
- Preserve deterministic order, exact tiers, temporal truth, stable cursors,
  coverage, explanations, and lexical fallback. Bound cross-project fan-out,
  graph traversal, reranking, buffering, and client concurrency with explicit
  partial/unavailable coverage.
- Context comparisons pin the same work and source identity and report
  required/included/independently relevant/irrelevant/stale/truncated/unknown
  authorized anchors, precision at 1/3/5, required-anchor coverage, bytes/
  tokens, assembly latency, time to first valid action, rediscovery, verified
  correctness, rework, and censored/unknown outcomes. Completed status, lower
  latency, token reduction, or worker self-report never substitutes for
  independently verified quality.
- Batch affected-test traversal by breadth-first frontier while retaining the
  exact exhaustive sorted set and deterministic distance-ranked
  recommendations.
- Preserve alias, cursor, idempotency, deletion, and restart semantics when
  optimizing anchor persistence tails; attribute statements, lock hold,
  WAL/fsync, payload, and retrieval-anchor work before batching or indexing.
- Incremental task readiness, critical path, history, Kanban/DAG/timeline/
  causal/workload, task-shape, model-capability, outcome, and calibration
  projections update only affected horizons and remain deterministic. Kanban
  is not a second cache/query engine.
- Proposal generation is coalesced, bounded, cancellable, and deduplicated by
  pinned input digest; lease heartbeat, cancellation, explicit runtime
  controls, and deterministic fallback have priority. Optimization cannot
  coarsen model-version identity, omit negative/censored outcomes, hide
  coverage, or alter a reviewed estimator.
- Record requested and actual topology, route, fan-out, concurrency, provider/
  backend/model/protocol, queue, effects, and terminal state separately. A
  throughput gain that increases rejected, unknown, censored, duplicate-effect,
  review, integration, or rework outcomes is a regression.

#### Provider and workflow runtime

- Measure provider discovery/negotiation, queue, lease-to-process/session start,
  context assembly, structured event ingestion, first progress, stdout/stderr/
  artifact throughput, cancellation/interrupt/terminate/kill escalation,
  terminal receipt, reconnect/resume, and restart recovery.
- Preserve typed argv/stdin, exact executable/protocol/model identity,
  sandbox/approval/environment boundaries, ordered structured events, lease
  fencing, and resume proof. Process pooling or app-server reuse requires exact
  project/configuration identity and cannot retain another attempt's
  context or secrets.
- Lower startup cost never permits hidden CLI fallback, shell execution,
  PID-only adoption, dropped terminal outcomes, recursive auxiliary dispatch,
  or heartbeat-only no-progress resets.
- Native Claude Code CLI, Codex app-server, and explicitly allowed Codex CLI
  fallback remain distinct strata. Missing/stale executables, version changes,
  malformed/oversized streams, saturation, missing heartbeat, cancellation,
  daemon restart/resume, credential-handling failures, and concurrent auxiliary attempts
  across isolated worktrees remain in the journey.
- The workflow deadline and progress frontier owners remain unchanged.
  Measure queue wait, remaining monotonic deadline, configured no-progress
  timeout, last committed frontier, stall duration, and every cancellation/
  kill result. Unproved provider termination remains partial or effect-unknown.
- Stalled temporal/session retrieval exposes last real progress, backlog,
  blocker, retry class, and typed unavailable reason and rejects known
  unavailable reads before expensive retrieval. Heartbeats never synthesize
  progress, and restart/idempotency behavior remains unchanged.

#### Git intelligence and LSP gateway

- Measure Plan 36 status/diff/hunk preview and explicit index-transaction apply
  separately across repository size, changed paths/hunks, index-lock wait,
  parsed/applied bytes, and stale-preview rejection. Preserve `HunkRef`
  preconditions, revalidation, index-lock ownership, atomic receipts, and
  refusal of autonomous branch/worktree/ref/history mutation.
- Reuse native Git object/diff/patch/index behavior and canonical graph/query
  caches; do not build a second repository graph or retain patch payloads as a
  performance cache.
- Attribute LSP gateway, queue, bridge, analyzer, indexing, merge, publication,
  cold/warm startup, workspace index, hover/navigation, edit-to-diagnostic/
  context, coalescing, cancellation, clean-cache reuse/no-op, overlay conflict,
  reconnect, and crash/recovery.
- Share analyzer process/generation/cache state only under complete identity and
  exact client overlay isolation. Coalescing propagates cancellation without
  dropping a response still needed elsewhere; cache keys include complete
  provider and input identity. Lower process count never permits stale,
  cross-session, provenance-losing, or disclosed unsaved content.
- PR13 feedback measurements retain one-shot stage/total latency, trigger
  budget, dedupe/suppression, render truncation/expansion, edit-to-durable
  adapter latency, GitHub ingest/remap/surface, CI localization, and concurrent
  agent-proximity cost.
- Request dispatch profiles authorization, fair admission, staleness sync,
  canonical execution, rendering, analytics/accounting, notifications, and
  hot-swap separately. Structural decomposition or caching preserves byte-
  identical responses, errors, token accounting, cancellation, fairness,
  notifications, and crash/restart behavior.

#### Developer build and verification

**Measured root-check baseline (2026-07-28).** On this Linux machine, with a
warm target and an mtime-only touch of one root leaf file,
`cargo check -p tracedecay --lib --all-features` took **121.35 s**. The
`tracedecay` root then contained 824,173 lines in 1,142 `src/` files. As a
contrast, touching one file in the roughly 37k-line `tracedecay-domain` crate
and checking that crate took **20.19 s**. The ten existing workspace crates
combined are roughly 147k lines, about one sixth of root. Root remains one
compilation unit, so adding cores cannot shorten this wall by itself.

This is the comparison point for the leaves-first extraction sequence in
[Plan 12](12-root-compatibility-migration.md), not proof that a crate boundary
will help. For each candidate slice, retain the same-host root baseline and
measure a treatment with the same command, target warmth, leaf-touch edit
class, features, toolchain, and correctness checks. Keep a boundary only when
the measured frequently touched compile graph improves; otherwise report
`pending` or reject it. Package count, source size, and the architectural
sequence do not substitute for that comparison.

- Record stock-Cargo clean, exact no-op, private body, public signature/type,
  macro/proc-macro, build-script/asset, feature/dependency/manifest, and
  focused-test edit classes with explicit package, target, features, test
  target, and toolchain.
- Record wall time, CPU/utilization, peak memory, rebuilt/reused units, critical
  path, codegen/link, build-script work, and cache outcome. Compare on the same
  host/toolchain/source/build state.
- Isolate heavy providers, grammars, model runtimes, transports, dashboard
  assets, and test-only support from unrelated focused work. Split integration
  targets only when the measured focused journey improves after added codegen/
  link cost. Keep build scripts deterministic with narrow rerun inputs.
- Portable manifests, profiles, features, and build settings are eligible only
  after clean, incremental, test, release, CI, and published-package effects
  are measured separately.
- Local wrappers, target locations, lane allocation, cache policy, and Rust
  Analyzer are environmental context, never roadmap mechanisms or portable
  thresholds.

Each change is independently removable. An optimization that requires a
parallel storage authority, semantics-changing shortcut, hidden fallback,
performance-only cache of protected payloads, or machine-local build policy is
ineligible.

### Verify, roll out, and clean up

- Run direct journey, semantic-equivalence, crash/restart, overload,
  cancellation, and recovery tests for the touched path plus normal repository
  CI.
- Retain the optimized implementation only when direct tests pass and the Linux
  comparison reports `pass`.
- Remove rejected candidate code, temporary profiling hooks, candidate-only
  flags, placeholder/provisional baselines, and standalone harness/protocol
  scaffolding. Production instrumentation used for health and regression
  diagnosis remains bounded and non-blocking.
- Publish one concise summary of measured journey improvements. There is no
  combined performance score.
- Existing Observatory/Costs views may render comparison identity,
  support/coverage, intervals/margins, stage/resource/effect evidence, and
  disposition from the canonical backend result. They contain no client-side
  formula and do not become a separate benchmark product.

### Runtime rollout and rollback

- Activate a candidate through a versioned profile that names the exact prior
  profile and exposes an explicit rollback operation; there is no
  “latest profile” lookup.
- Side-effect-free paths may shadow the candidate. Effectful paths use the
  owning operation's normal receipts and fencing rather than a separate
  performance-control protocol.
- Semantic divergence, a practical resource/deadline regression, wrong-project
  output, duplicate/unknown effects, or recovery failure returns to the pinned
  prior profile. In-flight work remains under its owning workflow's
  reconciliation rules.
- Missing or noisy comparison evidence prevents activation or reports
  `pending`; it does not create epochs, fixed-window rituals, or
  a standalone canary gate.

## Semantic and safety constraints

- One fenced daemon owns each mutable shard. No optimization adds a client
  database connection, second writer, dual write/read, or repair-on-read path.
- Results preserve authorization, project isolation, stable errors, scope,
  exact tiers, ordering, cursors, coverage, legal actions, durable effects,
  idempotency receipts, paging, streaming, backpressure, cancellation, retry,
  reconnect/resume, and one canonical terminal outcome.
- No-op and retry paths preserve zero or bounded durable work as defined by
  their owner. Batching never weakens atomic cursor/receipt/projection commits
  or crash/restart replay.
- Cache, process, analyzer, connection, or generation sharing requires complete
  store, project/worktree generation, scope, authorization,
  configuration, protocol, model/provider, and overlay identity as applicable.
- Explicit Git mutation preserves preview freshness, index-lock ownership,
  atomicity, receipts, and rejection of autonomous branch, ref, worktree,
  history, or remote mutation.
- Linux developer measurements use Linux process/resource boundaries. Normal
  Linux/macOS/Windows CI must remain green for the default-feature product.

## Direct acceptance

- Every retained change starts from an observed bottleneck in a shipped
  PR13–PR19 journey and has a reproducible Linux baseline/candidate comparison.
  Schema-only, declaration-only, synthetic placeholder, and planning-artifact
  evidence is inadmissible.
- Paired evidence clears the frozen practical improvement margin; required
  latency, throughput, memory, CPU, disk, write-amplification, no-op,
  startup/recovery, cancellation, timeout, shed, retry, failure, unknown,
  censoring, quality, and effect guardrails do not regress.
- Journey tests prove semantic equivalence and cover concurrent load,
  cancellation, daemon reconnect, crash/restart, WAL/checkpoint interruption,
  projector/generation recovery, cache loss, provider failure, and overload.
- Default-feature product tests pass in normal Linux/macOS/Windows CI for each
  retained cross-platform path; developer benchmarks remain Linux-only.
- Missing compatible baseline, noisy samples, unsupported tail statistics,
  partial child-process/resource coverage, or incomplete correctness coverage
  produces `pending` or `fail` and no rollout.
- Fabricated or incompatible baseline lineage, missing raw aggregate lineage,
  post-result threshold changes, coordinated
  omission/survivor bias, hidden protected strata, or measurement-artifact
  leakage invalidate the comparison entirely. They do not decide candidate
  quality or remain published as accepted evidence.
- Active rollout immediately returns to the exact pinned prior accepted profile
  on semantic divergence, authority violation, wrong-scope evidence,
  hidden fallback, duplicate or unknown unsafe effect, secret disclosure,
  deterministic-order failure, or recovery failure.
- Direct product tests, normal CI, and the Linux comparison pass.

## Replacement and deletion

PR20 removes candidate implementations that did not pass, temporary
performance-only services and protocols, execution ledgers, generated
scorecards, placeholder baselines, synthetic acceptance packets, and
obsolete comparison artifacts. Existing observability may retain the typed
operational outcome needed to explain why no rollout occurred, but it must not
present a failed or pending outcome as a gain.

## Not in PR20

- New product semantics, benchmark-only APIs, a telemetry database, benchmark
  daemon, leaderboard, or performance dashboard required for product use.
- Machine-specific target paths, lane/shim/cache policy, analyzer shutdown, or
  serialization of independent developer work.
- Optimization selected by package count, code size, schema conformance,
  transferred thresholds, point estimates, or publication pressure.
