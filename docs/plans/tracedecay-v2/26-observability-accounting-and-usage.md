# TraceDecay V2 Observability, Accounting, and Usage Plan

## Status / role

Cross-cutting instrumentation is implemented only with each owning product
slice; this sentence is not a blanket completion certificate. PR14 completes
the Observatory and Costs experience over the resulting canonical read models.
As of 2026-07-27 the focused dashboard backend and frontend suites exercise
those experiences successfully, so the former suite-blocked "implemented but
unverified" qualification is withdrawn. PR14 acceptance is still open:
Observatory lacks hook-hint/event-flow/latency presentation, Costs lacks its
latency breakdown, and the Plan 11 performance/SSE/usability journeys have not
executed. Plan 11/PR14 owns those UI gaps; this plan retains the canonical
measurement and coverage semantics. This plan is a product observability
contract, not a plan compiler or delivery tracker.
Its versioned measurements and coverage semantics are the canonical product
telemetry input to [PR20 performance optimization](33-end-to-end-performance-optimization.md).
Versioned benchmark, profiler, and operating-system measurements remain valid
PR20 evidence under that plan's measurement contract.
Plan 33 owns no execution ledger. It consumes accepted production
observability and comparison artifacts, freezes workloads, compares effects,
and records only the accepted/provisional/rejected comparison disposition
required by its direct optimization journey. Those artifacts are not new
telemetry events, rollout states, planning authority, or a generated delivery
tracker. Product owners emit through this plan's existing schemas, and Plan 33
never changes Plan 26 labels.

**Observatory truthfulness correction (2026-07-26).** The canonical event
envelope preserves `Failed` and `TimedOut` as terminal outcomes. The
Observatory projector counts either as an observed failure only when coverage
is known; unknown, stale, partial, sampled, capped, invalid, or dropped input
withholds the numeric value and carries an unavailable reason instead of
rendering zero. This behavior shipped in `7d1e1d1f5` and was hardened in
`5d5902dd3`; the broader dashboard acceptance status above remains unchanged.

Earlier event-file layouts, implementation ownership lists, fixture matrices,
benchmark packets, panel paths, and aggregate gate manifests are historical
evidence, not prerequisites or artifacts that later work must recreate.
Only actually independently released public event/metric names retain protocol
compatibility; persisted observability records use the fresh-store rule. All
other retention is judged by the direct measurement, privacy, lifecycle,
platform, and regression behavior below.

The Plan 26 Work/topology event family is absent from `origin/master` and a
published release. Its `V1` suffixes alone do not imply a V2 sibling. Transient
source-only/internal emission helpers, wire-visible V2 event revisions, event
files, stored observability rows, accounting projections, checkpoints, and
receipts change in place. Only their exact final persisted shape is accepted;
any other database, store, spool, file, or projection returns typed
`ResetRequired` and requires explicit reset or recreation. No storage reader,
migration, backfill, dual write, or census path exists.

## Outcome

Every operational and product metric states what was measured, over which population and horizon, at which watermark, with what coverage. Unknown, partial, stale, sampled, or capped data stays visible and can never render as a trustworthy zero.

## Delivery-first product journeys

The event, projection, descriptor, retention, label, and measurement
contracts below remain binding. They ship as instrumentation and read models on
real product paths, not as schema-only, fixture-only, projector-only, or
promotion-gate milestones.

**PR14 finding → evidence → Doctor → confirmed remediation:** production
operations emit through the canonical observability boundary; denominator-safe
projectors build the exact read models consumed by CLI, MCP, HTTP, Observatory,
Costs, Settings, and the one Doctor application kernel; the user drills from a
finding to safe evidence, confirms an owner remediation, follows its receipt,
and sees post-operation health, usage, latency, resource, token, cost, drop,
coverage, and unavailable state reconcile at pinned watermarks. Every
retrieval, adoption, automation, performance, retention,
rejected-argument, LSP, feedback-cycle, and provider measurement specified
below remains attached to its owning production action and visible through the
appropriate PR14 view.

**PR17 work item → admitted execution → observed outcome → reviewed
replanning:** Plan 24 work identity and evidence flow into a real Plan 32 run;
the selected provider adapter emits requested/actual route, negotiation,
queue/admission, lease, progress, heartbeat, stream/artifact, cancellation,
recovery, terminal, resource, token, cost, topology, integration, test/CI,
review, and outcome observations; canonical projectors expose denominator,
coverage, censoring, selection/override, drift, and safe anchors; Work,
Observatory, and Costs render those same values; and Plan 24 may propose a
versioned graph change that still requires explicit user disposition.

The detailed semantics below are the coverage audit for those journeys. No
product metric, label, dimension, cohort, compatibility surface, retention
behavior, or unavailable state is reduced or deferred by this framing.

## Owns

- Canonical accounting, usage, latency, outcome, and health event contracts.
- Metric descriptors, units, populations, horizons, coverage, and aggregation semantics.
- Versioned quantifier descriptors, cohort definitions, coverage/uncertainty
  semantics, temporal baselines/deltas, calibration/drift observations,
  outcome linkage, and optional decision-policy evidence. A
  universal code-health/quality/reward score is explicitly not an SLO or
  product-success denominator.
- Denominator-safe projections and Observatory/Costs read models.
- Product-wide lag, SLO, adoption, hint-outcome, and automation-outcome definitions.
- Plan 24 task/model outcome observations, comparable evaluation cohorts, and
  denominator-safe routing-review metrics consumed by typed policy, including
  task-shape feature/estimate revisions, proposal lifecycle, model-capability
  profiles, independent-review grades, first-pass and parent-normalized
  outcomes, calibration error, censoring, selection/override/exploration
  exposure, and drift/change-point evidence.
- Bounded execution-topology observations and read models for
  requested/accepted/admitted/active/useful concurrency, duplicate work and
  duplicate effects, conflict-prediction precision/recall, ready-to-integrated
  latency, observed native-merge success, stale-stack age, blocked time,
  runtime/test/CI reruns, operational leaks, and runtime/delivery
  fanout.
- The canonical versioned independent-review and task-outcome label vocabulary,
  evidence requirements, transition-validity inputs, and measurement schema
  consumed by Plan 24 graph state, Plan 06 policy, Plan 11 UI, and public
  application/surface contracts.
- Trace and retrieval anchors needed to explain aggregate results without exposing private content.

## Does not own

- A separate telemetry database, scheduler log, workflow event stream, or per-surface counter system.
- Product execution, retries, admission, policy, or side effects.
- Model assignment, task decomposition, route activation, or opaque
  self-modifying policy. This plan supplies evidence; Plan 06 policy recommends
  and Plan 32 executes under Plan 24 semantics.
- Work-plan/item proposal or graph-transition authority. Plan 24 consumes
  canonical labels and decides legal graph transitions; this plan never accepts
  a proposal, changes readiness, or marks graph work complete.
- Worktree, branch-stack, integration, Git, lease, test, CI, or conflict
  authority. Metrics observe typed owner events and receipts; they never infer
  a merge from branch labels, call Git/CI, request a rerun, release a blocker,
  cancel an attempt, or turn a correlation into a graph/runtime action.
- Raw provider payloads or unsanitized content.
- A source parser, Markdown parser, compatibility inventory, plan ledger, generated execution graph, or meta compiler.
- UI-local metric formulas or transport-specific metric meanings.

## Required behavior

### Canonical events

- Emit versioned events through the same authoritative event/store path as other V2 observations.
- Emit bounded [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)
  events for sessions; methods, outcomes, and latency; queueing and
  cancellation; analyzer startup, restart, and indexing/degraded state; cache
  reuse and overlay freshness; diagnostic add and clear; provider conflicts;
  host delivery path; partial coverage and drops; and bridge reconnect.
