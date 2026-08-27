# Explorer concept plates

## Purpose

Independent source lanes and an honest search lifecycle.

Route: `/explorer`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns the shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- A concept plate is not a production authority. The shipping workspace derives each state from its named production response, transport, and authorization paths; otherwise it is unavailable.

## Canonical semantic-state matrix

| Required semantic or interaction state | Current explainer | Entry condition |
|---|---|---|
| Browse lanes | [v4-lane-lifecycle.md](v4-lane-lifecycle.md) | Open `/explorer` and begin a supported search. |
| Search progress | No current plate | Required semantic coverage; no current plate. |
| Cancelled | No current plate | Required semantic coverage; no current plate. |
| Result inspector | No current plate | Required semantic coverage; no current plate. |
| Complete empty | No current plate | Required semantic coverage; no current plate. |
| Partial | No current plate | Required semantic coverage; no current plate. |
| Stale | No current plate | Required semantic coverage; no current plate. |
| Offline | No current plate | Required semantic coverage; no current plate. |
| Source unavailable | No current plate | Required semantic coverage; no current plate. |
| Error | No current plate | Required semantic coverage; no current plate. |

Rows marked “No current plate” are required coverage, not implied by the selected image.

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
