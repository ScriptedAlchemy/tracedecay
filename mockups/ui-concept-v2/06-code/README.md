# Code concept plates

## Purpose

CORTEX, TRACE, and CORE with named graph, symbol, freshness, and diagnostic boundaries.

Route: `/code`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns the shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- A concept plate is not a production authority. The shipping workspace derives each state from its named production response, transport, and authorization paths; otherwise it is unavailable.

## Canonical semantic-state matrix

| Required semantic or interaction state | Current explainer | Entry condition |
|---|---|---|
| Cortex | [v3-lenses.md](v3-lenses.md) | Open `/code` after the index serves the selected model. |
| Graph hover | No current plate | Required semantic coverage; no current plate. |
| Trace | No current plate | Required semantic coverage; no current plate. |
| Core | No current plate | Required semantic coverage; no current plate. |
| Index empty | No current plate | Required semantic coverage; no current plate. |
| Stale index | No current plate | Required semantic coverage; no current plate. |
| Warming index | No current plate | Required semantic coverage; no current plate. |
| Renderer unavailable | No current plate | Required semantic coverage; no current plate. |
| Diagnostics unavailable | No current plate | Required semantic coverage; no current plate. |

Rows marked “No current plate” are required coverage, not implied by the selected image.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-cortex.png](v1-cortex.png) | [v1-cortex.md](v1-cortex.md) | `superseded` | Earlier Code lookbook iteration; replaced by canonical `v3-lenses`. |
| [v2-hud-pass-dark.png](v2-hud-pass-dark.png) | [v2-hud-pass-dark.md](v2-hud-pass-dark.md) | `superseded` | Earlier Code lookbook iteration; replaced by canonical `v3-lenses`. |
| [v2-hud-pass-light.png](v2-hud-pass-light.png) | [v2-hud-pass-light.md](v2-hud-pass-light.md) | `superseded` | Earlier Code lookbook iteration; replaced by canonical `v3-lenses`. |
| [v3-lenses.png](v3-lenses.png) | [v3-lenses.md](v3-lenses.md) | `current` | Pre-Task-1 canonical selection for Code. |

## Historical decisions

The pre-Task-1 canonical table selects v3; previous studies are superseded.
