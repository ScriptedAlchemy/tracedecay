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
| Priced provider spend | [v3-provider-spend.md](v3-provider-spend.md) | Depicted by this selected still. |
| Unpriced/null spend | [v3-provider-spend.md](v3-provider-spend.md) | Depicted by this selected still. |
| Absent provider values | [v3-provider-spend.md](v3-provider-spend.md) | Depicted by this selected still. |
| Count-only saved tokens | [v3-provider-spend.md](v3-provider-spend.md) | Depicted by this selected still. |
| Loading/partial/stale/unavailable/denied/measured-empty legend | [v3-provider-spend.md](v3-provider-spend.md) | Depicted by this selected still. |
| A time-range change result | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-provider-burn.png](v1-provider-burn.png) | [v1-provider-burn.md](v1-provider-burn.md) | `superseded` | Earlier Costs lookbook iteration; replaced by canonical `v3-provider-spend`. |
| [v2-hud-pass-dark.png](v2-hud-pass-dark.png) | [v2-hud-pass-dark.md](v2-hud-pass-dark.md) | `superseded` | Earlier Costs lookbook iteration; replaced by canonical `v3-provider-spend`. |
| [v2-hud-pass-light.png](v2-hud-pass-light.png) | [v2-hud-pass-light.md](v2-hud-pass-light.md) | `superseded` | Earlier Costs lookbook iteration; replaced by canonical `v3-provider-spend`. |
| [v3-provider-spend.png](v3-provider-spend.png) | [v3-provider-spend.md](v3-provider-spend.md) | `current` | Pre-Task-1 canonical selection for Costs. |

## Historical decisions

The pre-Task-1 canonical table selects v3; previous studies are superseded.
