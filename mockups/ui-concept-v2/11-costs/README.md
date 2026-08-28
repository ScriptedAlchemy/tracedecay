# Costs concept plates

## Purpose

Actual provider spend and coverage without treating unpriced/null as zero.

Route: `/costs`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/costs/CostsPage.tsx` reads `/api/plugins/savings/overview` with `SavingsOverviewPayloadV1Schema`; `CanonicalCosts.tsx`, `TopologyMetricsCosts.tsx`, and `spend.ts` preserve spend/coverage distinctions.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Priced provider spend | [final/01-provider-spend-attribution.md](final/01-provider-spend-attribution.md) | Canonical usage joins an applicable provider/model price for the selected UTC range. |
| Unpriced, null/unknown, or unavailable pricing | [final/01-provider-spend-attribution.md](final/01-provider-spend-attribution.md) | Usage exists but no canonical applicable price, value, or pricing source is available. |
| Project/model/session attribution | [final/01-provider-spend-attribution.md](final/01-provider-spend-attribution.md) | Select a provider, project, model, session, or topology row. |
| Count-only saved tokens | [final/01-provider-spend-attribution.md](final/01-provider-spend-attribution.md) | Avoided usage can be counted but cannot be priced on a common canonical basis. |
| Budget status and coverage | [final/01-provider-spend-attribution.md](final/01-provider-spend-attribution.md) | A budget is configured and spend coverage for its scope is known or explicitly partial. |
| Loading/partial/stale/unavailable/denied/measured-empty | [final/01-provider-spend-attribution.md](final/01-provider-spend-attribution.md) | Usage, pricing, budget, or attribution authority reports its independent typed state. |
| A time-range change result | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
