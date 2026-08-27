# Settings concept plates

## Purpose

Effective configuration boundaries without fabricated provenance or remote query.

Route: `/settings`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/settings/SettingsPage.tsx` decodes `/api/settings` with `SettingsPayloadV1Schema` and `DashboardEnvelopeV1Schema`; `SettingsEditorController.tsx`, `MultiRootPanel.tsx`, and `RemoteBrainPanel.tsx` own write gates and remote state.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Effective values | [v4-effective-only.md](v4-effective-only.md) | Depicted by this selected still. |
| Writable rows | [v4-effective-only.md](v4-effective-only.md) | Depicted by this selected still. |
| System-locked rows | [v4-effective-only.md](v4-effective-only.md) | Depicted by this selected still. |
| Multi-root advertised/no query | [v4-effective-only.md](v4-effective-only.md) | Depicted by this selected still. |
| Remote Brain reachable | [v4-effective-only.md](v4-effective-only.md) | Depicted by this selected still. |
| Registry loading | [v4-effective-only.md](v4-effective-only.md) | Depicted by this selected still. |
| Stream partial | [v4-effective-only.md](v4-effective-only.md) | Depicted by this selected still. |
| Multi-root stale | [v4-effective-only.md](v4-effective-only.md) | Depicted by this selected still. |
| Remote Brain state matrix | [v4-effective-only.md](v4-effective-only.md) | Depicted by this selected still. |
| A filter, review, or CAS result | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-layer-cake.png](v1-layer-cake.png) | [v1-layer-cake.md](v1-layer-cake.md) | `superseded` | Earlier Settings lookbook iteration; replaced by canonical `v4-effective-only`. |
| [v2-effective-values.png](v2-effective-values.png) | [v2-effective-values.md](v2-effective-values.md) | `superseded` | Earlier Settings lookbook iteration; replaced by canonical `v4-effective-only`. |
| [v3-hud-pass-dark.png](v3-hud-pass-dark.png) | [v3-hud-pass-dark.md](v3-hud-pass-dark.md) | `superseded` | Earlier Settings lookbook iteration; replaced by canonical `v4-effective-only`. |
| [v3-hud-pass-light.png](v3-hud-pass-light.png) | [v3-hud-pass-light.md](v3-hud-pass-light.md) | `superseded` | Earlier Settings lookbook iteration; replaced by canonical `v4-effective-only`. |
| [v4-effective-only.png](v4-effective-only.png) | [v4-effective-only.md](v4-effective-only.md) | `current` | Pre-Task-1 canonical selection for Settings. |

## Historical decisions

The pre-Task-1 canonical table selects v4; previous studies are superseded.