- Plans 25/31 emit bounded incremental-index observations for exact worktree
  identity classes without paths: event-to-reconcile and event-to-ready
  latency; debounced/raw event counts; overflow/rescan reason; candidate,
  hashed, parsed, and no-op file counts; Tree-sitter changed-range bytes;
  chunks added/changed/deleted/reused; relation/test invalidation fan-out;
  projection batches/chunks; physical cross-worktree reuse count; queue depth/
  bytes; cancellation/supersession; full-rebuild reason; and publication
  outcome. Parse, graph, lexical, vector, and publication stages remain
  separately timed.
- Retrieval observations distinguish serving a current generation, serving a
  prior complete compatible generation while a newer one indexes, omitting
  semantic results, and strict-semantic unavailable. Indexing time never counts
  as query latency, and absence of semantic candidates during indexing never
  becomes a zero-match quality result.
- LSP telemetry contains no paths, source, symbols, or diagnostic messages.
- [Plan 36](36-git-aware-change-context-and-index-transactions.md) telemetry may
  identify the operation kind and outcome only. Patch content,
  paths, commit messages, author identity, and conflict content never enter
  canonical events, aggregates, exports, or drill-down anchors.
- [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)
  telemetry is staged by owning PR: PR11 emits feedback-cycle trigger identity,
  evaluation stage, per-trigger terminal reason, budget-exceeded state,
  duplicate-trigger dedupe, suppression, and stage/total latency; PR12 emits
  CLI/MCP/HTTP/LSP delivery, truncation, and expansion state without payloads;
  PR13 emits GitHub item/thread lifecycle
  (`current`, `outdated`, `resolved`, `edited`, `deleted`) separately from
  GitHub ingress provider outcome (`complete`, `partial`, `unavailable`,
  `denied`, `rate_limited`, `stale`, `failed`),
  CI-failure localization states and typed provenance without log content,
  concurrent-agent proximity warning emission/suppression/expiry/risk class,
  pinned Plan 20 `feedback.proximity.risk_threshold` revision/digest,
  host-adapter delivery state, and truncation/expansion handle/anchor usage
  and failures without payloads; PR14 owns Observatory/Doctor read models and
  dashboard projections over those events. The GitHub lifecycle and ingress
  outcome sets are exhaustive and orthogonal: lifecycle describes observed
  item/thread state, while ingress outcomes describe refresh, coverage,
  availability, rate-limit, staleness, failure, or read authorization
  (`denied`) only. Plan 35 semantic-evidence provider states (unsupported,
  absent, indexing, stale, cancelled, timed-out, failed, partial versus
  supported plus completed plus complete-coverage zero-findings) remain a
      third set. GitHub Stacked PR capability
      (`Unavailable | PrivatePreviewDisabled | Enabled | Degraded`) is a
      fourth independent set and never becomes comment lifecycle, ingress
      outcome, or semantic-evidence provider state. Attempted outbound GitHub writes emit separate `policy=denied`
  and `effect=suppressed` observations before any call, never a lifecycle or
  ingress value. No `posted`, `updated`, `dismissed`, or `replied` lifecycle
  exists; `resolved` is the observed read-only lifecycle value. All metrics remain
  denominator-safe. Telemetry contains no source, path, diagnostic message,
  comment body, CI log content, or private session content.
- Identify scope, capability, operation, result, event and observation time,
  duration or quantity, unit, producer revision, and trace.
- Use stable idempotency keys so retries and replay cannot double count.
- Record terminal outcomes separately from attempts and preserve cancellation, rejection, timeout, partial success, and unknown outcomes.
- Keep instrumentation bounded and non-blocking while making dropped or delayed telemetry measurable.
- PR17 emits bounded Plan 24/32 observations for task-shape and
  decomposition grade and estimate ranges; proposal/decision identity;
  requested/actual model route and exact model/version/effort/tool/host
  capability; first-pass scope completion and accepted correctness;
  tests/review independence and finding severity; escaped defects;
  rework/remediation and parent/integration overhead; retries and typed causes;
  queue/execution latency; tokens/cost/resources; autonomy; human
  intervention/override; cancellation; censoring/unknown horizon; and audit
  coverage. Records pin work/acceptance/decomposition, estimator, cohort,
  policy/config/catalog, evidence horizon, and valid/observation-time
  revisions. Self-reported completion is a separate evidence class and never
  substitutes for independent acceptance, tests, review, or outcome.
- PR17 auxiliary-provider observations include requested and actual
  provider/backend/executable/protocol/model/reasoning identity; capability
  negotiation and explicit fallback decision; exact task/attempt/Session and
  scope identity; queue/admission/start/progress/heartbeat/event/artifact/
  terminal timing; stdout/stderr/structured-event byte and drop coverage;
  sandbox/approval/capability class; cancellation/deadline/interrupt/terminate/
  kill stages; restart/reconnect/resume state; and one terminal
  `Completed`, `Unsupported`, `Absent`, `Stale`, `Cancelled`, `TimedOut`,
  `Failed`, or `Partial` outcome. Events never contain argv/stdin values, raw
  output, environment values, secrets, prompts, paths, or private context.
- PR17 topology/routing observations include selected and actual topology,
  partition count, edge cut, coupling, critical path, serial fraction, hubs,
  barriers, runnable/actual concurrency, saturation, scheduler overhead,
  context transfer, coordination/integration/review/rework; plus eligible
  route set, exclusions, score vector, randomized propensity when applicable,
  deterministic baseline, exploration/override/fallback reason, horizon,
  censoring, and defensible counterfactual coverage.
- Escalation, recall, handoff, and verifier observations include blocker
  recall, question precision and over-asking, intervention outcome,
  helpful/neutral/harmful/unknown precedent utility, quarantine/retirement
  effectiveness, rediscovery reads/searches/tests/tokens/time-to-first-valid-
  action, accepted correctness/rework, checkpoint grounding, no-progress
  precision, verifier exploit success, false accept/reject, legitimate-solver
  retention, and reviewer independence/conflict.

### Execution-topology observations and bounded metrics

Plans 24, 32, 36, and 37 emit source facts through the one observability
application boundary; this plan owns only the schemas, joins, descriptors, and
read models. The persisted execution-topology event family is:

- `ExecutionTopologySampledV1`
  (`work.execution_topology.sampled.v1`);
- `WorkConflictPredictionObservedV1`
  (`work.conflict_prediction.observed.v1`);
- `WorkConflictOutcomeLinkedV1`
  (`work.conflict_outcome.linked.v1`);
- `WorkIntegrationTransitionObservedV1`
  (`work.integration.transition.observed.v1`);
- `WorkStackDriftObservedV1`
  (`work.stack_drift.observed.v1`);
- `GitHubStackCapabilityObservedV1`
  (`work.github_stack_capability.observed.v1`);
- `WorkDuplicateEffortObservedV1`
  (`work.duplicate_effort.observed.v1`);
- `WorkBlockedIntervalObservedV1`
  (`work.blocked_interval.observed.v1`);
- `WorkRerunObservedV1` (`work.rerun.observed.v1`);
- `WorkExecutionLeakObservedV1`
  (`work.execution_leak.observed.v1`); and
- `WorkDeliveryFanoutObservedV1`
  (`work.delivery_fanout.observed.v1`).

Every event uses `ObservabilityEnvelopeV1`, carries event/observation time and
valid-time interval where applicable, pins schema/producer/config/policy
revisions, coverage, watermark, idempotency identity, and an opaque
authorized local join reference, and distinguishes attempt from terminal
outcome. Events contain no task title or TaskId, actor/user/session, project or
repository identity, path, worktree locator, branch/ref, commit/object ID,
commit message, author, patch/conflict/source/test/CI/log content, prompt,
provider output, model version, host name, argv/stdin/environment, secret, URL,
or reversible digest. Exact identities remain in authorized owning history and
are reached only through local non-exportable anchors after authorization.

