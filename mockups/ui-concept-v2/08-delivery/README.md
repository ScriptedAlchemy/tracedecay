# Delivery concept plates

## Purpose

Local Git and provider delivery as independent authorities rather than a healthy pipeline.

Route: `/delivery`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/delivery/DeliveryPage.tsx`, `DeliveryField.tsx`, `field.ts`, and `time.ts` are the delivery-field authority; its DOM test fixes independent local/provider behavior.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Local Git ready | [v3-independent-authorities.md](v3-independent-authorities.md) | Depicted by this selected still. |
| Pull requests unavailable | [v3-independent-authorities.md](v3-independent-authorities.md) | Depicted by this selected still. |
| Reviews unavailable | [v3-independent-authorities.md](v3-independent-authorities.md) | Depicted by this selected still. |
| CI rate limited | [v3-independent-authorities.md](v3-independent-authorities.md) | Depicted by this selected still. |
| Failure localization stale | [v3-independent-authorities.md](v3-independent-authorities.md) | Depicted by this selected still. |
| Releases not published | [v3-independent-authorities.md](v3-independent-authorities.md) | Depicted by this selected still. |
| Index freshness ready | [v3-independent-authorities.md](v3-independent-authorities.md) | Depicted by this selected still. |
| Projection-state matrix | [v3-independent-authorities.md](v3-independent-authorities.md) | Depicted by this selected still. |
| A provider refetch result | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Asset ledger

Every existing PNG is indexed exactly once. Lifecycle is explicit and never inferred from filename version order.

| PNG | Explainer | Lifecycle | Decision |
|---|---|---|---|
| [v1-recency-field.png](v1-recency-field.png) | [v1-recency-field.md](v1-recency-field.md) | `superseded` | Earlier Delivery lookbook iteration; replaced by canonical `v3-independent-authorities`. |
| [v2-hud-pass-dark.png](v2-hud-pass-dark.png) | [v2-hud-pass-dark.md](v2-hud-pass-dark.md) | `superseded` | Earlier Delivery lookbook iteration; replaced by canonical `v3-independent-authorities`. |
| [v2-hud-pass-light.png](v2-hud-pass-light.png) | [v2-hud-pass-light.md](v2-hud-pass-light.md) | `superseded` | Earlier Delivery lookbook iteration; replaced by canonical `v3-independent-authorities`. |
| [v3-independent-authorities.png](v3-independent-authorities.png) | [v3-independent-authorities.md](v3-independent-authorities.md) | `current` | Pre-Task-1 canonical selection for Delivery. |

## Historical decisions

The pre-Task-1 canonical table selects v3; previous studies are superseded.
