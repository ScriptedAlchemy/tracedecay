# Sessions concept plates

## Purpose

Provider-qualified session inspection with coverage and redaction boundaries.

Route: `/sessions`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns the shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- A concept plate is not a production authority. The shipping workspace derives each state from its named production response, transport, and authorization paths; otherwise it is unavailable.

## Canonical semantic-state matrix

| Required semantic or interaction state | Current explainer | Entry condition |
|---|---|---|
| Timeline/list | [v3-provenance-inspector.md](v3-provenance-inspector.md) | Open `/sessions` after a timeline/list response. |
| Transcript search | No current plate | Required semantic coverage; no current plate. |
| Paged inspector | No current plate | Required semantic coverage; no current plate. |
| Empty window | No current plate | Required semantic coverage; no current plate. |
| Partial page | No current plate | Required semantic coverage; no current plate. |
| Offline | No current plate | Required semantic coverage; no current plate. |
| Store unavailable | No current plate | Required semantic coverage; no current plate. |
| Temporal unavailable | No current plate | Required semantic coverage; no current plate. |
| Token count unknown | No current plate | Required semantic coverage; no current plate. |

Rows marked “No current plate” are required coverage, not implied by the selected image.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-inspector.png](v1-inspector.png) | [v1-inspector.md](v1-inspector.md) | `superseded` | Earlier Sessions lookbook iteration; replaced by canonical `v3-provenance-inspector`. |
| [v2-hud-pass-dark.png](v2-hud-pass-dark.png) | [v2-hud-pass-dark.md](v2-hud-pass-dark.md) | `superseded` | Earlier Sessions lookbook iteration; replaced by canonical `v3-provenance-inspector`. |
| [v2-hud-pass-light.png](v2-hud-pass-light.png) | [v2-hud-pass-light.md](v2-hud-pass-light.md) | `superseded` | Earlier Sessions lookbook iteration; replaced by canonical `v3-provenance-inspector`. |
| [v3-provenance-inspector.png](v3-provenance-inspector.png) | [v3-provenance-inspector.md](v3-provenance-inspector.md) | `current` | Pre-Task-1 canonical selection for Sessions. |

## Historical decisions

The pre-Task-1 canonical table selects v3; previous studies are superseded.
