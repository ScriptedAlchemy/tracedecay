# Costs concept plates

## Purpose

Actual provider spend and coverage without treating unpriced/null as zero.

Route: `/costs`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns the shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- A concept plate is not a production authority. The shipping workspace derives each state from its named production response, transport, and authorization paths; otherwise it is unavailable.

## Canonical semantic-state matrix

| Required semantic or interaction state | Current explainer | Entry condition |
|---|---|---|
| Actual spend | [v3-provider-spend.md](v3-provider-spend.md) | Open `/costs` after spend and coverage authorities respond. |
| Coverage | No current plate | Required semantic coverage; no current plate. |
| Empty ledger | No current plate | Required semantic coverage; no current plate. |
| Partial coverage | No current plate | Required semantic coverage; no current plate. |
| Source unavailable | No current plate | Required semantic coverage; no current plate. |
| Pricing unavailable | No current plate | Required semantic coverage; no current plate. |
| Identity unknown | No current plate | Required semantic coverage; no current plate. |

Rows marked “No current plate” are required coverage, not implied by the selected image.

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