`ExecutionTopologySampledV1` records the selected topology kind
`Single | Sequential | Parallel | Hierarchical | Hybrid`, requested, Plan
24-accepted, Plan 32-admitted, active, and useful width; runnable and blocked
counts; capacity saturation class; critical-path and serial-fraction buckets;
fan-in/fan-out buckets; shared-authority serialization count; and coverage.
“Useful” requires distinct admitted attempts that each advanced a committed
Plan 32 `ProgressFrontier` during the interval and are not linked by an
adjudicated duplicate-work relation. Heartbeats, queued work, card presence,
provider child processes, and transport fanout never count as useful
concurrency.
It separately records bounded classes for execution placement
(`None | InPlace | LinkedWorktree | IsolatedClone`), branch topology
(`NoBranches | Unbranched | IndependentBranches | LocalStack`), review
topology (`NoReview | IndependentReview | StandardPullRequests |
GitHubStackedPullRequests`), and integration strategy (`NoIntegration |
ExternalObservedOnly | FastForwardOnly | MergeCommit |
CherryPickExactCommits`). These dimensions are never derived from one another.

`WorkConflictPredictionObservedV1` records prediction identity, prediction
time before integration, `Mechanical | Semantic | Combined`, predicted
`Conflict | NoConflict | Abstained | Unknown`, score kind and descriptor/
calibration revision, eligible relation classes, evidence coverage, expiry,
and at most eight authorized local anchors. `WorkConflictOutcomeLinkedV1`
links an independently observed native conflict/no-conflict or adjudicated
semantic conflict/no-conflict outcome to that prediction and records
`Observed | Censored | Unknown`, adjudicator class, horizon, coverage, and
late/corrected revision. A clean Git operation cannot adjudicate semantic
conflict; a test/review finding cannot adjudicate native mechanical conflict.
Unlinked, stale, partial, denied, and unresolved cases remain explicit and do
not enter a confusion-matrix denominator.

`WorkIntegrationTransitionObservedV1` uses the exhaustive phase set
`Ready | ProposalCreated | DryRunRequested | DryRunTerminal |
ApplyRequested | ApplyTerminal | NativeIntegratedObserved |
RequiredChecksTerminal | AcceptedOutcomeObserved | Cancelled | Censored |
Unknown` and result set `Succeeded | Conflicted | Rejected | Denied | Stale |
Locked | Cancelled | TimedOut | Failed | Partial | EffectUnknown |
Unsupported | Unknown`. It records operation kind
`FastForward | MergeCommit | Rebase | CherryPick | StackRetarget |
GraphOnly | ExternalObserved | Unknown`, source/target scope classes,
dependency-commit coverage, required-test/check coverage, and owner-receipt
class. `ApplyRequested` can exist only for an owning typed operation that
actually supports apply. Plan 36 supports clean, authorized, no-conflict,
policy-approved fast-forward, two-parent merge, and exact ordered cherry-pick;
rebase remains external observation only. `NativeIntegratedObserved` requires native Git evidence that
the exact target snapshot contains the declared integration result; a Plan 24
proposal decision, card move, process exit, or CI result cannot synthesize it.

`WorkStackDriftObservedV1` records
`HeadAdvanced | BaseAdvanced | MergeBaseChanged | DependencyMissing |
RefDeleted | WorktreeGenerationChanged | Retargeted | Superseded | Unknown`,
first-observed and terminal times, open/closed state, current coverage, and a
fixed age bucket. `WorkDuplicateEffortObservedV1` records only an independently
adjudicated `ExactDuplicate | SupersededOverlap | RepeatedInvestigation |
DuplicateEffect | NotDuplicate | Censored | Unknown` relation plus bounded
wall, token, cost, test, and effect quantities with their evidence class.
Similarity, proximity, same path, or concurrent execution alone never labels
duplicate work.

`GitHubStackCapabilityObservedV1` records exactly `Unavailable |
PrivatePreviewDisabled | Enabled | Degraded`, capability/probe revision,
complete/partial coverage, and standard-Git/other-forge fallback availability.
It never labels repository, stack, PR, branch, user, or provider position.
Stack-operation observations distinguish direct versus stack-aware merge queue,
atomic lower-layer inclusion, and partial-merge rebase/retarget as provider
outcomes; they never report a TraceDecay rebase or force-push effect.

`WorkBlockedIntervalObservedV1` records one exact valid-time interval and
`Dependency | NeedsInput | Capability | Policy | Scope | Conflict | Lease |
Backpressure | Test | CI | Review | EffectUnknown | Other | Unknown`.
Intervals are versioned and may overlap; the projection computes unioned wall
time separately from per-cause attributed time. `WorkRerunObservedV1` requires
a new Plan 32 attempt ID or an observed test/CI rerun ID linked to one prior
attempt and uses `RuntimeRetry | RuntimeFallback | TestRerun | CiRerun |
Recovery | HumanRequested | Unknown` plus a typed cause. Repeated logs,
redelivery, or the same attempt ID never count as a rerun.

`WorkExecutionLeakObservedV1` records only independently proved
`LeaseAfterTerminal | AttemptWithoutLiveOwner | EffectUnknownPastDeadline |
MissingWorktreeBinding | UnboundedDelivery | None | Unknown`, the detection
horizon, recovery state, coverage, and safe owner class.
`WorkDeliveryFanoutObservedV1` records one canonical event class, eligible
surface count, attempted/delivered/deduplicated/dropped/unknown counts, and
surface family `Hook | MCP | LSP | Dashboard | CLI | Other`; it never records
addresses, payloads, principals, or recipient identity.

The canonical read model is `ExecutionTopologyMetricsV1` with these exact
descriptor names and semantics:

- `work_execution_concurrency_width{phase=requested|accepted|admitted|active|useful}`
  is a duration-weighted distribution, not a point-in-time maximum;
- `work_execution_useful_concurrency_ratio` is useful attempt-time divided by
  admitted attempt-time over intervals with known coverage;
- `work_execution_fanout_width{phase=requested|accepted|admitted|peak_active|useful}`
  reports the width distribution and preserves serialized/blocked work;
- `work_duplicate_effort_total{kind,unit}` and
  `work_duplicate_effort_ratio{unit}` use only adjudicated duplicate relations
  and report wall time, tokens, cost, tests, and effects separately;
- `work_duplicate_effects_total{outcome=prevented|committed|unknown}` keeps a
  prevented duplicate distinct from a duplicate observable effect;
- `work_conflict_prediction_total{kind,outcome}` plus
  `work_conflict_prediction_precision{kind}` and
  `work_conflict_prediction_recall{kind}` use linked pre-integration
  predictions and independent outcomes only;
- `work_ready_to_integrated_seconds{integration_kind}` starts at the first
  Plan 24 `Ready` valid time for the pinned work-item version and ends at the
  first exact `NativeIntegratedObserved`; supersession, cancellation, target
  drift, or incomplete horizon is censored, not success or zero;
- `work_merge_attempts_total{integration_kind,outcome}` and
  `work_merge_success_ratio{integration_kind}` count observed native
  integrations only. Mechanical success, required checks, accepted task
  outcome, and escaped defects remain separate dimensions;
- `work_stale_stack_age_seconds{drift_kind,state=open|closed}` starts at the
  first proved invalidating observation and stops only on exact retarget,
  supersession, or restored-current evidence;
- `work_blocked_wall_seconds` is the union of blocked intervals, while
  `work_blocked_cause_seconds{cause}` is attributed per cause and may sum above
  wall time when causes overlap;
- `work_reruns_total{source=runtime|test|ci,cause}` and
  `work_rerun_rate{source}` use eligible original attempts/runs as the
  denominator and never count transport replay;
