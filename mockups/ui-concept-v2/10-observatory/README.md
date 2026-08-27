# Observatory concept plates

## Purpose

Observatory sources with independent coverage and failure boundaries, never an invented heartbeat.

Route: `/observatory`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns the shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- A concept plate is not a production authority. The shipping workspace derives each state from its named production response, transport, and authorization paths; otherwise it is unavailable.

## Canonical semantic-state matrix

| Required semantic or interaction state | Current explainer | Entry condition |
|---|---|---|
| Overview | [v4-overview-honest.md](v4-overview-honest.md) | Open `/observatory` after one or more source authorities respond. |
| Index progress | No current plate | Required semantic coverage; no current plate. |
| Storage findings | No current plate | Required semantic coverage; no current plate. |
| Empty | No current plate | Required semantic coverage; no current plate. |
| Partial | No current plate | Required semantic coverage; no current plate. |
| Stale | No current plate | Required semantic coverage; no current plate. |
| Baseline pending | No current plate | Required semantic coverage; no current plate. |
| Unsupported | No current plate | Required semantic coverage; no current plate. |
| Blocked progress | No current plate | Required semantic coverage; no current plate. |

Rows marked “No current plate” are required coverage, not implied by the selected image.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v0-radial-first.png](v0-radial-first.png) | [v0-radial-first.md](v0-radial-first.md) | `superseded` | Earlier Observatory lookbook iteration; replaced by canonical `v4-overview-honest`. |
| [v1-radial.png](v1-radial.png) | [v1-radial.md](v1-radial.md) | `superseded` | Earlier Observatory lookbook iteration; replaced by canonical `v4-overview-honest`. |
| [v2-overview-stack.png](v2-overview-stack.png) | [v2-overview-stack.md](v2-overview-stack.md) | `superseded` | Earlier Observatory lookbook iteration; replaced by canonical `v4-overview-honest`. |
| [v3-hud-pass-dark.png](v3-hud-pass-dark.png) | [v3-hud-pass-dark.md](v3-hud-pass-dark.md) | `superseded` | Earlier Observatory lookbook iteration; replaced by canonical `v4-overview-honest`. |
| [v3-hud-pass-light.png](v3-hud-pass-light.png) | [v3-hud-pass-light.md](v3-hud-pass-light.md) | `superseded` | Earlier Observatory lookbook iteration; replaced by canonical `v4-overview-honest`. |
| [v4-overview-honest.png](v4-overview-honest.png) | [v4-overview-honest.md](v4-overview-honest.md) | `current` | Pre-Task-1 canonical selection for Observatory. |

## Historical decisions

The pre-Task-1 canonical table selects v4; previous studies are superseded.
