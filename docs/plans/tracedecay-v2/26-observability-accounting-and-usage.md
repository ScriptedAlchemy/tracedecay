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