- `work_execution_leaks_total{kind,outcome}` retains unknown coverage; and
- `work_delivery_fanout_total{surface,outcome}` and
  `work_delivery_duplicate_ratio{surface}` count delivery and dedupe without
  treating multi-surface delivery as duplicate product work.

Width uses fixed buckets `0`, `1`, `2`, `3..4`, `5..8`, `9..16`, `17..32`,
`33..64`, and `over_64`. Ready/integration, stale age, blocked time, and rerun
latency use `under_1m`, `1m..5m`, `5m..15m`, `15m..1h`, `1h..4h`,
`4h..24h`, `1d..7d`, and `over_7d`. Raw timestamps and exact durations remain
authorized local detail; shared packets contain only bucket counts.

Conflict precision/recall requires at least 50 independently adjudicated
eligible cases per kind, at least 90% outcome coverage, at most 10% censoring,
and no unresolved descriptor/cohort shift. Merge-success and rerun rates, and
ready-to-integrated percentiles, require at least 20 eligible cases and 90%
coverage. Useful-concurrency and blocked/stale distributions require at least
90% interval coverage. When a floor fails, the metric is unavailable with
eligible/observed/censored/unknown counts; it never renders zero, 100%, or a
trend arrow.

Allowed local grouping dimensions are only topology kind, work class,
fixture-size bucket, integration kind, conflict kind, blocker class, rerun
source/cause, leak kind, surface family, host family, OS family, product
major/minor version, and coverage class. The existing maximum of eight
dimensions, 4,096 local cells per daily bucket, 256 returned cells, and
minimum-five local-cell suppression applies. Shared aggregation remains
limited to the existing share allowlist; topology, conflict, blocker, rerun,
leak, and integration dimensions remain local-only. No metric groups, filters, sorts, or
exports by person, agent, TaskId, initiative, project, repository, worktree,
branch, ref, commit, model version, or exact route.

Each event contains only counts plus at most eight authorized local anchor
references and 16 cause buckets; overflow sets coverage `Capped` and folds to
`other`. `OptionalLocalDetail30d` retains joinable event detail for at most 30
days; `LocalRollup395d` retains bounded cells; owning task/runtime/Git receipts
keep their separate owner retention and are not copied. Projection rebuild,
late linkage, correction, deletion, and retention expiry are idempotent and
version-monotone. A branch/worktree deletion may close or censor a metric
interval but cannot erase a retained aggregate or rewrite a historical
denominator.

The domain contract owns event payloads, closed enums, descriptors, and
validation; the existing telemetry store owns interval coalescing,
prediction/outcome linkage, bounded cells, late correction, retention, and
rebuild; the application boundary owns authorized local record/query use
cases; and Observatory/Costs render the application read model without local
formulas. Direct tests cover binding, rebuild, payload safety, cross-surface
parity, and rendering. Historical file and test names do not constrain that
ownership.

This contract ships through the PR17 executable Work loop: the domain event
types, canonical serialization, exhaustive enum handling, prohibited-field
checks, and idempotency are implemented with the first real owner emission;
deterministic joins, interval union, censoring, fixed buckets,
support/coverage floors, late correction, retention, and rebuild equality are
implemented with the first Work/Observatory/Costs query that consumes them;
Plans 24/32/36/37 emit through the existing application boundary and
CLI/MCP/HTTP/Observatory/Costs return the same descriptors, values,
denominators, coverage, and unavailable reasons; and the aggregate product
test exercises concurrency, duplicate work/effects, conflict outcomes,
integration, stale stacks, blocked intervals, reruns, leaks, fanout,
replay/drop, cardinality, and cross-transport parity on the same production
path. Any duplicate committed effect, prohibited payload in telemetry,
identity-bearing metric label, formula drift, or cross-transport mismatch
blocks acceptance.

### Concrete event and type contract

The canonical domain contract owns retrieval, adoption, performance, and
execution-topology events. The existing observation store persists the common
envelope, builds denominator-safe read models, and applies retention. The
observability application operation is the only write/query boundary. Product
owners instrument their own paths and emit these types; they do not add
another counter store. Historical module locations are not part of the
contract.

`ObservabilityEnvelopeV1` contains `event_id`, `event_kind`,
`schema_revision`, `idempotency_key`, opaque local `trace_id`, authorized
`scope_ref`, capability and operation enums, event and observation time,
duration or quantity and unit, terminal result, producer/configuration/policy/
revisions, watermark, `CoverageStateV1`, sampling probability,
retention class, and emitted/delayed/dropped counts. `CoverageStateV1` is
exactly `Known | Partial | Stale | Unknown | Sampled | Capped`. Attempts and
terminal events have different idempotency identities.

The persisted performance contract includes
`PerformanceMeasurementDescriptorV1`, `BenchmarkRunAggregateV1`,
`PairedEffectEstimateV1`, and `PerformanceDispositionV1`.

The minimum cross-cutting V1 event payloads added by this plan are:

- `RetrievalQueryObservedV1` (`retrieval.query.completed.v1`);
- `RetrievalPlannerObservedV1` (`retrieval.planner.decided.v1`);
- `RetrieverObservedV1` (`retrieval.retriever.completed.v1`);
- `RetrievalSynthesisObservedV1` (`retrieval.synthesis.completed.v1`);
- `RetrievalSourceObservedV1` (`retrieval.source.observed.v1`);
- `ContextOutcomeObservedV1` (`retrieval.context.outcome_linked.v1`);
- `RetrievalAblationObservedV1` (`retrieval.ablation.measured.v1`);
- `AdoptionEligibilityObservedV1` (`adoption.eligibility_observed.v1`);
- `AdoptionOutcomeLinkedV1` (`adoption.outcome_linked.v1`);
- `AnalyticsConsentChangedV1` (`analytics.consent.changed.v1`);
- `OperationResourceObservedV1` (`operation.resource.completed.v1`);
- `NoProgressObservedV1` (`operation.no_progress.terminal.v1`);
- `ExecutionTopologySampledV1`,
  `WorkConflictPredictionObservedV1`, `WorkConflictOutcomeLinkedV1`,
  `WorkIntegrationTransitionObservedV1`, `WorkStackDriftObservedV1`,
  `GitHubStackCapabilityObservedV1`, `WorkDuplicateEffortObservedV1`,
  `WorkBlockedIntervalObservedV1`,
  `WorkRerunObservedV1`, `WorkExecutionLeakObservedV1`, and
  `WorkDeliveryFanoutObservedV1`, with event-kind strings fixed in the
  execution-topology section above;
- `WorkflowRunSourceEventV1`, `WorkflowStageSourceEventV1`,
  `WorkflowEffectSourceEventV1`, `WorkflowRouteSourceEventV1`, and
  `WorkflowRecoverySourceEventV1`, emitted by Plan 32 for run terminal,
  budget exhaustion, queue/backpressure, progress timeout, cancellation,
  effect, retry/recovery, requested/actual route, recursive-dispatch
  rejection, and fan-out observations;
- `BenchmarkRunAttemptedV1`, `BenchmarkRunTerminalV1`, and
  `BenchmarkComparisonRecordedV1`; and
- `TelemetryDropObservedV1` (`telemetry.drop.observed.v1`).

Plans 35–37 and every other owning slice define their additional exhaustive
source-event enum in that slice while using `ObservabilityEnvelopeV1`; omission
from this minimum list is not permission to emit an untyped counter.
Every listed event has canonical serialization/digest and persistence,
replay, late-arrival, and retention coverage. The historical fixture paths are
not required.
Each producer has a saturating in-memory atomic drop count and one reserved
control-lane slot outside the fixed data queue. The next accepted envelope and
shutdown flush carry the accumulated count; `TelemetryDropObservedV1` uses the
reserved slot. A full telemetry queue therefore cannot hide its own drops, and
the counter is not another durable event store. Envelopes also carry a process
boot identity and producer sequence. A boot without a clean terminal envelope
marks coverage from its last persisted sequence through restart `Unknown` and
reports only the proved drop lower bound; abrupt process loss never renders as
zero drops.

