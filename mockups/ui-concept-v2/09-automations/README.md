# Automations concept plates

## Purpose

Scheduler ledger and run evidence without an invented approvals surface.

Route: `/automations`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/automations/AutomationsPage.tsx`, `RunHistory.tsx`, and `AutomationsPage.transport.dom.test.tsx` own the scheduler/run read models and transport states.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Scheduler paused/configured | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | Depicted by this selected still. |
| Write-scope lock | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | Depicted by this selected still. |
| Scheduler unavailable retry | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | Depicted by this selected still. |
| Due and skip readings | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | Depicted by this selected still. |
| Malformed job error | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | Depicted by this selected still. |
| Managed skills active/disabled | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | Depicted by this selected still. |
| Receipt applied/quarantined | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | Depicted by this selected still. |
| Run outcomes and write denial | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | Depicted by this selected still. |
| Artifact integrity failed | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | Depicted by this selected still. |
| Authority/state key | [v3-scheduler-ledger.md](v3-scheduler-ledger.md) | Depicted by this selected still. |
| A pause or resume mutation | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

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
