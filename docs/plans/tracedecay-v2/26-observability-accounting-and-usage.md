# TraceDecay V2 Observability, Accounting, and Usage Plan

## Status / role

Cross-cutting instrumentation is implemented with each owning product slice. PR14 completes the Observatory and Costs experience over the resulting canonical read models. This plan is a product observability contract, not a plan compiler or delivery tracker.
Its versioned measurements and coverage semantics are the canonical product
telemetry input to [PR20 performance optimization](33-end-to-end-performance-optimization.md).
Versioned benchmark, profiler, and operating-system measurements remain valid
PR20 evidence under that plan's measurement contract.

## Outcome

Every operational and product metric states what was measured, over which population and horizon, at which watermark, with what coverage. Unknown, partial, stale, sampled, or capped data stays visible and can never render as a trustworthy zero.

## Owns

- Canonical accounting, usage, latency, outcome, and health event contracts.
- Metric descriptors, units, populations, horizons, coverage, and aggregation semantics.
- Versioned quantifier descriptors, cohort definitions, coverage/uncertainty
  semantics, temporal baselines/deltas, calibration/drift observations,
  privacy-safe outcome linkage, and optional decision-policy evidence. A
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
- Raw provider payloads or unsanitized content.
- A source parser, Markdown parser, compatibility inventory, plan ledger, generated execution graph, or meta compiler.
- UI-local metric formulas or transport-specific metric meanings.

## Required behavior

### Canonical events

- Emit versioned events through the same authoritative event/store path as other V2 observations.
- Emit privacy-safe [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)
  events for sessions; methods, outcomes, and latency; queueing and
  cancellation; analyzer startup, restart, and indexing/degraded state; cache
  reuse and overlay freshness; diagnostic add and clear; provider conflicts;
  host delivery path; partial coverage and drops; and bridge reconnect.
- LSP telemetry contains no paths, source, symbols, or diagnostic messages.
- [Plan 36](36-git-aware-change-context-and-index-transactions.md) telemetry may
  identify the operation kind and privacy-safe outcome only. Patch content,
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
  third set. Attempted outbound GitHub writes emit separate `policy=denied`
  and `effect=suppressed` observations before any call, never a lifecycle or
  ingress value. No `posted`, `updated`, `dismissed`, or `replied` lifecycle
  exists; `resolved` is the observed read-only lifecycle value. All metrics remain
  denominator-safe. Telemetry contains no source, path, diagnostic message,
  comment body, CI log content, or private session content.
- Identify scope, capability, operation, result, event and observation time, duration or quantity, unit, producer revision, trace, and privacy classification.
- Use stable idempotency keys so retries and replay cannot double count.
- Record terminal outcomes separately from attempts and preserve cancellation, rejection, timeout, partial success, and unknown outcomes.
- Keep instrumentation bounded and non-blocking while making dropped or delayed telemetry measurable.
- PR17 emits privacy-safe Plan 24/32 observations for task-shape and
  decomposition grade and estimate ranges; proposal/decision identity;
  requested/actual model route and exact model/version/effort/tool/host
  capability; first-pass scope completion and accepted correctness;
  tests/review independence and finding severity; escaped defects;
  rework/remediation and parent/integration overhead; retries and typed causes;
  queue/execution latency; tokens/cost/resources; autonomy; human
  intervention/override; cancellation; censoring/unknown horizon; and audit
  coverage. Records pin work/acceptance/decomposition, estimator, cohort,
  policy/config/catalog/privacy, evidence horizon, and valid/observation-time
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

### Concrete event and type contract

The canonical domain contract lives in
`crates/tracedecay-domain/src/observability/mod.rs`,
`crates/tracedecay-domain/src/observability/retrieval.rs`,
`crates/tracedecay-domain/src/observability/adoption.rs`,
`crates/tracedecay-domain/src/observability/performance.rs`, and
`crates/tracedecay-domain/src/observability/attestation.rs`.
`crates/tracedecay-store/src/observation/telemetry.rs` persists the common
envelope, `crates/tracedecay-store/src/observation/telemetry_projection.rs`
builds denominator-safe read models, and
`crates/tracedecay-store/src/observation/telemetry_retention.rs` applies the
retention policy. `src/application/observability/{mod,record,query,privacy}.rs`
is the only application write/query boundary. Product owners instrument their
own paths and emit these types; they do not add another counter store.

