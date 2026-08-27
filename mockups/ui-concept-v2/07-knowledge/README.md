# Knowledge concept plates

## Purpose

Independent Facts, Geometry, Curation, and Oplog cameras without invented geometry or trust.

Route: `/knowledge`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/knowledge/KnowledgePage.tsx` uses generated `MemoryOverviewPayloadV1Schema`, `MemoryStatusPayloadV1Schema`, and `MemoryFactDetailPayloadV1Schema`; `MemoryGeometry.tsx`, `CurationConsole.tsx`, and `MemoryOplog.tsx` own camera reads.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Facts admitted/disputed/withheld | [v4-four-cameras.md](v4-four-cameras.md) | Depicted by this selected still. |
| Geometry unavailable | [v4-four-cameras.md](v4-four-cameras.md) | Depicted by this selected still. |
| Method not PCA | [v4-four-cameras.md](v4-four-cameras.md) | Depicted by this selected still. |
| Curation partial | [v4-four-cameras.md](v4-four-cameras.md) | Depicted by this selected still. |
| Curation pending/applied/rejected | [v4-four-cameras.md](v4-four-cameras.md) | Depicted by this selected still. |
| Oplog loading | [v4-four-cameras.md](v4-four-cameras.md) | Depicted by this selected still. |
| Oplog partial | [v4-four-cameras.md](v4-four-cameras.md) | Depicted by this selected still. |
| Oplog stale | [v4-four-cameras.md](v4-four-cameras.md) | Depicted by this selected still. |
| Oplog unavailable | [v4-four-cameras.md](v4-four-cameras.md) | Depicted by this selected still. |
| Registry/stream state matrix | [v4-four-cameras.md](v4-four-cameras.md) | Depicted by this selected still. |
| A completed curation mutation | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-single-view.png](v1-single-view.png) | [v1-single-view.md](v1-single-view.md) | `superseded` | Earlier Knowledge lookbook iteration; replaced by canonical `v4-four-cameras`. |
| [v2-four-cameras.png](v2-four-cameras.png) | [v2-four-cameras.md](v2-four-cameras.md) | `superseded` | Earlier Knowledge lookbook iteration; replaced by canonical `v4-four-cameras`. |
| [v3-hud-pass-dark.png](v3-hud-pass-dark.png) | [v3-hud-pass-dark.md](v3-hud-pass-dark.md) | `superseded` | Earlier Knowledge lookbook iteration; replaced by canonical `v4-four-cameras`. |
| [v3-hud-pass-light.png](v3-hud-pass-light.png) | [v3-hud-pass-light.md](v3-hud-pass-light.md) | `superseded` | Earlier Knowledge lookbook iteration; replaced by canonical `v4-four-cameras`. |
| [v4-four-cameras.png](v4-four-cameras.png) | [v4-four-cameras.md](v4-four-cameras.md) | `current` | Pre-Task-1 canonical selection for Knowledge. |

## Historical decisions

The pre-Task-1 canonical table selects v4; previous studies are superseded.
