# 11c — Work workspace design: PR14 core and PR17 advanced controls

Status: design contract for the Work workspace, allocated to PR14 core delivery
with residual advanced workflow controls in PR17 by the approved 2026-07-28
sequencing decision. It was written 2026-07-25 after the PR14 grammar library landed.
Semantics are owned by
[Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md); this doc
owns how those semantics look, move, and connect. Normative like 11a/11b: every
channel encodes a stated measurement, absence is drawn, degenerate
distributions are said rather than drawn, caps are captioned.

Plan 24's contract rule applies here: pure source-only/internal Work view
helpers, branch-local V2 DTO revisions, and routes change in place.
Persisted V2 state accepts only its exact final shape; any other database,
store, spool, file, or projection returns typed `ResetRequired` and requires
explicit reset or recreation. No storage reader, migration, backfill, dual
write, or census path exists. Product graph/item versions remain UI-visible
history/CAS identities rather than by themselves evidence for another wire
version.

The user explicitly requires a first-class TraceDecay task graph/Kanban
inspired by Hermes but more powerful; it is a product feature, not roadmap
plumbing and does not require GitHub. PR14 ships Kanban, DAG, timeline, causal,
workload, basic topology read, TaskId selection, `task_activity` SSE, and deep
links over the canonical graph. PR17 adds advanced topology, placement,
automation execution, expertise/calibration, fan-out/synthesis/recovery, and
host/LSP handoff controls over those same authorities.

Beauty and function are simultaneous acceptance criteria here too. Generic,
clinical, simple, or non-magnificent workflow UI; a bland vertical list that
consumes the workspace; and falsified task, activity, cost, or readiness state
are rejected.

Specific hue, type, spacing, dark/light, motion, and easing choices inherited
from Plans 11a/11b are design-owner/agent decisions, not user preferences.

## Inheritance — one grammar, new nouns

PR14's Work slice reuses the five visual grammars already proved on real data;
PR17's advanced controls do not invent a sixth.
it maps each Plan 24 projection onto the grammar that already carries that
shape of meaning:

| Projection | Grammar it inherits | The mapping |
| --- | --- | --- |
| Kanban | Brain's measured columns (categorical x, spread inside, never across) | columns = REAL task states from Plan 24's state machine, never cosmetic lanes; card = body with area = measured cost (tokens/attempts/wall-time — pick one, caption it), brightness = recency of last attempt; WIP pressure printed per column, not implied |
| DAG / critical path | Transit map strata | strata = longest-path depth over the task DAG (same Tarjan condensation discipline as code); the critical path is the widest channel; a dependency that jumps backward wears the climb hue and the caption states it is an observation, not an error |
| Timeline / attempts | Loom weave | agents/executors as warp threads (hue = executor identity, stable app-wide), tasks as landings, retries as repeated crossings of the same landing; stale/instant spans drawn hollow exactly as the PR-421 weave does |
| Causal | Disagreement field | declared dependencies (plan edges) overlaid with OBSERVED execution order (attempt timestamps); "executed-before-but-undeclared" is the loud state — it is hidden coupling in the plan itself |
| Workload / executor / model | Cortex aggregation | executors/models as regions, area = task mass, contours = concurrency, heat = recent churn; aggregation ratio printed (N tasks ⟵ M regions) |

## Navigation — zoom is a position, selection is one

Two rules, both already proven:

1. **One canonical selection, many synchronized projections** (plan 11
   mandate). Selecting a task in ANY projection selects it in all; the
   projection switcher moves the camera, never the selection. No projection
   owns state.
2. **Zoom is a position, not a transition** (the LENS model from 11b).
   Altitude runs portfolio → epic/plan → task → attempt on one continuum;
   crossing an aggregation boundary is a readable position on a printed ruler,
   not a screen replacement. Drill-ins are in-page and Escape returns — the
   TraceView pattern (focus trap, aria equivalence via the adjacent list,
   reduced-motion static twin).

Command-K jumps by task id/title from anywhere; the Work workspace registers
its nouns in the same palette as symbols and projects.

## Connection into the bigger picture

A task is the hub noun that finally joins the app's three graphs, and every
edge below already has (or has a named dependency for) a wire source:

- task → sessions that executed it: the weave (needs the commit/session
  correlation + span-refresh fix — filed, currently 0/12 commit links and
  instant-collapsed spans; PR17 hard-depends on it).
- task → commits/PRs it produced: Delivery landings; the PR-421 weave sheet is
  the prototype of this join.
- task → code it touched: file-granular edited_files today (stated as
  file-granular per 11b); the topography Trace drill-in from a task's touched
  area is the long-range payoff.
- live motion: task state changes strike over SSE. Requires a `task_activity`
  family on the activity bus — same coalescing contract as the four shipped
  families, and the same project-granularity caveat until ActivityPulseV1
  carries finer scope. Design against project-level strikes first.

Same kindColor arc discipline: executor hues, state hues, and kind hues are
three separate stable assignments; a hue never means two things on one screen.

## Sensory contract application

Physics only where it encodes measurement: attempt-thread tension = retry
count; card weight = measured cost (a heavy task drags slower in kanban —
same mass-response curve as Trace, same tuning table). Drag-to-reprioritize is
INTERACTION, not measurement — it gets crisp motion, no fake physics, and a
reduced-motion static path that produces identical final order.

## Honesty beats specific to workflow data

- Attempt counts, queue ages, and wall-times are real or absent — never
  estimated. An unscheduled task is drawn in an explicit unscheduled band, not
  omitted.
- Kanban columns that are empty stay visible at full width (an empty REVIEW
  column is a reading).
- Every projection states its population cap and window ("50 most recent of
  312 tasks · last 14d") — the Agents-page lesson.
- Delegation trees show unattributed work as hollow (the executor the store
  cannot name is not guessed).

## Reference images

The grammars this doc inherits are visual; read them as pictures, not prose.
Historical reference sheets per grammar are indexed in
[11b — Visual artifact index](11b-structure-visualization.md#visual-artifact-index-reference-images):
measured columns → the shipped Brain baselines (`dashboard/audit-baselines/`),
strata → the transit sheet, weave → the TRACE prototype + the PR-421 weave,
disagreement → the disagreement-field sheet, cortex aggregation → the CORTEX
sheet, zoom-as-position → the LENS sheet. These branch-tied artifacts are
historical design evidence, not acceptance authority or records to recreate.
An implementer may consult them but must verify the current product in real
Google Chrome.

## Visual verification

Review every Work projection in real Google Chrome at full viewport, capture
screenshots, and click through every interaction state, including drag,
selection synchronization, projection switches, live activity, cancellation,
and unavailable paths. The embedded browser is rejected because its viewport
is too small. Automated browser tests support this pass but do not replace it,
and the pass produces no per-commit evaluation document or git-hash-tied
evidence manifest.

## Dependencies and sequencing

1. Plan 24 task/plan stores + executor semantics (owner: Plan 24) — blocking.
2. commit↔session correlation + span refresh fix — blocking for the weave
   projection (chip filed 2026-07-25).
3. `task_activity` SSE family — thin, after Plan 24 lands events.
4. Build order inside PR14: Kanban + DAG first (pure projections over the
   store), weave/causal second (need correlation fix), cortex workload last.
   PR17 then layers advanced workflow controls without replacing projections
   or canonical selection.
   (needs volume to be worth aggregating).
