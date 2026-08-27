# Agents concept plates

## Purpose

Agent relationships through independent usage, tree, handoff, token, and failure authorities.

Route: `/agents`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/agents/AgentsPage.tsx` decodes `Analytics*PayloadV1Schema` from `/api/plugins/analytics`; `SubagentTree.tsx`, `AgentHandoffs.tsx`, `AgentHandoffTokens.tsx`, and `AgentFailureContext.tsx` own the named panels.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Usage ready | [v3-authority-tree.md](v3-authority-tree.md) | Depicted by this selected still. |
| Diagnostics stale | [v3-authority-tree.md](v3-authority-tree.md) | Depicted by this selected still. |
| Subagent tree ready | [v3-authority-tree.md](v3-authority-tree.md) | Depicted by this selected still. |
| Work graph ready | [v3-authority-tree.md](v3-authority-tree.md) | Depicted by this selected still. |
| Handoff tokens schema refusal | [v3-authority-tree.md](v3-authority-tree.md) | Depicted by this selected still. |
| Managed-agent counts | [v3-authority-tree.md](v3-authority-tree.md) | Depicted by this selected still. |
| Failure unavailable | [v3-authority-tree.md](v3-authority-tree.md) | Depicted by this selected still. |
| Failure failed | [v3-authority-tree.md](v3-authority-tree.md) | Depicted by this selected still. |
| Failure recovery | [v3-authority-tree.md](v3-authority-tree.md) | Depicted by this selected still. |
| Failure timeout | [v3-authority-tree.md](v3-authority-tree.md) | Depicted by this selected still. |
| A delegated handoff transition | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

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
