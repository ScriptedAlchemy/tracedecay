# Work concept plates

## Purpose

Six cameras over one immutable product-graph version; unavailable cameras stay unserved.

Route: `/work`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/work/WorkPage.tsx` uses `useWorkGraphViews`, `useWorkAttempts`, and `useWorkTopology`; `dashboard/src/workspaces/work/views/WorkProjectionSwitcher.tsx`, `WorkBoard.tsx`, and `workViewsModel.ts` bind projections to one graph read.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Board selected | [v4-six-cameras.md](v4-six-cameras.md) | Depicted by this selected still. |
| Immutable graph revision | [v4-six-cameras.md](v4-six-cameras.md) | Depicted by this selected still. |
| Ready task | [v4-six-cameras.md](v4-six-cameras.md) | Depicted by this selected still. |
| Blocked task | [v4-six-cameras.md](v4-six-cameras.md) | Depicted by this selected still. |
| Unavailable task | [v4-six-cameras.md](v4-six-cameras.md) | Depicted by this selected still. |
| Measured empty | [v4-six-cameras.md](v4-six-cameras.md) | Depicted by this selected still. |
| DAG unavailable | [v4-six-cameras.md](v4-six-cameras.md) | Depicted by this selected still. |
| Timeline unavailable | [v4-six-cameras.md](v4-six-cameras.md) | Depicted by this selected still. |
| Causal unavailable | [v4-six-cameras.md](v4-six-cameras.md) | Depicted by this selected still. |
| Workload unavailable | [v4-six-cameras.md](v4-six-cameras.md) | Depicted by this selected still. |
| Topology unavailable | [v4-six-cameras.md](v4-six-cameras.md) | Depicted by this selected still. |
| A projection switch result | No current plate | Required interaction/result is not depicted. |
| A task-selection result | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-nine-routes.png](v1-nine-routes.png) | [v1-nine-routes.md](v1-nine-routes.md) | `superseded` | Earlier Work lookbook iteration; replaced by canonical `v4-six-cameras`. |
| [v2-six-cameras.png](v2-six-cameras.png) | [v2-six-cameras.md](v2-six-cameras.md) | `superseded` | Earlier Work lookbook iteration; replaced by canonical `v4-six-cameras`. |
| [v3-hud-pass-dark.png](v3-hud-pass-dark.png) | [v3-hud-pass-dark.md](v3-hud-pass-dark.md) | `superseded` | Earlier Work lookbook iteration; replaced by canonical `v4-six-cameras`. |
| [v3-hud-pass-light.png](v3-hud-pass-light.png) | [v3-hud-pass-light.md](v3-hud-pass-light.md) | `superseded` | Earlier Work lookbook iteration; replaced by canonical `v4-six-cameras`. |
| [v4-six-cameras.png](v4-six-cameras.png) | [v4-six-cameras.md](v4-six-cameras.md) | `current` | Pre-Task-1 canonical selection for Work. |

## Historical decisions

The pre-Task-1 canonical table selects v4; previous studies are superseded.
