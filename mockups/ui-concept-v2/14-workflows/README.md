# Workflows concept plates

## Purpose

Workflow definitions and lifecycle evidence with immutable versions, pinned digests, and constrained run lookup.

Route: `/workflows`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns the shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- A concept plate is not a production authority. The shipping workspace derives each state from its named production response, transport, and authorization paths; otherwise it is unavailable.

## Canonical semantic-state matrix

| Required semantic or interaction state | Current explainer | Entry condition |
|---|---|---|
| Definitions | [v3-definition-ledger.md](v3-definition-ledger.md) | Open `/workflows` after the definition registry responds. |
| Detail | No current plate | Required semantic coverage; no current plate. |
| Lifecycle CAS | No current plate | Required semantic coverage; no current plate. |
| Run lookup | No current plate | Required semantic coverage; no current plate. |
| Empty registry | No current plate | Required semantic coverage; no current plate. |
| Unknown | No current plate | Required semantic coverage; no current plate. |
| Runtime unavailable | No current plate | Required semantic coverage; no current plate. |
| CAS conflict | No current plate | Required semantic coverage; no current plate. |
| Concealed run | No current plate | Required semantic coverage; no current plate. |

Rows marked “No current plate” are required coverage, not implied by the selected image.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-lifecycle-tracks.png](v1-lifecycle-tracks.png) | [v1-lifecycle-tracks.md](v1-lifecycle-tracks.md) | `superseded` | Earlier Workflows lookbook iteration; replaced by canonical `v3-definition-ledger`. |
| [v2-hud-pass-dark.png](v2-hud-pass-dark.png) | [v2-hud-pass-dark.md](v2-hud-pass-dark.md) | `superseded` | Earlier Workflows lookbook iteration; replaced by canonical `v3-definition-ledger`. |
| [v2-hud-pass-light.png](v2-hud-pass-light.png) | [v2-hud-pass-light.md](v2-hud-pass-light.md) | `superseded` | Earlier Workflows lookbook iteration; replaced by canonical `v3-definition-ledger`. |
| [v3-definition-ledger.png](v3-definition-ledger.png) | [v3-definition-ledger.md](v3-definition-ledger.md) | `current` | Pre-Task-1 canonical selection for Workflows. |

## Historical decisions

The pre-Task-1 canonical table selects v3; previous studies are superseded.