`ObservabilityEnvelopeV1` contains `event_id`, `event_kind`,
`schema_revision`, `idempotency_key`, opaque local `trace_id`, authorized
`scope_ref`, capability and operation enums, event and observation time,
duration or quantity and unit, terminal result, producer/configuration/policy/
privacy revisions, watermark, `CoverageStateV1`, sampling probability,
retention class, and emitted/delayed/dropped counts. `CoverageStateV1` is
exactly `Known | Partial | Stale | Unknown | Sampled | Capped`. Attempts and
terminal events have different idempotency identities.

`PerformanceMeasurementDescriptorV1`, `BenchmarkRunAggregateV1`,
`PairedEffectEstimateV1`, and `PerformanceDispositionV1` are defined in
`performance.rs`; `BenchmarkBaselineAttestationV1`,
`BenchmarkComparisonAttestationV1`, and their
`BenchmarkAttestationV1` enum are defined in `attestation.rs`.

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
- `WorkflowRunSourceEventV1`, `WorkflowStageSourceEventV1`,
  `WorkflowEffectSourceEventV1`, `WorkflowRouteSourceEventV1`, and
  `WorkflowRecoverySourceEventV1`, emitted by Plan 32 for run terminal,
  budget exhaustion, queue/backpressure, progress timeout, cancellation,
  effect, retry/recovery, requested/actual route, recursive-dispatch
  rejection, and fan-out observations;
- `BenchmarkRunAttemptedV1`, `BenchmarkRunTerminalV1`,
  `BenchmarkAttestationRecordedV1`, `BenchmarkBaselineAcceptedV1`,
  `BenchmarkBaselineRevokedV1`, and `BenchmarkAttestationSupersededV1`; and
- `TelemetryDropObservedV1` (`telemetry.drop.observed.v1`).

Plans 35–37 and every other owning slice define their additional exhaustive
source-event enum in that slice while using `ObservabilityEnvelopeV1`; omission
from this minimum list is not permission to emit an untyped counter.
Every listed event has canonical serialization and digest fixtures in
`crates/tracedecay-domain/tests/observability_contract.rs`; persistence,
replay, late arrival, and retention fixtures live in
`crates/tracedecay-store/tests/observability_projection.rs`.
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
state. Instrumentation is implemented in
`src/application/retrieval/pipeline.rs`,
`src/query/retrieval/{exact,lexical,semantic,graph,temporal,task_session,diagnostic,fusion,dedupe,diversity,rerank,hydrate}.rs`,
and the existing `src/query/temporal/` kernel through its temporal adapter.

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
oracle. Fixtures live in
`tests/observability_suite/{retrieval,context_outcomes,ablation}.rs`.

### Privacy-safe adoption and retention

`AnalyticsModeV1` is `Off | LocalOnly | AggregateShare`; `LocalOnly` is the
default and `AggregateShare` requires explicit opt-in. `Off` prevents optional
adoption event serialization, projection, and egress. Mandatory operational
and audit receipts, Plan 24 outcome evidence, and Plan 32 run/effect history
remain under their owning entity lifetime and deletion contracts; they cannot
be disabled without breaking product authority, are excluded from adoption
read models in `Off`, and are never exported by adoption analytics.
`LocalOnly` permits authorized local drill-down and has no network exporter.
`AggregateShare` emits weekly aggregate contribution
packets without anchors or stable installation, actor, user, project,
repository, session, trace, task, or operation identity.

The adoption funnel is `Eligible -> Enabled -> Available -> Invoked ->
Terminal -> IndependentlyUseful -> RepeatUseful`. Every stage reports both its
previous-stage and original-eligible denominator, exclusions, unknown and
censored counts, watermark, horizon, coverage, and interval. Repeat useful use
means another independently useful outcome in a later seven-day window;
28-day retained useful use is a separate projection. Display, click, raw
invocation, process completion, self-report, cards closed, tests run, token
volume, and subjective trust are never product-success outcomes. Search uses
Plan 15 relevance/correct-abstention labels; task adoption uses Plan 24 work
identity and the canonical outcome labels below.