### Retrieval, planner, and context measurement

[Plan 15](15-search-quality-evaluation-and-retrieval-research.md) owns search
labels and quality promotion. This plan owns their event schema and read model.
[Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) owns task
planning and outcome identity, while
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md) owns actual scheduling and
runtime receipts. Requested and actual selection, fan-out, route, and outcome
therefore remain separate observations.

`RetrieverObservedV1` references Plan 15's canonical `RetrieverKind` exactly:
`ExactLiteral | Lexical | Semantic | Graph | Temporal | TaskSession |
Diagnostic`. Phrase, BM25, typo recovery, exact flat scan, and ANN are
versioned implementation/profile dimensions inside those lanes; reranking is
a composition stage, not another retriever.
`RetrievalQueryObservedV1` records the pinned query snapshot and profile
revisions, `RetrievalQueryFamilyV1`, authorized scope class, enabled lane set,
scheduled/start/terminal times, total candidate/context/token budgets,
answered/abstained/partial/denied/terminal result, source and lane coverage,
planner/synthesis trace references, emitted/delayed/dropped coverage, and no
query bytes or reversible query digest.
`RetrievalQueryFamilyV1` is `ExactTechnical | Phrase | NaturalLanguage | Typo |
Temporal | Graph | TaskSession | Diagnostic | NoAnswer | Unknown`; adding or
reinterpreting a family increments the schema revision, and an unrecognized
value decodes only as `Unknown`, never into another cohort.
`RetrieverObservedV1` records retriever/profile revision, requested and
consumed candidate budget, raw/eligible/deduplicated/returned candidate
counts, budget/cutoff reason, queue/I/O/parse/model/rank duration, final-top-k
and unique contribution counts, and fixed rank buckets `1`, `2..3`, `4..5`,
`6..10`, `11..25`, `26..50`, and `over_50`. Labeled evaluation additionally
records oracle Recall@N, first-useful rank, relevant selected count, and
marginal Recall@K/nDCG@10. Online traffic without a pinned oracle records those
fields as unavailable, never zero.

At most ten `CandidateRankContributionV1` entries are retained per query.
Only candidates already authorized for the requester may enter this list.
Each contains only an authorization-bound anchor reference, retriever,
pre-rerank and final rank, exact-tier flag, and one
`ContributionKindV1`: `Selected | DuplicateSuppressed | StaleRejected |
BudgetTruncated | RerankedOut`. A denied or forbidden candidate leaves no
anchor, count, rank, cache, event, aggregate influence, or explanation.
Wrong-scope and authorization-leakage rates are computed only inside Plan
15's sanitized authorized evaluation harness and exported as run-level
metrics without candidate references. Raw query or source text, snippets,
scores, logits, margins, embeddings, paths, symbols, or provider payloads are
forbidden. An admitted anchor is a local join key and never an export or
dashboard grouping key.

`RetrievalPlannerObservedV1` records eligible, selected, and excluded
retriever enums with typed reasons; requested/admitted/deferred fan-out;
source and shard counts; candidate/token/time budgets; and selection plus
queue duration. `RetrievalSynthesisObservedV1` records actual fan-out, union,
deduplicated, stale, and final authorized counts; operation-level denied lane
outcomes without candidate counts; per-retriever final contribution; fan-out
wait, merge, dedupe, rerank, hydration, render,
synthesis, critical-path, and total duration; cancellation; and partial/budget
state. Instrumentation remains on the canonical application retrieval pipeline
and its existing retriever and temporal adapters rather than a parallel
measurement path.

Source availability, authorization, and coverage are orthogonal.
`RetrievalSourceAvailabilityV1` is `Unsupported | Absent | Indexing |
Available | Cancelled | TimedOut | Failed`;
`RetrievalFreshnessV1` is `Current | Stale | Unknown`;
`AuthorizationOutcomeV1` is `Allowed | Denied`; and coverage uses
`CoverageStateV1`. `RetrievalSourceObservedV1` records only cataloged source
kind, authority class, generation digest, watermark, fixed freshness bucket,
eligible/searched/returned counts for allowed access, the four states, typed
operation-level denial reason, and drop count. For `Denied`, all source and
candidate counts, generation, watermark, and freshness are unavailable so the
event cannot reveal source or candidate existence. Denial never becomes
absence, stale evidence never becomes an empty result, and complete zero-match requires
`Available + Current + Allowed + Known`.

`ContextOutcomeObservedV1` binds the retrieval profile, context packet,
authorized work/attempt and outcome-label revisions. It records required,
available, included, cited, independently verified relevant, labeled
irrelevant, stale, truncated, and unknown authorized anchor counts plus one
operation-level denial outcome with no candidate/anchor cardinality; presented and
used tokens/bytes; Precision@1/3/5 and required-anchor coverage where labels
exist; time to first valid action; rediscovery reads/searches/tests/tokens;
first-pass status; review independence; accepted correctness; rework; and
outcome coverage. `ContextSupplied | EvidenceCited |
IndependentlyVerifiedUse | NoUseObserved` describes linkage, not causality.
Plan 32 `Completed` and worker self-report cannot produce Plan 26 `Accepted`.
Plan 24 context packets additionally report count and token/byte distributions
by its closed work class and fixture-size stratum, using fixed packet buckets
`0`, `1..1024`, `1025..4096`, `4097..16384`, `16385..65536`, and
`over_65536`; no work title, payload, path, or task identity becomes a metric
label.

`RetrievalAblationObservedV1` pins evaluation run, partition, query stratum,
baseline/candidate profile, oracle/label revisions, enabled retrievers, and
equal total and per-retriever candidate budgets. Allocation and redistribution
rules freeze before the run; unused budget is not silently moved. It reports
Plan 15 Precision@1/3/5, Recall@5/10, MRR, nDCG@10, first-useful rank,
no-answer precision, duplicate/wrong-scope rates, risk/coverage, AURC,
candidate oracle Recall@N before reranking, p50/p95/p99 latency, process-tree
RSS/PSS and separately named cgroup/container high-water evidence, support,
interval, coverage, and disposition. Exact flat-vector scan remains the ANN
oracle. Retrieval, context-outcome, and ablation tests retain these behaviors;
their fixture paths are historical implementation detail.

### Adoption analytics and retention

`AnalyticsModeV1` remains `Off | LocalOnly | AggregateShare`; `LocalOnly` is
the default and `AggregateShare` requires explicit opt-in. `Off` stops optional
adoption collection, `LocalOnly` has no network exporter, and opt-out stops
egress before its configuration operation succeeds. Ordinary bounded retention
and deletion apply to optional analytics; owning product receipts and run
history retain their existing lifecycles and are never exported as adoption
analytics.

Optional local detail expires after 30 days, local rollups after 395 days, and
share staging within 24 hours after opt-out; backup copies expire within 30
days. Local cells below five eligible units are suppressed. Rates require 20
eligible units and 90% coverage; route/model comparisons require 30 eligible
outcomes, 90% coverage, at most 10% censoring, and no unresolved cohort shift.
Shared cells require 100 contribution windows, at most four dimensions, and
one contribution per installation/capability/outcome/day.

The adoption funnel remains `Eligible -> Enabled -> Available -> Invoked ->
Terminal -> IndependentlyUseful -> RepeatUseful` with explicit denominators,
unknown/censored counts, watermarks, horizons, coverage, and intervals.
Display, click, invocation, process completion, self-report, cards closed,
tests run, token volume, and subjective trust do not become success outcomes.

