# Work concept plates

## Purpose

Work is the dependency-aware planning and admission surface: users inspect one
immutable task graph revision through a Kanban board or causal camera,
understand readiness and placement, admit work through typed production
commands, and compare the plan with observed attempts and outcomes.

Route: `/work`.

## Authoritative final set

[`final/README.md`](final/README.md) is the implementation authority for the
reviewed Work state set. Its selected-task DAG plate and same-stem brief define
the current product, evidence, interaction, scale, and accessibility contract.

Historical plates below are retained for design provenance only. None is a
current implementation reference.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and evidence language;
  [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage;
  [IMPLEMENTATION.md](../IMPLEMENTATION.md) owns the browser/scene boundary.
- `dashboard/src/workspaces/work/WorkPage.tsx` uses `useWorkGraphViews`,
  `useWorkAttempts`, and `useWorkTopology`; `WorkProjectionSwitcher.tsx`,
  `WorkBoard.tsx`, and `workViewsModel.ts` bind projections to one graph read.
- The final plate remains concept/synthetic. These source paths identify
  production authorities, not proof that the pictured data or controls are live.

## Canonical semantic-state matrix

| Semantic state or interaction | Authoritative brief | Entry condition |
|---|---|---|
| Board/DAG projection | [final/01-task-dag-board.md](final/01-task-dag-board.md) | Open Work after an immutable graph revision is served. |
| Selected task and dependency neighborhood | [final/01-task-dag-board.md](final/01-task-dag-board.md) | Select a stable task identity from graph or exact table. |
| Ready, blocked, admitted, pending, stale, or unavailable task | [final/01-task-dag-board.md](final/01-task-dag-board.md) | Render the typed state from Work authority. |
| Admission and relation replan | [final/01-task-dag-board.md](final/01-task-dag-board.md) | Daemon serves a prepared command and permission result. |
| Placement | [final/01-task-dag-board.md](final/01-task-dag-board.md) | Placement authority serves candidate, selected, refused, or unavailable state. |
| Planned versus observed work | [final/01-task-dag-board.md](final/01-task-dag-board.md) | Attempt/session/agent/code evidence correlates to a task or remains unmatched. |
| Outcome drill-through | [final/01-task-dag-board.md](final/01-task-dag-board.md) | An exact or graded link exists to tests, review, or Delivery. |
| Projection unavailable | [final/README.md](final/README.md) | A camera authority does not serve the selected revision. |

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