Retention classes are `OptionalLocalDetail30d`, `PrivateBenchmarkRaw30d`,
`LocalRollup395d`, `ShareStaging24h`, `OwningEntityLifetime`, and
`PromotedBenchmarkAggregate`. Opt-out stops egress synchronously before its
configuration receipt returns and purges share staging in the same daemon
transaction; local deletion tombstones or crypto-shreds optional adoption
observations and rollups within 24 hours. Backup copies expire within 30 days.
Sanitized benchmark aggregates survive only when the user explicitly promotes
them; private raw benchmark evidence expires after 30 days unless a shorter
project retention applies. Already unlinkable shared
aggregates cannot be retracted, and that limitation is disclosed before
opt-in.

`crates/tracedecay-store/src/observation/telemetry_retention.rs` owns one
hourly daemon sweep with a persisted per-authority watermark, idempotent
tombstone/crypto-shred receipt, bounded batch and retry, and restart resume.
Opt-out destroys the optional-detail and share-staging encryption keys in its
configuration transaction, so daemon downtime cannot restore access; the
sweep physically removes unreadable rows within 24 hours of cumulative daemon
availability. A missed physical deadline or failed backup-expiry receipt is a
Plan 14 Doctor finding and keeps aggregate sharing disabled.

Local views collapse cells below five eligible units. A rate requires at least
20 eligible units and 90% observed coverage. Exact route/model comparisons
require at least 30 eligible outcomes, 90% outcome coverage, no more than 10%
censoring, and no unresolved cohort/version shift. Shared cells require at
least 100 contribution-windows, permit at most four dimensions, and cap each
installation at one contribution per capability/outcome/day. Shared
dimensions are only cataloged capability (maximum 64), surface, host family,
major/minor product version, OS family, outcome class, coverage class, and
eight fixed latency buckets. Unknown remains `unknown`; overflow becomes
`other`. Active-user and active-project denominators are local-only because
shared packets contain no stable identity.

No event, local aggregate, export, or drill-down contains query text, prompts,
source, snippets, symbols, paths, diagnostic or error messages, review bodies,
private session content, hidden reasoning, patches, commit messages, authors,
conflicts, argv/stdin values, stdout/stderr, environment values, secrets,
hostnames, CI logs, raw provider payloads, free-form labels, or reversible
digests of them. Events permit at most 16 retriever/source rows and 32
`SpanStageV1` rows; overflow aggregates into an `other` cell and sets coverage
to `Capped`. A descriptor/horizon retains at most eight local grouping
dimensions, 4,096 local cells per daily bucket, and 1,024 catalog identities
per projection epoch; overflow aggregates into `other` while exact details
remain only in authorized owning history. Queries return at most 256 cells.
Exact provider/model/executable versions are catalog IDs and remain authorized
local dimensions.

Privacy/configuration and retention operations are implemented in
`src/application/observability/privacy.rs`. The shared aggregate serializer is
`src/application/observability/adoption_share.rs`. Secret-canary, mode,
suppression, purge, backup-expiry, and no-egress fixtures live in
`tests/observability_suite/{privacy,adoption,retention,aggregate_share}.rs`.

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

`BenchmarkAttestationV1` is an enum. `BenchmarkBaselineAttestationV1` contains
one subject commit/tree/content identity, suite, supported baseline decision,
population/unit/horizon, workload/corpus/environment/oracle/harness/clock/
schema/configuration/threshold digests, protocol, raw lineage, coverage,
correctness gates, and evidence grade. Baseline acceptance is not an embedded
boolean; it is the separate CAS transition event below.
`BenchmarkComparisonAttestationV1` adds an independently accepted baseline
attestation reference, candidate identity, paired outcomes and ablations, and
`promote | reject | insufficient_evidence`.
Baseline lifecycle is `Recorded -> Accepted -> Superseded | Revoked`.
Acceptance, supersession, and revocation require expected attestation revision,
actor, reason, and compare-and-swap receipt. A comparison pins the exact
accepted baseline revision and prior accepted rollback profile. Revocation
before a comparison decision invalidates that comparison; later revocation
preserves the historical decision but blocks new promotion and rollout.
`BenchmarkAttestationSupersededV1` names the replacement baseline and every
comparison made ineligible.
`EvidenceGradeV1` is:

- `Clean`: immutable clean subject trees, verified digests and raw aggregate
  lineage, required platforms/strata/support/coverage, acceptable A/A noise,
  frozen thresholds and intervals, and a valid measurement protocol;
- `Provisional`: structurally valid and privacy-safe, but dirty tree, missing
  required platform or confirmation cohort, insufficient tail support,
  excessive censoring/noise, estimated required resource evidence, or partial
  coverage; its disposition is always `insufficient_evidence`; or