Events, aggregates, exports, fixtures, and drill-downs contain no credentials,
prompts, private source, provider output, query text, paths, patches, logs,
argv/stdin, environment values, or free-form payload labels. Shared output is
aggregate-only and contains no stable user, project, repository, session, task,
trace, or operation identity. Cardinality and result limits remain bounded;
overflow is reported as capped coverage rather than silently dropped.

### Resource accounting and benchmark evidence

`OperationResourceObservedV1` records p50/p95/p99-eligible scheduled-arrival
and service latency; closed `SpanStageV1` queue, store-lock, index-lock, I/O,
parse, projection, model, rank, merge, hydration, synthesis, render, persist,
provider-discovery, provider-negotiation, lease-to-start, context-assembly,
event-ingestion, first-progress, cancellation, terminal, reconnect, and resume
spans; baseline/peak/steady process-tree RSS and PSS plus separately named
container/cgroup high-water evidence; live heap, allocation churn, retained/
fragmented, SQLite-cache, queue/result/generation bytes; user/system CPU;
temporary/database bytes and read/write amplification; input/output/reasoning/
cache tokens; cost amount/currency/pricing revision; and attempted, committed,
reconciled, unknown, prevented-duplicate, and retried effects. Token and cost
values carry `ProviderReported | LocallyMeasured | Estimated | NotApplicable |
Unknown`.
Correctness, safety, latency, resources, tokens, cost, autonomy, and effects
remain separate dimensions.

Plan 32 owns `MonotonicRunDeadline`,
`ConcurrencyPolicyV1.no_progress_timeout`, `ProgressFrontier`, and cancellation
escalation. `NoProgressObservedV1` records the pinned run-deadline identity,
concurrency-policy digest, workflow stage, configured timeout, last committed
frontier and elapsed stall, remaining monotonic run budget, escalation action,
and terminal/effect-reconciliation outcome. A heartbeat never advances the
frontier. Plans 26 and 33 may evaluate timeout precision and resource impact
but cannot create another deadline, reset rule, timer, or escalation policy.

Performance comparison records retain the exact baseline and candidate build,
workload, corpus, environment, oracle, configuration, platform, coverage,
paired outcomes, resource results, and one disposition:
`promote | reject | insufficient_evidence`. A comparison may promote only from
a reproducible accepted baseline and pins the prior rollback profile. Missing
lineage, dirty or incompatible subjects, post-result threshold changes,
coordinated omission, insufficient support, hidden regressions, or incomplete
coverage yield `reject` or `insufficient_evidence` directly; no attestation,
signature, evidence-grade taxonomy, cryptographic local proof, or separate
baseline ceremony is required.

Checked-in performance fixtures use sanitized realistic data and contain no
credentials, prompts, private source, provider payloads, local paths, or
project/user identifiers. PR20 owns the concrete comparison artifact layout
needed by its production journey.

### Canonical review and outcome labels

PR17 uses one Plan 26-owned label schema. Every label records schema revision,
work/acceptance/decomposition identity, attempt and evidence horizon,
valid/observation time, source class, retrieval anchors, coverage/confidence,
actor/reviewer identity where permitted, and conflict/override provenance.

The exhaustive task-outcome lifecycle labels are `Pending`,
`ObservedPartial`, `Reviewable`, `Accepted`, `Rejected`, `Censored`, and
`Unknown`. Review independence is `Independent`, `NonIndependent`,
`Conflicted`, `Missing`, or `Unknown`; review judgment is `Accepted`,
`Rejected`, `Partial`, or `Unknown`. First-pass scope completion, correctness,
test evidence, escaped defects, rework/remediation, autonomy/intervention, and
residual risk are separate measured dimensions with explicit unknown/coverage,
not aliases for the outcome label.

`Accepted` and `Rejected` describe independently evidenced outcome judgment,
not Plan 24 proposal acceptance and not Plan 32 terminal runtime status.
`Censored` names a known observation cutoff such as cancellation,
supersession, lost authority, or unfinished horizon; `Unknown` means the
available evidence cannot classify the outcome. `Partial` review judgment does
not imply `ObservedPartial` task outcome. Plan 32 `Completed`, `Failed`,
`Cancelled`, `TimedOut`, and provider outcomes remain runtime evidence that may
support—but never substitute for—these labels.

Plan 24 owns the graph transition table that consumes the exact label revision
plus acceptance/dependency evidence. Plan 24 may display or branch on these
labels but cannot redefine, coerce, or mint a second review/outcome vocabulary.
Late or corrected evidence appends a new label revision and leaves prior labels
queryable.

### Truthful aggregation

- Bind every numerator to an explicit denominator and eligible population.
- Carry `known`, `partial`, `stale`, `unknown`, `sampled`, and `capped` coverage with watermark and horizon.
- Refuse percentages, savings, success rates, or SLO claims when their denominator or coverage is insufficient.
- Separate zero observed events from absent, delayed, excluded, or unreadable data.
- Preserve methodology and descriptor revision so changed definitions do not rewrite history silently.
- Publish eligible, observed, completed, censored, unknown, excluded,
  overridden, and exploration counts separately. A model/version or route
  ranking is unavailable when missing outcomes, selection bias, version drift,
  or cohort shift could reverse it.
- Keep child-task throughput and quality attributable to the pinned parent and
  initiative, including decomposition, coordination, integration, and review
  overhead. Splitting work cannot improve the denominator by itself.
- Report calibration by estimate dimension and cohort: predicted band or
  interval, observed value, error/coverage, horizon, sample/censoring counts,
  and estimator revision. Never collapse correctness, safety, latency, tokens,
  cost, and autonomy into one reward score.
- Compact immutable evaluation read models record eligible, attempted,
  answered, abstained, denied, unknown, excluded and censored counts;
  per-stratum support/results; intervals; calibration and risk/coverage;
  flaky/indeterminate evidence; deviations; and exactly one
  `promote | reject | insufficient_evidence` disposition. They reuse canonical
  events and anchors and do not form a benchmark service or separate database.

### Required product views

- Ingest and projection lag by source, project, provider, and store authority.
- Latency and availability SLOs with explicit eligible populations and failure classes.
- Capability and surface adoption with active-user, active-project, and invocation denominators.
- `retrieval-quality` shows per-retriever budgets, candidate/rank/contribution,
  source freshness/coverage/denial, planner/fan-out/synthesis spans, context
  precision, task-outcome linkage, and equal-budget ablations.
- `adoption-outcomes` shows the outcome funnel, correct abstention,
  independently useful and retained use; `adoption-coverage` shows eligible
  versus observed, late/dropped/capped, suppression, and denominator failures;
  `analytics-controls` shows local mode, share staging age, retention/deletion,
  and egress failures.
- `performance-budgets` shows p50/p95/p99 with support and intervals,
  queue/lock/provider spans, RSS/CPU/I/O, no-progress outcomes, and accepted
  budget revision; `performance-comparisons` shows baseline/candidate evidence
  and promote/reject/insufficient-evidence disposition.
- Hint emission, delivery, action, usefulness, dismissal, and unknown-outcome funnels.
- Appropriate-reliance views keep accepted-correct, accepted-incorrect,
  rejected-correct, rejected-incorrect, independently verified, override with
  rationale, no eligible verification, and unknown/censored separate.
  Acceptance, clicks, display, or subjective trust are not correctness.
- Automation admission, execution, useful work, effect, recovery, and terminal outcome funnels.
- Task/work graph throughput and quality by eligible task-shape cohort,
  decomposition policy, executor/provider/model/effort, while preserving
  first-pass completion, correctness, tests/review, rework, latency,
  tokens/cost, autonomy, overrides, cancellations, unknown outcomes, and
  evidence coverage as separate dimensions.
