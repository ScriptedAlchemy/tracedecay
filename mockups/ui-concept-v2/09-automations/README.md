# Automations concept plates

## Purpose

Scheduler ledger and run evidence without an invented approvals surface.

Route: `/automations`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns the shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- A concept plate is not a production authority. The shipping workspace derives each state from its named production response, transport, and authorization paths; otherwise it is unavailable.

## Canonical semantic-state matrix

| Required semantic or interaction state | Current explainer | Entry condition |
|---|---|---|
| Overview | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | Open `/automations` after scheduler and run-ledger authorities respond. |
| Scheduler pause/resume | No current plate | Required semantic coverage; no current plate. |
| Run artifacts | No current plate | Required semantic coverage; no current plate. |
| Empty | No current plate | Required semantic coverage; no current plate. |
| Partial list | No current plate | Required semantic coverage; no current plate. |
| Offline | No current plate | Required semantic coverage; no current plate. |
| Denied | No current plate | Required semantic coverage; no current plate. |
| Unavailable | No current plate | Required semantic coverage; no current plate. |
| Artifact mismatch | No current plate | Required semantic coverage; no current plate. |

Rows marked “No current plate” are required coverage, not implied by the selected image.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-cron-strip.png](v1-cron-strip.png) | [v1-cron-strip.md](v1-cron-strip.md) | `superseded` | Earlier Automations lookbook iteration; replaced by canonical `v3-scheduler-ledger`. |
| [v2-hud-pass-dark.png](v2-hud-pass-dark.png) | [v2-hud-pass-dark.md](v2-hud-pass-dark.md) | `superseded` | Earlier Automations lookbook iteration; replaced by canonical `v3-scheduler-ledger`. |
| [v2-hud-pass-light.png](v2-hud-pass-light.png) | [v2-hud-pass-light.md](v2-hud-pass-light.md) | `superseded` | Earlier Automations lookbook iteration; replaced by canonical `v3-scheduler-ledger`. |
| [v3-scheduler-ledger.png](v3-scheduler-ledger.png) | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | `current` | Pre-Task-1 canonical selection for Automations. |

## Historical decisions

The pre-Task-1 canonical table selects v3; previous studies are superseded.
