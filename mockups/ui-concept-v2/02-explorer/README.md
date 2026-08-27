# Explorer concept plates

## Purpose

Independent source lanes and an honest search lifecycle.

Route: `/explorer`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/explorer/ExplorerPage.tsx` composes `controller.ts`, `laneModel.ts`, `absence.ts`, and `Inspector.tsx`; they are the source-local lane and inspector authority.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Create submitted | [v4-lane-lifecycle.md](v4-lane-lifecycle.md) | Depicted by this selected still. |
| Poll running | [v4-lane-lifecycle.md](v4-lane-lifecycle.md) | Depicted by this selected still. |
| Cancel available | [v4-lane-lifecycle.md](v4-lane-lifecycle.md) | Depicted by this selected still. |
| Code lane ready | [v4-lane-lifecycle.md](v4-lane-lifecycle.md) | Depicted by this selected still. |
| Sessions lane partial | [v4-lane-lifecycle.md](v4-lane-lifecycle.md) | Depicted by this selected still. |
| Knowledge lane measured empty | [v4-lane-lifecycle.md](v4-lane-lifecycle.md) | Depicted by this selected still. |
| Semantic lane absent | [v4-lane-lifecycle.md](v4-lane-lifecycle.md) | Depicted by this selected still. |
| Selected code-hit inspector | [v4-lane-lifecycle.md](v4-lane-lifecycle.md) | Depicted by this selected still. |
| Lane/run-state legend | [v4-lane-lifecycle.md](v4-lane-lifecycle.md) | Depicted by this selected still. |
| A completed cancellation result | No current plate | Required interaction/result is not depicted. |
| A timed-out lane result | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-three-lanes.png](v1-three-lanes.png) | [v1-three-lanes.md](v1-three-lanes.md) | `superseded` | Earlier Explorer lookbook iteration; replaced by canonical `v4-lane-lifecycle`. |
| [v2-four-lanes.png](v2-four-lanes.png) | [v2-four-lanes.md](v2-four-lanes.md) | `superseded` | Earlier Explorer lookbook iteration; replaced by canonical `v4-lane-lifecycle`. |
| [v3-hud-pass-dark.png](v3-hud-pass-dark.png) | [v3-hud-pass-dark.md](v3-hud-pass-dark.md) | `superseded` | Earlier Explorer lookbook iteration; replaced by canonical `v4-lane-lifecycle`. |
| [v3-hud-pass-light.png](v3-hud-pass-light.png) | [v3-hud-pass-light.md](v3-hud-pass-light.md) | `superseded` | Earlier Explorer lookbook iteration; replaced by canonical `v4-lane-lifecycle`. |
| [v4-lane-lifecycle.png](v4-lane-lifecycle.png) | [v4-lane-lifecycle.md](v4-lane-lifecycle.md) | `current` | Pre-Task-1 canonical selection for Explorer. |

## Historical decisions

The pre-Task-1 canonical table selects v4; previous studies are superseded.