- `execution-topology` shows requested/accepted/admitted/active/useful
  concurrency and fanout, independently adjudicated duplicate work,
  mechanical/semantic conflict confusion matrices, ready-to-integrated
  latency, observed native fast-forward/merge/cherry-pick outcomes, stale-stack
  age, GitHub stack capability state and generic-fallback availability, unioned and
  cause-attributed blocked time, runtime/test/CI reruns, duplicate effects,
  operational leaks, and delivery fanout. Every card exposes support,
  eligible denominator, censoring/unknowns, interval coverage, horizon,
  descriptor revision, and safe anchors; unsupported or under-floor metrics
  render unavailable rather than zero.
- Task-intelligence calibration and drift views: estimate versus outcome
  intervals, decomposition and live resize/re-route proposal disposition,
  independent-review coverage, exact model-version boundaries, current versus
  historical cohorts, censoring/selection exposure, abstention/fallback
  reasons, and insufficient-evidence state.
- Auxiliary-provider reliability and cost views by eligible backend,
  executable/protocol/model version, capability and task-shape cohort:
  negotiation availability, explicit fallback, queue/start latency,
  heartbeat/progress and stream coverage, cancellation escalation,
  restart/resume, artifacts, terminal outcomes, tokens/cost, and unknown
  effect. Native Claude Code, Codex app-server, and Codex CLI remain separate
  dimensions; absence or failure of one never counts as success of another.
- Usage, cost, and measured savings with declared pricing inputs, exclusions, and confidence.
- Store, index, daemon, hook, and remote-coverage health derived from canonical facts rather than incidental row presence.
- Diagnostic and analyzer/provider coverage carry the complete canonical state
  set: `unsupported`, `absent`, `indexing`, `stale`, `cancelled`, `timed-out`,
  `failed`, and `partial`. These remain distinct from
  `supported`+`completed`+`complete` zero-findings. Metrics and views never
  collapse any state into a clean empty result, and surface overlay freshness,
  cache reuse, provider conflicts, and host delivery path without leaking
  source, path, or message content.

### Rejected-argument analytics

Consume only the canonical dispatcher event defined by
[PR12](21-cli-mcp-tool-surface-and-output-unification.md); projections never
reparse CLI text, MCP errors, HTTP bodies, or logs. Provide frequency and rate
read models grouped by tool/command, normalized rejected argument name, error
class, schema/version, transport, and, when present, provider, model family,
and agent-host kind. Preserve explicit unknown/unavailable dimensions rather
than inventing attribution.

Every result includes the eligible attempt denominator, horizon, watermark,
schema and projector revision, sampling/capping state, redacted-name count,
and emitted, delayed, dropped, and unreported-event coverage. Rankings and
rates are unavailable when coverage or cardinality controls make them
misleading. Low-frequency dimensions are suppressed or coarsened under the
shared export allowlist; raw values, payloads, prompts, paths, hostnames, user
identifiers, secrets, error text, and reversible token digests are neither
stored nor exposed by drill-down.

The views support evidence-based schema decisions: identify repeated safe
misspellings, obsolete names, transport-specific incompatibilities, and
provider/model/host biases; compare attempted names with the schema active at
event time; and evaluate a proposed alias or help change against a pinned
baseline. They recommend no automatic aliases and never change schemas,
dispatch, or retry behavior. Alias adoption remains an explicit product
decision with collision, ambiguity, and maintenance review.

### Doctor and health

- Doctor, Observatory, CLI, MCP, API, and dashboard consume one typed health and
  remediation kernel owned by PR14. Doctor uses the kernel read-only for
  detection and explanation; remediation remains explicit confirmed operations.
  An alias reports kernel availability; it cannot substitute a private probe or
  claim health from binding presence.
- Replace separate `session_start`/`session_end` baseline tools with one
  health-delta operation over pinned before/after watermarks and coverage.
- Analytics consume canonical versioned events only. Session or surface
  handlers never maintain private counters, outcome rules, or database queries.

### Observatory and Costs

- PR14 exposes shared typed read models through application queries and the
  then-shipped CLI, MCP, HTTP, and dashboard adapters. PR18 adds SDK adapters
  and parity when the official SDKs ship.
- PR17 extends those same adapters with `ExecutionTopologyMetricsV1` and
  Observatory/Costs execution-topology views; it does not add a telemetry
  store, formula, or transport-local projection.
- Backend and UI adapters render application read models; dashboard formulas
  are prohibited. Direct dashboard/API tests preserve Observatory, Costs, and
  execution-topology parity. Historical adapter, panel, and fixture names are
  not mandatory recreation targets.
- Every card, chart, and export shows scope, horizon, freshness, coverage, unit, and denominator.
- Users can drill from an aggregate to safe trace or retrieval anchors and see why data is partial or unknown.
- UI and transports consume the same values; none recompute business metrics locally.

## Acceptance

- Retry, replay, cancellation, timeout, drop, late-arrival, cap, and partial-shard fixtures produce stable non-duplicated outcomes.
- Missing denominators and incomplete coverage render unknown or partial on every transport, never zero or 100%.
- Aggregates reconcile to canonical events for pinned watermarks and remain reproducible after projector rebuilds.
- Lag, SLO, adoption, hint, automation, usage, cost, and savings fixtures verify units, populations, horizons, and exclusions.
- Observatory, CLI, MCP, HTTP, and exports pass value and coverage parity tests
  in PR14; PR18 SDK conformance adds the same parity fixtures for each shipped
  SDK.
- Product-path tests prove events and drill-down anchors contain no prohibited
  raw content.
- Execution-topology contract tests serialize and round-trip every persisted
  event kind and enum above; schema checks reject every prohibited identity/
  content field and every free-form metric label. Product-path coverage proves
  topology, operation, event, supported-platform, retention, source-event, and
  rollup behavior without requiring a one-row-per-case matrix.
- Concurrency/fanout fixtures reconcile requested, Plan 24-accepted, Plan
  32-admitted, active, useful, serialized, blocked, and capacity-deferred
  widths over event-time intervals. Heartbeats, queue presence, duplicate
  work, provider-native children, and hook/LSP/dashboard delivery do not
  inflate useful concurrency; duplicate delivery is measured only in the
  delivery-fanout projection.
- Duplicate-work fixtures distinguish independently adjudicated exact/
  superseded/repeated effort from similarity and proximity; preserve
  not-duplicate/censored/unknown outcomes; report wall/tokens/cost/tests/
  effects separately; and prove task splitting, retries, redelivery, or
  duplicate-effect prevention cannot improve the denominator. Duplicate
  committed effects and prohibited telemetry payloads reject promotion.
- Conflict fixtures build separate mechanical, semantic, and combined
  confusion matrices from predictions made before integration and
  independent outcomes. They include true/false positive and negative,
  abstained, unknown, stale, partial, denied, corrected, late, and censored
  cases; precision/recall is unavailable below 50 cases, 90% coverage, or
  above 10% censoring and never reclassifies a clean mechanical result as a
  clean semantic outcome.
- Integration fixtures prove ready-to-integrated starts at the first exact
  Plan 24 Ready transition and ends only at exact native target containment;
  supersession, target drift, cancellation, missing receipt, and incomplete
  horizon censor. Observed merge success remains separate from required test/
  CI completion, Plan 24 acceptance, escaped defects, and TraceDecay operation
  support; Plan 36 preview-only results never emit fictional apply events, and
  apply events require the exact Plan 36 product-runtime owner receipt, never a
  PR acceptance receipt.
- Stale-stack and blocked-time fixtures coalesce duplicate/overlapping
  intervals deterministically, keep open intervals visible at the watermark,
  preserve per-cause overlap separately from unioned wall time, and handle
  head/base/merge-base/worktree-generation drift, retarget, ref deletion,
  branch retention, late events, and projector rebuild without rewriting
  history.
