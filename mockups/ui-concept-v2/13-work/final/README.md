---
design_status: current
evidence_class: concept_synthetic
---

# Work final state set

This folder is the authoritative implementation reference for Work. Work lets
a user turn an objective into a dependency-aware Kanban board and task graph,
understand what is ready or blocked, admit work through real placement
authority, and compare the plan with the attempts and outcomes TraceDecay
actually observed.

All identities and values pictured here are concept/synthetic. Production must
bind every task, relation, status, placement, activity row, command, and outcome
to the evidence ladder in [`DESIGN-SYSTEM.md`](../../DESIGN-SYSTEM.md). A task
status is never evidence that execution occurred or that its outcome succeeded.

## State manifest

| State | Image | Product brief | Status |
|---|---|---|---|
| Selected task in dependency graph | [01-task-dag-board.png](01-task-dag-board.png) | [01-task-dag-board.md](01-task-dag-board.md) | approved |

## Shared interaction contract

- Kanban board, DAG, timeline, causal, workload, and topology are projections of one
  immutable Work graph revision. Switching projection preserves scope and task
  selection; an unavailable projection remains explicitly unavailable.
- Cards expose typed task lifecycle and dependency state. Solid and dashed
  edges represent named hard and soft relations, never decorative proximity.
- Selection opens the task inspector without mutating the graph. The inspector
  separates task definition, admission, placement, attempts, observed activity,
  and outcomes so no one status stands in for another.
- Accept, admit, replan, cancel, or retry appears actionable only when the
  daemon has prepared the corresponding typed command and the user has the
  required permission. Completion is shown only after the authoritative result
  receipt returns; denial, conflict, staleness, and refusal remain visible.
- Placement names the selected agent, host, session, worktree, or execution
  target only when the owning authority supplies it. Candidate, pending,
  refused, and unavailable placement are distinct states.
- Planned tasks and dependencies are compared with observed attempts, agent and
  session activity, code changes, tests, review evidence, and Delivery outcomes.
  Unmatched planned work and unplanned observed work remain visible rather than
  being silently reconciled.
- Task, attempt, agent, session, code, and Delivery links drill through to the
  corresponding workspace while preserving project scope and a return path.

## Browser and accessibility contract

- React/DOM owns the shell, projection switcher, controls, inspector, exact
  tables, permissions, focus order, and accessible names. The shared scene
  layer may render dense task topology from deterministic positions but never
  owns command semantics or authoritative text.
- Every graph and command interaction has a keyboard equivalent and visible
  focus state. The exact task/dependency/attempt table supports the same
  selection, sorting, evidence grades, and drill-through routes.
- At 200% browser zoom, controls and the selected-task inspector reflow or enter
  a dedicated focus mode; labels are never raster-scaled or clipped into an
  unusable canvas.
- Reduced motion removes animated edge travel, camera interpolation, bloom,
  and live-feed motion. Static glyph, line style, state text, and timestamps
  retain the complete meaning.
- Dense real graphs use deterministic clustering, semantic zoom, virtualized
  tables, search, filters, and path-to-root/path-to-outcome focus. No overview
  allocates a permanent DOM element to every task or attempt.

## Production authorities

- The Work graph owns immutable graph revision, task identity, criteria,
  lifecycle, dependencies, and relation replans.
- Work admission and placement authorities own prepared commands, permissions,
  conflicts, refusals, selected execution targets, and result receipts.
- Attempt, session, agent, repository, code, test, review, and Delivery
  projections own observed activity and outcomes. Work may correlate these
  records but must preserve `EXACT`, `EXPLICIT`, `INFERRED`, `AMBIGUOUS`,
  `STALE`, and `UNAVAILABLE` grades.
- `dashboard/src/workspaces/work/WorkPage.tsx`, `useWorkGraphViews`,
  `useWorkAttempts`, `useWorkTopology`,
  `dashboard/src/workspaces/work/views/WorkProjectionSwitcher.tsx`,
  `WorkBoard.tsx`, and `workViewsModel.ts` are the current composition and
  projection targets.
- [`IMPLEMENTATION.md`](../../IMPLEMENTATION.md) owns the hybrid DOM/scene
  architecture, deterministic layout, density strategy, and renderer spike.
