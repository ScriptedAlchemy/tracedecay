# Workflows concept plates

## Purpose

Workflow definitions and lifecycle evidence with immutable versions, pinned digests, and constrained run lookup.

Route: `/workflows`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/workflows/WorkflowsPage.tsx` and `workflowQueries.ts` use the canonical `/application/workflow` routes for definitions, lifecycle, and run projection; generated contracts decode every rendered value.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Definitions loading/ready/measured-empty/refused/unknown | [v3-definition-ledger.md](v3-definition-ledger.md) | Depicted by this selected still. |
| Selected definition detail | [v3-definition-ledger.md](v3-definition-ledger.md) | Depicted by this selected still. |
| Pinned policy/config/catalog digests | [v3-definition-ledger.md](v3-definition-ledger.md) | Depicted by this selected still. |
| Steps | [v3-definition-ledger.md](v3-definition-ledger.md) | Depicted by this selected still. |
| CAS conflict | [v3-definition-ledger.md](v3-definition-ledger.md) | Depicted by this selected still. |
| Schema refusal | [v3-definition-ledger.md](v3-definition-ledger.md) | Depicted by this selected still. |
| Validation failure | [v3-definition-ledger.md](v3-definition-ledger.md) | Depicted by this selected still. |
| Denied/locked scope | [v3-definition-ledger.md](v3-definition-ledger.md) | Depicted by this selected still. |
| Idle disabled run lookup | [v3-definition-ledger.md](v3-definition-ledger.md) | Depicted by this selected still. |
| Run lookup state chips | [v3-definition-ledger.md](v3-definition-ledger.md) | Depicted by this selected still. |
| A lifecycle write result | No current plate | Required interaction/result is not depicted. |
| A found run projection | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

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
