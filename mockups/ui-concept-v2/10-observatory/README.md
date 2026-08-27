# Observatory concept plates

## Purpose

Observatory sources with independent coverage and failure boundaries, never an invented heartbeat.

Route: `/observatory`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/observatory/ObservatoryPage.tsx` reads `/api/storage/telemetry`, `/api/code-index/freshness`, `/api/observatory`, and analytics diagnostics; `DoctorInspector.tsx`, `CanonicalObservations.tsx`, `PerformanceBudgets.tsx`, and `HookHints.tsx` own panels.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Doctor findings | [v4-overview-honest.md](v4-overview-honest.md) | Depicted by this selected still. |
| Doctor partial/stale | [v4-overview-honest.md](v4-overview-honest.md) | Depicted by this selected still. |
| Observed/omitted/unavailable families | [v4-overview-honest.md](v4-overview-honest.md) | Depicted by this selected still. |
| Budget under/over/unmeasured | [v4-overview-honest.md](v4-overview-honest.md) | Depicted by this selected still. |
| Hook coverage partial | [v4-overview-honest.md](v4-overview-honest.md) | Depicted by this selected still. |
| Store measured | [v4-overview-honest.md](v4-overview-honest.md) | Depicted by this selected still. |
| Store unavailable | [v4-overview-honest.md](v4-overview-honest.md) | Depicted by this selected still. |
| Registry/stream/pulse matrix | [v4-overview-honest.md](v4-overview-honest.md) | Depicted by this selected still. |
| A refetch or recovery result | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

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
