# Agents concept plates

## Purpose

Agent relationships through independent usage, tree, handoff, token, and failure authorities.

Route: `/agents`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns the shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- A concept plate is not a production authority. The shipping workspace derives each state from its named production response, transport, and authorization paths; otherwise it is unavailable.

## Canonical semantic-state matrix

| Required semantic or interaction state | Current explainer | Entry condition |
|---|---|---|
| Overview | [v3-authority-tree.md](v3-authority-tree.md) | Open `/agents` after the relevant authority responds. |
| Delegation/handoff | No current plate | Required semantic coverage; no current plate. |
| Failure context | No current plate | Required semantic coverage; no current plate. |
| Empty store | No current plate | Required semantic coverage; no current plate. |
| No delegation | No current plate | Required semantic coverage; no current plate. |
| Partial attempt coverage | No current plate | Required semantic coverage; no current plate. |
| Denied | No current plate | Required semantic coverage; no current plate. |
| Unavailable | No current plate | Required semantic coverage; no current plate. |

Rows marked “No current plate” are required coverage, not implied by the selected image.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-host-tree.png](v1-host-tree.png) | [v1-host-tree.md](v1-host-tree.md) | `superseded` | Earlier Agents lookbook iteration; replaced by canonical `v3-authority-tree`. |
| [v2-hud-pass-dark.png](v2-hud-pass-dark.png) | [v2-hud-pass-dark.md](v2-hud-pass-dark.md) | `superseded` | Earlier Agents lookbook iteration; replaced by canonical `v3-authority-tree`. |
| [v2-hud-pass-light.png](v2-hud-pass-light.png) | [v2-hud-pass-light.md](v2-hud-pass-light.md) | `superseded` | Earlier Agents lookbook iteration; replaced by canonical `v3-authority-tree`. |
| [v3-authority-tree.png](v3-authority-tree.png) | [v3-authority-tree.md](v3-authority-tree.md) | `current` | Pre-Task-1 canonical selection for Agents. |

## Historical decisions

The pre-Task-1 canonical table selects v3; previous studies are superseded.
