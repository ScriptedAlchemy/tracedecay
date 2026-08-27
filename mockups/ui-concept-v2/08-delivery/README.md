# Delivery concept plates

## Purpose

Local Git and provider delivery as independent authorities rather than a healthy pipeline.

Route: `/delivery`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns the shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- A concept plate is not a production authority. The shipping workspace derives each state from its named production response, transport, and authorization paths; otherwise it is unavailable.

## Canonical semantic-state matrix

| Required semantic or interaction state | Current explainer | Entry condition |
|---|---|---|
| Repository field | [v3-independent-authorities.md](v3-independent-authorities.md) | Open `/delivery` after registry response or explicit repository selection. |
| Selected repository | No current plate | Required semantic coverage; no current plate. |
| Pipeline | No current plate | Required semantic coverage; no current plate. |
| Empty registry | No current plate | Required semantic coverage; no current plate. |
| Stale | No current plate | Required semantic coverage; no current plate. |
| Failed | No current plate | Required semantic coverage; no current plate. |
| Rate-limited | No current plate | Required semantic coverage; no current plate. |
| Denied | No current plate | Required semantic coverage; no current plate. |
| Unavailable | No current plate | Required semantic coverage; no current plate. |
| Unknown branches | No current plate | Required semantic coverage; no current plate. |

Rows marked “No current plate” are required coverage, not implied by the selected image.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-recency-field.png](v1-recency-field.png) | [v1-recency-field.md](v1-recency-field.md) | `superseded` | Earlier Delivery lookbook iteration; replaced by canonical `v3-independent-authorities`. |
| [v2-hud-pass-dark.png](v2-hud-pass-dark.png) | [v2-hud-pass-dark.md](v2-hud-pass-dark.md) | `superseded` | Earlier Delivery lookbook iteration; replaced by canonical `v3-independent-authorities`. |
| [v2-hud-pass-light.png](v2-hud-pass-light.png) | [v2-hud-pass-light.md](v2-hud-pass-light.md) | `superseded` | Earlier Delivery lookbook iteration; replaced by canonical `v3-independent-authorities`. |
| [v3-independent-authorities.png](v3-independent-authorities.png) | [v3-independent-authorities.md](v3-independent-authorities.md) | `current` | Pre-Task-1 canonical selection for Delivery. |

## Historical decisions

The pre-Task-1 canonical table selects v3; previous studies are superseded.