- Rerun fixtures require a new linked runtime/test/CI identity, preserve cause
  and original denominator, and reject log replay, SSE redelivery, the same
  attempt, or a renamed branch as a rerun. Leak fixtures cover lease-after-
  terminal, ownerless attempt, effect-unknown past deadline, missing worktree
  binding, unbounded delivery, none, and unknown with exact
  recovery/coverage semantics.
- Cardinality, retention, and parity fixtures enforce fixed width/time
  buckets, eight grouping dimensions, 4,096 local cells/day, 256 returned
  cells, minimum-five suppression, 30-day detail and 395-day rollup retention,
  source-event anchor cap, and the stated rate/support floors. CLI, MCP, HTTP,
  Observatory, Costs, and exports return identical values, denominators,
  horizons, coverage, and unavailable reasons without a UI-local formula.
- Retrieval fixtures prove per-retriever counts reconcile to planner and
  synthesis totals; ranks and contribution cap deterministically; equal-budget
  ablations cannot transfer unused budget; source state, authorization, and
  coverage remain orthogonal; and missing labels make precision/contribution
  unavailable rather than zero.
- Context fixtures prove precision and required-anchor coverage link to exact
  Plan 15 label and Plan 24 work/outcome revisions without claiming causality
  or treating Plan 32 completion as acceptance.
- Adoption fixtures prove `Off` and `LocalOnly` make no network request,
  shared cells meet suppression/contribution caps, all rates satisfy support
  and coverage floors, opt-out blocks egress before its receipt and purges
  staging transactionally, deletion and backup expiry meet their 24-hour and
  30-day bounds, and activity-only vanity metrics cannot render as useful
  outcomes.
- Resource fixtures reconcile scheduled-arrival latency, every span, RSS,
  tokens/cost evidence class, and effect state; no-progress fixtures prove a
  heartbeat alone cannot extend the progress frontier or synthesize success.
- Comparison fixtures prove reproducible baseline lineage, frozen thresholds,
  no fabricated or sentinel baseline, no coordinated omission, protected-
  stratum visibility, and aggregate-only Git artifacts.
- Plan 24 routing-review fixtures prove cohort eligibility, minimum sample and
  coverage, policy/evidence revisions, requested-versus-actual route,
  independent outcome evidence, exploration/fallback state, and override
  attribution rebuild deterministically. Small/private cohorts are suppressed;
  prompts, source, symbols, paths, review bodies, private session content, and
  hidden reasoning never enter route metrics. Missing or shifted evidence
  cannot produce a confident recommendation or hide a deterministic fallback.
- Task-intelligence fixtures preserve calibrated size bands, first-pass
  identity, parent-normalized decomposition/integration overhead, independent
  versus self review, exact model-version cohorts, valid/observation time,
  censored/unknown outcomes, selection/override/exploration exposure, and
  proposal disposition through replay and late evidence. Cold-start, sparse,
  shifted, or high-censoring populations produce bounded fallback/abstention
  rather than a success rank; task splitting and cheap self-reports cannot
  improve quality denominators.
- Topology, route, escalation, recall, handoff, verifier, and
  appropriate-reliance fixtures preserve the dimensions above, exact
  model/version cohorts, intervals/set width, selection propensity,
  calibration validity, drift and censoring. They prove Plan 26 supplies
  labels and measurements only: it never recommends policy, mutates Plan 24,
  schedules Plan 32, or creates another Doctor.
- Review/outcome schema fixtures exhaust every label, legal evidence
  requirement, independence/judgment combination, runtime-versus-outcome
  distinction, censored-versus-unknown case, late correction, schema-version
  replay, and missing/conflicting coverage. Cross-plan fixtures prove Plan 24
  consumes the same revision for graph transitions and cannot create a local
  label, while Plan 32 process completion or worker self-report alone never
  yields `Accepted`.
- Auxiliary-provider fixtures reconcile fake/native negotiation, attempt,
  stream, cancellation, resume, artifact, and terminal events without
  double-counting retries or fallback. Version drift, malformed/truncated
  streams, secret/shell-injection canaries, daemon restart, missing
  heartbeats, and explicit app-server-to-CLI fallback preserve truthful
  coverage and requested-versus-actual identity; raw argv/stdin/output/env and
  secrets never enter observations or drill-down anchors.
- Git fixtures prove patch, path, commit-message, author, and conflict content
  never enters telemetry while attempts, typed outcomes, latency, and dropped
  coverage remain truthful.
- LSP fixtures reconcile session, request, analyzer, cache, diagnostic,
  coverage, drop, and reconnect events while proving paths, source, symbols,
  and messages never enter telemetry.
- Analyzer/provider coverage fixtures exercise every canonical state
  (`unsupported`, `absent`, `indexing`, `stale`, `cancelled`, `timed-out`,
  `failed`, `partial`, and `supported`+`completed`+`complete` zero-findings)
  in required product views and prove none collapse to clean empty. Table-driven
  parity/coverage tests verify Observatory, CLI, MCP, HTTP, and exports render
  the same state labels, denominators, and non-zero coverage semantics.
- Rejected-argument fixtures reconcile exact frequencies and eligible-attempt
  rates by tool/command, safe rejected name, error class, schema/version,
  transport, provider, model family, and agent-host kind for pinned watermarks.
- Equivalent CLI, MCP, and HTTP rejections project to the same dimensions;
  retry/replay does not double count, and late or out-of-order events rebuild
  deterministically.
- Secret-bearing `--name=value`, positional, malformed, oversized,
  high-cardinality, non-UTF-8, and private-identifier fixtures prove that no
  value or prohibited token reaches canonical events, aggregates, exports, or
  drill-down while redacted-name counts remain truthful.
- Drop, daemon-unavailable, sampling, cap, suppression, missing-attribution,
  and schema-upgrade fixtures expose partial/unknown coverage and never render
  absence as zero; removed-name and misspelling fixtures support reproducible
  alias/schema analysis without changing dispatch behavior.
- [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)
  fixtures reconcile staged emission: PR11 cycle trigger/stage/terminal/budget/
  dedupe/latency events; PR12 CLI/MCP/HTTP/LSP delivery/truncation/expansion
  events; PR13 GitHub item/thread lifecycle (`current`, `outdated`, `resolved`,
  `edited`, `deleted`) and ingress provider outcome (`complete`, `partial`,
  `unavailable`, `denied`, `rate_limited`, `stale`, `failed`), CI localization
  provenance without log payloads, proximity emitted/suppressed/expired/risk-
  class dimensions plus pinned Plan 20 threshold revision/digest, and host-adapter
  state; PR14 Observatory/Doctor read-model parity across transports. Table-driven
  fixtures cover the separate exhaustive GitHub lifecycle and ingress outcome
  sets, plus the Plan 35 provider states (unsupported, absent, indexing, stale,
  cancelled, timed-out, failed, partial versus supported plus completed plus
  complete-coverage zero-findings), and
  LSP projection lifecycle/outcome labels consistent with Plans 37 and 35.
  Truncation/expansion handle/anchor usage and failure counts carry explicit
  denominators. Outbound-write fixtures emit only separate `policy=denied` and
  `effect=suppressed` observations before any GitHub call, never ingress state;
  no metric claims a posted, updated, dismissed, or replied GitHub comment,
  while observed read-only `resolved` remains a required lifecycle value.
- Repository checks reject alternate counter writers, UI-local formulas, and meta-plan instrumentation.

Tests may consolidate fixtures and reorganize harnesses when they retain every
observable state, denominator, platform, lifecycle, privacy, correction,
retention, rebuild, and cross-surface regression above. Historical fixture
counts, names, and matrix layouts are not acceptance artifacts.
