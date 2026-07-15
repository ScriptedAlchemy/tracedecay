# TraceDecay V2 Observability, Accounting, and Usage Plan

## Status / role

Cross-cutting instrumentation is implemented with each owning product slice. PR14 completes the Observatory and Costs experience over the resulting canonical read models. This plan is a product observability contract, not a plan compiler or delivery tracker.

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

### Observatory and Costs

- PR14 exposes shared typed read models through application queries and thin CLI, MCP, HTTP, SDK, and dashboard adapters.
- Every card, chart, and export shows scope, horizon, freshness, coverage, unit, and denominator.
- Users can drill from an aggregate to safe trace or retrieval anchors and see why data is partial or unknown.
- UI and transports consume the same values; none recompute business metrics locally.

## Acceptance

- Retry, replay, cancellation, timeout, drop, late-arrival, cap, and partial-shard fixtures produce stable non-duplicated outcomes.
- Missing denominators and incomplete coverage render unknown or partial on every transport, never zero or 100%.
- Aggregates reconcile to canonical events for pinned watermarks and remain reproducible after projector rebuilds.
- Lag, SLO, adoption, hint, automation, usage, cost, and savings fixtures verify units, populations, horizons, and exclusions.
- Observatory, CLI, MCP, HTTP, SDK, and exports pass value and coverage parity tests.
- Privacy fixtures prove events and drill-down anchors contain no prohibited raw content.
- Repository checks reject alternate counter writers, UI-local formulas, and meta-plan instrumentation.
