---
design_status: current
evidence_class: concept_synthetic
---

# Costs — provider spend attribution

- **Asset:** `01-provider-spend-attribution.png`
- **Route:** `/costs`
- **Lifecycle:** authoritative final concept

## User job

Understand actual provider spend and usage for the selected scope and time range, see which sessions/projects/models/topologies caused it, compare it with configured budgets, and recognize where price coverage is insufficient to make a dollar claim.

## Product behavior

- The primary chart shows actual priced spend over an explicit UTC range. Provider totals reconcile to the selected range and the detailed ledger.
- Project, model, session, and execution-topology tables explain attribution. Selecting a provider or row cross-filters the other views and preserves the query in the URL.
- Usage events and token quantities are facts independent of pricing. “Saved tokens” is count-only unless a canonical pricing authority can price both the observed and avoided usage on the same basis.
- Budgets show configured amount, scope, period, spend-to-date, remaining/overage, and freshness. Missing or invalid budgets are not treated as unlimited.
- Pricing classes stay distinct: priced, unpriced, null/unknown, unavailable, denied, stale, and measured-empty. Unpriced usage contributes to usage totals but never silently contributes `$0` to spend.

## Production authorities

- `CostsPage.tsx` reads `/api/plugins/savings/overview` through `SavingsOverviewPayloadV1Schema`.
- `CanonicalCosts.tsx`, `TopologyMetricsCosts.tsx`, and `spend.ts` preserve spend, usage, coverage, and topology distinctions.
- Provider usage records own token/event quantities and provider/model/session identities. Canonical provider pricing pages own rate applicability and effective dates. Budget configuration owns limits. Attribution joins must state exact, inferred, ambiguous, stale, or missing identity.
- Shared shell and typed-state behavior come from [navigation](../../NAVIGATION.md), [design system](../../DESIGN-SYSTEM.md), and [interaction states](../../INTERACTION-STATES.md).

## Canonical evidence ladder

From strongest to weakest: billed or canonical priced usage with exact provider/model/rate window; measured usage joined exactly to an applicable canonical rate; measured usage with no applicable price; count-only derived savings; inferred attribution; stale pricing; unavailable, denied, null, or missing input. Dollar totals may include only priced rows, and must always disclose pricing coverage.

Unknown price, missing price, and zero cost are three different states. Likewise, no usage, unavailable usage, and measured-empty are not interchangeable.

## Interaction and scale contract

- Keyboard: provider legend, chart series, attribution rows, and range controls are operable without pointer input. Arrow keys traverse series/rows; Enter scopes; Escape clears the latest scope.
- Reduced motion: animated traces and count-up effects become static lines, endpoints, and labels; no spend meaning depends on motion.
- 200% zoom/reflow: provider authority becomes an inline section, tables scroll within labeled regions, chart data remains available as text, and currency/model labels do not truncate silently.
- Dense data: virtualize long provider/model/session ledgers, aggregate chart buckets deterministically, preserve exact totals, and disclose aggregation granularity.
- Exact fallback: expose provider usage, pricing application, project/model/session attribution, budget, and coverage tables; offer a source-qualified export where the production route exists.

## Truth boundary

This reviewed plate is `CONCEPT / SYNTHETIC DATA`. Every pictured dollar amount, token count, provider, date, coverage percentage, and latency is illustrative. Production dollars require canonical usage plus applicable pricing authority. Missing pricing or attribution remains unknown; the UI must not invent prices, convert absent values to zero, or claim savings that cannot be priced.
