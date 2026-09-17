# 11c — Work workspace design: core delivery and advanced controls

Status: design contract for the Work workspace, allocated to the core Work delivery
with residual advanced workflow controls in the advanced workflow delivery by the approved 2026-07-28
sequencing decision. It was written 2026-07-25 after the dashboard grammar library landed.
Semantics are owned by
[Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md); this doc
owns how those semantics look, move, and connect.

Delivery status (2026-08-06 mount audit): the core Work read/command/SSE path
is delivery-current on the integration tip. All nine `operation.work.*`
operations (`create`, `snapshot`, `delta`, `accept_task`, `accept_proposal`,
`review_proposal`, `admit_execution`, `attach_runtime_evidence`,
`replan_dependencies`) are mounted daemon → dashboard-api HTTP → SDK
registries → dashboard Work workspace, with the stage-grouped board, TaskId
deep links, `task_activity` SSE, snapshot/delta reads, and seven command
surfaces (verified: work_authority 8/8, dashboard-api routes 6/6, SDK facade
8/8, dashboard 1149/1149). The DAG/critical-path, timeline/attempt-weave,
causal, and workload-cortex projections are not yet built. They remain
committed in-scope V2 deliverables of this plan — not descoped — whose data
dependency is attempt/execution evidence owned by the
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md) workflow runtime: the
attempt family was deliberately deleted in the Work restore and returns with
Plan 32 execution evidence. Normative like 11a/11b: every
channel encodes a stated measurement, absence is drawn, degenerate
distributions are said rather than drawn, caps are captioned.

**Dated amendment (2026-08-07, recorded decision -- supersedes "not yet
built"/"nine operations" above).** The DAG/critical-path, timeline/attempt-weave,
causal, and workload-cortex projections are landed: the Work operation family
grew from nine to **16 mounted `operation.work.*` operations**
(`crates/tracedecay-sdk/src/operations.rs`): create, snapshot, delta, views,
accept_task, generate_proposal, review_proposal, accept_proposal,
admit_execution, attach_runtime_evidence, replan_dependencies, start_attempt,
cancel_attempt, resume_attempts, attempt_status, list_attempts. The
work-product graph gained a durable production authority
(`c920bc8b8b`, `448cf7ecff`;
`crates/tracedecay-rusqlite-runtime/src/work_product/{publication,read}.rs`),
the `views` route was mounted on it (`c18a3af6b2`, `8255f399ac`;
`crates/tracedecay-api/src/work.rs`), and the dashboard bound the four Work
views to it (`709e9a5b6a`;
`dashboard/src/workspaces/work/views/{WorkDagView,WorkTimelineView,WorkCausalView,WorkWorkloadView}.tsx`).
The views-operation truthfulness constraint from the decision trail below is
honored, not violated: genuinely unmeasured channels (`wall_clock`,
`observed_order`) still render typed absence rather than fabricated readings
(`dashboard/src/workspaces/work/workChannel.ts`, `workViewsModel.ts`).
Residual open scope, unaffected by this amendment: the advanced-delivery
slice (execution-topology lanes, placement UI, automation execution
controls, expertise/calibration views, fan-out/synthesis/recovery UI,
host/LSP handoff) remains MISSING per the 2026-07-28 core/advanced split,
and the task-session weave join (advanced) still depends on the filed
correlation fix.

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
plumbing and does not require GitHub. The core delivery ships Kanban, DAG, timeline, causal,
workload, basic topology read, TaskId selection, `task_activity` SSE, and deep
links over the canonical graph. The advanced delivery adds advanced topology, placement,
automation execution, expertise/calibration, fan-out/synthesis/recovery, and
host/LSP handoff controls over those same authorities.

Beauty and function are simultaneous acceptance criteria here too. Generic,
clinical, simple, or non-magnificent workflow UI; a bland vertical list that
consumes the workspace; and falsified task, activity, cost, or readiness state
are rejected.

Specific hue, type, spacing, dark/light, motion, and easing choices inherited
from Plans 11a/11b are design-owner/agent decisions, not user preferences.

## Inheritance — one grammar, new nouns

The core Work slice reuses the five visual grammars already proved on real data;
the advanced controls do not invent a sixth.
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
  instant-collapsed spans; the advanced delivery hard-depends on it).
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

1. Plan 24 task/plan stores + executor semantics (owner: Plan 24) — delivered
   for the core read/command path.
2. commit↔session correlation + span refresh fix — blocking for the weave
   projection (chip filed 2026-07-25).
3. `task_activity` SSE family — delivered.
4. Plan 32 attempt/execution evidence — blocking for the DAG/critical-path,
   timeline, causal, and workload projections; those views ship in full once
   the workflow runtime supplies attempt data.
5. Build order for the remaining views: Kanban is shipped; DAG first (pure
   projection over the store), weave/causal second (need correlation fix),
   cortex workload last (needs volume to be worth aggregating).
   The advanced delivery then layers advanced workflow controls without replacing projections
   or canonical selection.
