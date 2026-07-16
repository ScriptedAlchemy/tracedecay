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
- Denominator-safe projections and Observatory/Costs read models.
- Product-wide lag, SLO, adoption, hint-outcome, and automation-outcome definitions.
- Trace and retrieval anchors needed to explain aggregate results without exposing private content.

## Does not own

- A separate telemetry database, scheduler log, workflow event stream, or per-surface counter system.
- Product execution, retries, admission, policy, or side effects.
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
  telemetry covers feedback-cycle trigger, termination reason, loop-iteration
  count, GitHub review-thread ingestion states (ingested, remapped, outdated,
  resolved, deleted, suppressed — never posted), CI-failure localization
  states and typed provenance without log content, concurrent-agent proximity
  warning emission/suppression/expiry/risk class, and
  truncation/expansion handle/anchor usage and failures without payloads.
  PR13 emits GitHub/CI/proximity and feedback-cycle events; PR14 completes
  Observatory/Doctor read models over them. All metrics remain
  denominator-safe. Telemetry contains no source, diagnostic message, comment
  body, CI log content, or private session content.
- Identify scope, capability, operation, result, event and observation time, duration or quantity, unit, producer revision, trace, and privacy classification.
- Use stable idempotency keys so retries and replay cannot double count.
- Record terminal outcomes separately from attempts and preserve cancellation, rejection, timeout, partial success, and unknown outcomes.
- Keep instrumentation bounded and non-blocking while making dropped or delayed telemetry measurable.

### Truthful aggregation

- Bind every numerator to an explicit denominator and eligible population.
- Carry `known`, `partial`, `stale`, `unknown`, `sampled`, and `capped` coverage with watermark and horizon.
- Refuse percentages, savings, success rates, or SLO claims when their denominator or coverage is insufficient.
- Separate zero observed events from absent, delayed, excluded, or unreadable data.
- Preserve methodology and descriptor revision so changed definitions do not rewrite history silently.

### Required product views

- Ingest and projection lag by source, project, provider, and store authority.
- Latency and availability SLOs with explicit eligible populations and failure classes.
- Capability and surface adoption with active-user, active-project, and invocation denominators.
- Hint emission, delivery, action, usefulness, dismissal, and unknown-outcome funnels.
- Automation admission, execution, useful work, effect, recovery, and terminal outcome funnels.
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
  fixtures reconcile GitHub ingestion states (ingested, remapped, outdated,
  resolved, deleted, suppressed), CI localization provenance without log
  payloads, proximity emitted/suppressed/expired/risk-class dimensions, and
  truncation/expansion handle/anchor usage/failure counts with explicit
  denominators. PR13 emission and PR14 Observatory/Doctor read-model parity
  fixtures verify the same state labels and coverage semantics across
  transports; no metric claims a posted GitHub comment.
- Repository checks reject alternate counter writers, UI-local formulas, and meta-plan instrumentation.
