# Agents concept plates

## Purpose

Scalable parent/subagent delegation topology with independently sourced handoffs, tokens, work products, statuses, failures, and cross-page evidence.

Route: `/agents`.

## Authoritative final set

The reviewed implementation reference is [final/README.md](final/README.md). Historical lookbook plates remain in this folder for provenance but are superseded.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and evidence-grade language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- `dashboard/src/workspaces/agents/AgentsPage.tsx` reads independently typed analytics projections; the subagent tree, handoff, handoff-token, and failure components own their respective evidence.
- Sessions, Work, Code/Git, and Delivery remain the authorities for conversations, tasks/products, code changes, and PR outcomes.

## Canonical semantic-state matrix

| Semantic state or interaction | Current explainer | Coverage |
|---|---|---|
| Parent/subagent delegation topology | [final/01-delegation-topology.md](final/01-delegation-topology.md) | Depicted by generation. |
| Selected handoff and work product | [final/01-delegation-topology.md](final/01-delegation-topology.md) | Depicted. |
| Activity/category/tool window and capped coverage | [final/01-delegation-topology.md](final/01-delegation-topology.md) | Depicted. |
| Failure, partial, stale, refused, and unavailable authority states | [final/01-delegation-topology.md](final/01-delegation-topology.md) | Product contract; selected partial failure is depicted. |
| Sessions, Loom, Work, Code, and Delivery pivots | [final/01-delegation-topology.md](final/01-delegation-topology.md) | Required drill-through contract. |
| Hundred-plus-agent bundling, semantic zoom, and branch navigator | [final/01-delegation-topology.md](final/01-delegation-topology.md) | Required scale contract; overview sample is smaller. |
| Exact / explicit / inferred / ambiguous / stale / unavailable | [final/01-delegation-topology.md](final/01-delegation-topology.md) | Required product contract. |
| Keyboard, reduced motion, 200% zoom, dense data, exact fallback | [final/01-delegation-topology.md](final/01-delegation-topology.md) | Required acceptance gates. |

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
