# Code concept plates

## Purpose

CORTEX, TRACE, and CORE with named graph, symbol, freshness, and diagnostic boundaries.

Route: `/code`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/code/CodePage.tsx` reads `/api/plugins/graph/{overview,search,subgraph}` with `GraphOverviewPayloadV1Schema`, `GraphSearchPayloadV1Schema`, and `GraphSubgraphPayloadV1Schema`; `CodeDiagnostics.tsx` and `IndexFreshness.tsx` own state readouts.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Trace lens selected | [v3-lenses.md](v3-lenses.md) | Depicted by this selected still. |
| Direct-lens graph | [v3-lenses.md](v3-lenses.md) | Depicted by this selected still. |
| Selected-symbol evidence | [v3-lenses.md](v3-lenses.md) | Depicted by this selected still. |
| Served strata | [v3-lenses.md](v3-lenses.md) | Depicted by this selected still. |
| Fresh branch/index | [v3-lenses.md](v3-lenses.md) | Depicted by this selected still. |
| Build progress | [v3-lenses.md](v3-lenses.md) | Depicted by this selected still. |
| Diagnostics warming | [v3-lenses.md](v3-lenses.md) | Depicted by this selected still. |
| Diagnostics stale | [v3-lenses.md](v3-lenses.md) | Depicted by this selected still. |
| Unavailable generation | [v3-lenses.md](v3-lenses.md) | Depicted by this selected still. |
| Diagnostics ready | [v3-lenses.md](v3-lenses.md) | Depicted by this selected still. |
| Registry/stream state matrix | [v3-lenses.md](v3-lenses.md) | Depicted by this selected still. |
| Loaded Cortex content | No current plate | Required interaction/result is not depicted. |
| Loaded Core content | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

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
