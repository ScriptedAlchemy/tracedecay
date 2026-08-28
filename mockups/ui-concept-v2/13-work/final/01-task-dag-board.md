---
design_status: current
evidence_class: concept_synthetic
---

# Selected task in dependency graph

## User job

Understand what work is ready, blocked, admitted, or pending; see why tasks
depend on one another; select the next meaningful task; and admit or replan it
without confusing planned status with observed execution or delivery success.

## Product behavior

- The Kanban board and central DAG are projections of one immutable Work graph
  revision. Each card names the exact task identity, component, priority,
  estimate, and typed lifecycle state supplied by Work authority.
- Solid and dashed paths are named hard and soft dependencies. Critical-path
  emphasis is derived from the selected graph revision and remains available
  as exact text, not only color or glow.
- Selecting a task preserves the graph as context and opens a roomy inspector
  for task definition, acceptance criteria, admission, placement, attempts,
  observed activity, and linked outcomes.
- Production commands such as Accept task, Admit execution, and Apply relation
  replan are enabled only after the daemon returns a prepared typed command and
  permission result. The UI shows pending, accepted, refused, conflicted, stale,
  or unavailable results; it never optimistically fabricates success.
- The activity ledger records observed task events independently of card state.
  From a row or inspector link, the user can follow the exact attempt into
  Sessions, Agents, Code, tests, review evidence, or Delivery.
- Planned-versus-observed comparison calls out a task with no attempt, an
  attempt with no matching plan, work performed outside the selected scope, and
  an outcome whose evidence is missing or contradictory.

## Interaction and evidence

Hover/focus previews a task and its dependency neighborhood. Click/Enter
selects it; arrow keys traverse the visible DAG; path-to-root and
path-to-outcome commands isolate causally relevant work. Projection switching
retains task selection only when the same stable identity exists in the target
projection.

Every card, edge, event, placement, and outcome uses the canonical evidence
ladder: `EXACT`, `EXPLICIT`, `INFERRED`, `AMBIGUOUS`, `STALE`, or
`UNAVAILABLE`. An inferred relation remains dashed and named; no numeric
confidence substitutes for source provenance.

## Acceptance gates

- Keyboard selection and all task commands match pointer behavior and expose a
  visible focus state.
- Reduced motion preserves dependency, status, selection, and activity meaning
  without edge animation or camera travel.
- At 200% browser zoom, the graph can enter focus mode while the inspector and
  controls reflow; task labels and command results remain readable.
- Dense graphs cluster deterministically and support search, filters, semantic
  zoom, virtualized exact tables, and focused root/outcome paths.
- The exact task/dependency/attempt/outcome table is a complete accessible
  fallback and exposes source identity, timestamps, evidence grade, and the
  same drill-through destinations.

## Truth boundary

The plate is `CONCEPT / SYNTHETIC DATA`. Its sample task names, statuses,
counts, timestamps, estimates, owners, and command readiness are not runtime
receipts. Production may show a control as ready only when its named authority
provides the command and permission state.

## Production authorities

The Work graph owns tasks and relations; admission and placement own prepared
commands and execution targets; attempt/session/agent/code/test/review/Delivery
projections own observed work and outcomes. The concrete composition targets
and browser architecture are listed in [`README.md`](README.md) and
[`IMPLEMENTATION.md`](../../IMPLEMENTATION.md).
