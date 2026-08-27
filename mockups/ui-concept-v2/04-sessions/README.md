# Sessions concept plates

## Purpose

Provider-qualified session inspection with coverage and redaction boundaries.

Route: `/sessions`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/sessions/SessionsPage.tsx` reads `/api/plugins/hermes-lcm/{overview,timeline,search}` through `LcmOverviewPayloadV1Schema` and `LcmTimelinePayloadV1Schema`; `dashboard/src/workspaces/sessions/SessionInspector.tsx` owns detail.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Transcript search chrome | [v3-provenance-inspector.md](v3-provenance-inspector.md) | Depicted by this selected still. |
| Provider-qualified session list | [v3-provenance-inspector.md](v3-provenance-inspector.md) | Depicted by this selected still. |
| Active, ended, and failed rows | [v3-provenance-inspector.md](v3-provenance-inspector.md) | Depicted by this selected still. |
| Selected raw inspector | [v3-provenance-inspector.md](v3-provenance-inspector.md) | Depicted by this selected still. |
| Page-two pagination | [v3-provenance-inspector.md](v3-provenance-inspector.md) | Depicted by this selected still. |
| Coverage truncated/paginated | [v3-provenance-inspector.md](v3-provenance-inspector.md) | Depicted by this selected still. |
| Redaction none | [v3-provenance-inspector.md](v3-provenance-inspector.md) | Depicted by this selected still. |
| Store serving | [v3-provenance-inspector.md](v3-provenance-inspector.md) | Depicted by this selected still. |
| State legend | [v3-provenance-inspector.md](v3-provenance-inspector.md) | Depicted by this selected still. |
| A submitted transcript-search result | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

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