- `Rejected`: placeholder or invalid digest, missing/fabricated baseline,
  mismatched comparison identity, absent lineage, post-result threshold
  change, coordinated-omission/survivor-bias defect, protected-stratum hiding,
  or leakage from the evidence collection/artifact itself.

A clean attestation may still disposition `reject`; clean describes evidence
integrity, not candidate quality. A measured candidate correctness, privacy,
authorization, or recovery failure is clean evidence with `reject` and also
triggers the hard rollback gate. Plan 15 outcomes map without loss:
`invalid_run -> Rejected/insufficient_evidence`; `blocked`, `inconclusive`, and
`runtime_fallback_observed -> Provisional/insufficient_evidence`;
`rejected -> Clean/reject`; and `accepted -> Clean/promote`.

`ProjectStoreLayout::benchmark_run_dir(suite_id, attestation_id)` owns local
`manifest.json`, `runs.jsonl`, profiler artifacts, and private oracle
references under the resolved project store. The search suite uses Plan 15's
exact checked-in contract:
`fixture-manifest-v1.json`, `queries-v1.jsonl`, `snapshots-v1.jsonl`,
`judgments-development-v1.jsonl`, `locked-judgments-v1.json`,
`temporal-events-v1.jsonl`, `context-spans-v1.jsonl`, `tasks-v1.jsonl`,
`evidence-index.json`, `run-v1.json`, and `promotion-v1.json`, all under
`benchmarks/search-quality/`. Sanitization replaces repository, branch,
worktree, commit/tree/ref, snapshot, allowed-scope, query, anchor,
contamination, prompt, and task identities with fixture-local random aliases;
source/store/projection generations and watermarks, temporal event IDs,
labeler/adjudicator provenance, authorized-store locators, profile/model/tool
IDs, approval actors, and activation receipts are also replaced or omitted;
the alias map and HMAC keys for authorized private-query/prompt locators remain
only in the private oracle store. Checked-in values cannot be joined back to a
local project without that authorized map. PR20 uses its exact Plan 33
artifact paths. Evidence indexes contain safe aliases, digests, and
authorization-bound anchors, never local paths or payloads. Classification,
digest-mutation, baseline lifecycle/CAS, placeholder, threshold-freeze,
privacy, and supersession fixtures live in
`tests/observability_suite/attestation.rs`.

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
  `analytics-privacy` shows local mode, share staging age, retention/deletion,
  and egress failures.
- `performance-budgets` shows p50/p95/p99 with support and intervals,
  queue/lock/provider spans, RSS/CPU/I/O, no-progress outcomes, and accepted
  budget revision; `benchmark-attestations` shows clean/provisional/rejected
  evidence separately from promote/reject/insufficient-evidence disposition.
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
shared privacy policy; raw values, payloads, prompts, paths, hostnames, user
identifiers, secrets, error text, and reversible token digests are neither
stored nor exposed by drill-down.

The views support evidence-based schema decisions: identify repeated safe
misspellings, obsolete names, transport-specific incompatibilities, and
provider/model/host biases; compare attempted names with the schema active at
event time; and evaluate a proposed alias or help change against a pinned
baseline. They recommend no automatic aliases and never change schemas,
dispatch, or retry behavior. Alias adoption remains an explicit product
decision with collision, ambiguity, maintenance, and privacy review.

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
- Backend adapters are `src/dashboard/observatory_api.rs` and
  `src/dashboard/costs_api.rs`. The UI implementations are
  `dashboard/observatory/src/{entry,api,types,ObservatoryPage}.tsx`,
  `dashboard/observatory/src/{Retrieval,Adoption,Performance,Attestation}Panel.tsx`,
  and `dashboard/costs/src/{entry,api,types,CostsPage}.tsx`. Dashboard formulas
  are prohibited; these files render application read models.
- Dashboard/API parity fixtures are
  `tests/dashboard_api_test/{observatory,costs}.rs` and
  `dashboard/test/{observatory,costs}.vitest.tsx`.
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
- Privacy fixtures prove events and drill-down anchors contain no prohibited raw content.
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
- Attestation fixtures prove clean/provisional/rejected classification,
  immutable digests, independent baseline lineage, frozen thresholds, no
  fabricated or sentinel baseline, no coordinated omission, protected-stratum
  visibility, and aggregate-only Git artifacts.
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
