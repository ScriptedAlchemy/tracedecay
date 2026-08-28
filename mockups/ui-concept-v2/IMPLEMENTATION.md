# TraceDecay V2 browser implementation architecture

Status: implementation reference; renderer choice remains spike-gated

Concept authority: `DESIGN-SYSTEM.md`, `NAVIGATION.md`, `INTERACTION-STATES.md`,
and each screen's `final/` manifest and product brief

Application baseline: the React 19/Rsbuild dashboard on the V2 rewrite line

## Purpose

This document explains how to build the final V2 concepts as a real embedded
browser product. It does not authorize loading the concept PNGs into the
shipping application, reproducing their sample values, or presenting a fixture
as production evidence.

The implementation is deliberately hybrid:

- React and the DOM own navigation, scope, filters, inspectors, transcripts,
  diffs, review threads, comments, check matrices, forms, tables, keyboard
  controls, focus, URLs, and accessible exact-evidence fallbacks.
- A pure deterministic layout maps source records into stable spatial
  coordinates. Time is X. Parent, workstream, agent, and subagent hierarchy is
  Y. Layout does not depend on the renderer.
- One high-fidelity scene runtime renders the signature dense fields after a
  measured proof-of-capability spike. React Three Fiber from its React
  19-compatible v9 family with Three.js is the leading candidate; PixiJS v8 is
  the alternative for a purely 2D implementation.
- Canvas or SVG overlays render crisp temporal rails, cursors, event glyphs,
  selections, annotations, density summaries, and motion-independent state.
- A worker performs lane allocation, clustering, edge bundling, density
  aggregation, and semantic-zoom projection for large histories.
- The Rust backend owns one truthful journey projection joining the existing
  Delivery, Loom, Work, Agents, Sessions, Code, Git, pull-request, CI, review,
  and release evidence. It never invents private reasoning or silently upgrades
  a correlation into a fact.

React, Rsbuild, Graphology, generated contracts, and the existing query/state
machinery remain application infrastructure. Sigma is a replaceable legacy
renderer, not a constraint on the product language.

## Product and truth boundaries

### The concept images are not runtime assets

The final PNGs describe hierarchy, interaction, density, and visual language.
The shipping dashboard must not use them as backgrounds, rasterized panels,
precomputed graphs, or evidence. Every value and relation must arrive through a
real production authority or render as an honest typed absence.

The browser must preserve the evidence-grade ladder in `DESIGN-SYSTEM.md`:
`EXACT`, `EXPLICIT`, `INFERRED`, `AMBIGUOUS`, `STALE`, and `UNAVAILABLE`.
Source class is separate: user transcript, assistant summary, subagent report,
PR body, commit message, repository diff, check result, review finding, or
provider-unavailable reasoning. Decimal confidence theatre is prohibited.

Private chain-of-thought is never a source class. Persisted messages,
provider-visible summaries, explicit decisions, PR bodies, and commit messages
may be displayed with their actual provenance. Missing or private reasoning is
an explicit gap.

### Local and provider actions

Local TraceDecay feedback may attach a comment, challenge, risk mark, or
clarification request to a visible code hunk, task, decision, episode, or
persisted reasoning artifact only when the real local write path is composed.
GitHub/provider actions remain read-only unless a separate production slice
delivers and verifies the corresponding authenticated mutation end to end.

No control may simulate posting, resolving, merging, rerunning, or changing a
provider object. An unavailable path stays mounted only if it is visibly
disabled with the daemon-owned reason; otherwise omit it.

## Baseline versus target gap

The current V2 browser baseline is useful application infrastructure but is not
the final visual runtime:

| Surface | Current baseline | Final target | Why CSS alone is insufficient |
|---|---|---|---|
| Brain | Sigma circles and thin straight graph edges | luminous neural bodies, depth, curved causal paths, stable picking, semantic zoom, and measured activity | the target needs custom geometry/materials, controlled depth, bloom, and stateful path rendering |
| Loom | a rectangular host/session lane chart with loaded-page replay | a horizontal temporal execution field with spawn branches, handoffs, rejoin paths, density bundles, replay, and focus-plus-context | the target needs deterministic hierarchical lanes, edge bundling, semantic zoom, and thousands of selectable events |
| Delivery | a repository recency field and vertical Git/provider projection ledger | global/project PR discovery, umbrella outcomes, horizontal causal journeys, exact review workspaces, and why-to-code navigation | the target joins multiple authorities and alternates between dense scene, exact diff, transcript, and review modes |

The V2 work may retain Sigma for a bounded legacy graph while the spike runs.
It must not water down the accepted concepts to preserve Sigma as the rendering
authority. If the spike confirms Sigma cannot meet the target, replace it in
the shared scene layer instead of adding page-specific workarounds.

## Layer model

### 1. React and DOM authority

React 19 and Rsbuild remain the application shell and composition runtime. DOM
regions own:

- the canonical fourteen-channel rail, project scope, route state, filters,
  search, branch navigation, and selected-object breadcrumbs;
- start/continue review, previous/next episode, coverage progress, playback
  controls, follow-loaded-tail versus paused inspection, and visible source
  qualification;
- transcripts, reasoning summaries, exact code/diffs, comments, review threads,
  check matrices, task details, tables, forms, and all feedback controls;
- resizable/collapsible Story, Code and Impact, Evidence, and Feedback panes;
- focus management, roving focus where appropriate, keyboard shortcuts,
  announcements, reduced-motion preference, 200% zoom/reflow, and native text
  selection; and
- exact list, tree, table, transcript, and diff fallbacks synchronized with
  any scene selection.

Canvas text may supplement orientation at wide viewports. It may not become the
only readable source of names, code, statuses, or evidence.

### 2. Deterministic temporal layout

The layout package is a pure, renderer-independent transform:

```text
JourneyProjection + viewport + semanticZoom + expandedBranches
  -> TemporalSceneModel
       nodes[]       stable id, x, y, extent, kind, grade, source ref
       paths[]       source id, target id, relation, grade, curve controls
       clusters[]    members, counts, bounds, risk/evidence summaries
       rails[]       project/PR/workstream/time axis
       labels[]      priority, anchor, collision group
       minimap[]     stable global extents and density bins
```

Invariants:

- X is derived from recorded time, an explicit sequence index for undated
  evidence, or an honest undated gutter. Rendering never invents timestamps.
- Y is allocated deterministically from umbrella Delivery, repository/PR,
  workstream/task, parent session, agent, and subagent identity. Stable inputs
  produce stable coordinates across reloads and renderers.
- A spawn allocates a child branch lane. A handoff or returned result draws a
  named directed relation. Rejoin requires a real result, handoff, commit,
  changed artifact, or task edge; visual proximity is not evidence.
- Cross-repository PRs occupy separate rails under one umbrella outcome and
  retain individual PR entry points.
- Branch bundles are grouped by explicit task/workstream/repository/outcome
  first. Heuristic clustering is labeled inferred and exposes its basis.
- Long histories compress empty time and dense spans with visible breaks or
  density summaries. The minimap retains the full temporal extent.
- Layout does not use force simulation for a journey. Graphology may store and
  traverse topology, but coordinates come from the temporal/hierarchical
  layout contract.

### 3. High-fidelity scene runtime

The leading candidate is React Three Fiber from the React 19-compatible v9
family with Three.js. Exact package versions and peer compatibility must be
verified during the spike rather than pinned speculatively in this concept
brief.

The target scene uses:

- an orthographic 2.5D camera so time and lane position remain measurable;
- instanced nodes, event glyphs, density bodies, and label anchors;
- custom shader materials for source/evidence/state encoding and restrained
  luminous falloff;
- selective postprocessing/bloom that never carries meaning alone;
- curved line, ribbon, or tube geometry for branches, handoffs, causal links,
  and bundled paths;
- GPU picking mapped back to stable source identities;
- controlled pointer pan/zoom and programmatic focus without moving DOM chrome;
- playback animation driven from the URL-addressable cursor, not a free-running
  visual clock; and
- adaptive quality and a reduced-motion path that retains all information in a
  static composition.

PixiJS v8 is the alternative if the spike shows that a purely 2D custom
WebGL/WebGPU mesh, filter, and blend-mode implementation gives materially
better density, accessibility integration, memory use, or development
predictability. The production application must choose one of these scene
runtimes. Do not ship both without measured evidence that one cannot cover a
named surface.

### 4. Canvas and SVG overlays

A screen-space Canvas or SVG overlay above the scene owns crisp, zoom-stable:

- time ticks, date breaks, the playback cursor, current-window bounds, and
  overview/minimap viewport;
- event glyph outlines, selection rings, focus paths, annotations, comment
  anchors, missing-evidence gaps, and source/evidence legends;
- collapsed-branch brackets, count labels, density summaries, and reduced-
  motion static direction cues; and
- hit-region handoff to the DOM for controls and exact evidence.

SVG remains appropriate for small bounded diagrams and deterministic export.
It is not the primary 20,000-event renderer. DOM owns every interactive
control even when an overlay draws its visual affordance.

### 5. Layout and density worker

A dedicated worker consumes serializable projection data and produces the
`TemporalSceneModel`. It owns:

- deterministic lane allocation and ordering;
- workstream/repository/task clustering;
- branch and causal-edge bundling;
- density aggregation and representative frontier selection;
- collision-aware label priority;
- semantic-zoom projections; and
- viewport culling metadata.

It does not fetch data, infer truth, assign evidence grades, or decide review
status. Those are backend/application concerns. Layout jobs use revision IDs;
late results for an obsolete projection or viewport are discarded.

### 6. Honest journey projection

The existing `DeliveryOverviewV1`, `LoomTemporalPayloadV1`, LCM, Work, agent,
graph, Git, PR, review, CI, and release models are source authorities, not a
complete journey model. The backend needs one generated Rust/TypeScript journey
projection that joins them without creating a second store of truth.

The projection must retain stable identities for:

- project, repository, worktree, branch, PR, commit, release, check, review,
  file, hunk, symbol, test, task, workflow step, session, agent, subagent,
  message, persisted summary/decision, event, and feedback record;
- source record and source timestamp;
- relation grade and relation basis;
- projection generation, source freshness, pagination/truncation, and retained
  continuation handles; and
- unavailable/private/not-ingested source reasons.

The projection is append/join oriented. It does not rewrite immutable Git,
provider, transcript, or task facts. A local adjudication or feedback record is
a new linked artifact. Exact, explicit, inferred, ambiguous, stale, missing,
and unavailable relationships remain distinct in the wire contract and UI.

The initial API may extend the project gateway with a Delivery journey read and
bounded continuations, but the route and DTO names are implementation
decisions. They must be added to
`crates/tracedecay-dashboard-api/src/contract_schema.rs`, generated into
`dashboard/src/contracts/generated.ts`, and consumed through the existing
envelope/query ladder. The browser must not define a parallel handwritten DTO.

## Semantic zoom, replay, and addressability

Semantic zoom is an information contract, not just camera scale:

1. **Outcome:** umbrella Deliveries, repositories, major workstreams, risk and
   coverage summaries.
2. **Workstream:** task clusters, agent groups, branch bundles, major episodes,
   and review frontiers.
3. **Agent:** parent/child sessions, handoffs, commits, file groups, and checks.
4. **Episode:** persisted decisions, tasks, commands, edits, test runs,
   feedback, and outcomes.
5. **Event:** exact messages, tool calls, hooks, hunks, symbols, assertions,
   review comments, and check annotations.

The browser never keeps one visible lane, label, or DOM row per agent at
overview scale. For 100+ agents it renders bundles with reconciled counts,
risk/evidence summaries, and representative frontier nodes. A virtualized
branch navigator provides search, filters, keyboard traversal, pins,
unresolved/high-risk-only views, and path-to-root/path-to-outcome focus.
Focus-plus-context expands the selected causal neighborhood while unrelated
branches compress; the minimap keeps global orientation.

Playback exposes only records at or before the selected timestamp. Future
events are unrevealed rather than dimmed as if already known. URL state must be
able to address project, umbrella Delivery, PR, selected episode/event, semantic
zoom, expanded branch set or focused path, playback timestamp/mode, filters,
and workspace mode. Loaded-page replay remains visibly bounded by feed and
pagination state.

## Current V2 extension points

These are the known PR #707 application seams to extend. They describe the
reviewed V2 branch surface; the implementation must re-check exact HEAD and
signatures before editing.

| Concern | Existing extension points | Direction |
|---|---|---|
| Shell, scope, routes | `dashboard/src/app/shell/{Shell,NavRail,ScopeBar,StatusStrip}.tsx`, `dashboard/src/app/{channels,routes}.tsx`, URL scope synchronization | keep one DOM shell; add URL-addressable scene/review state without a second router or nav |
| Generated state | `crates/tracedecay-dashboard-api/src/contract_schema.rs`, `dashboard/src/contracts/generated.ts`, `useEnvelope.ts`, `usePayload.ts`, `envelope.ts` | add the journey projection and exact typed gaps through the generated contract/envelope authority |
| Graph data | Graphology plus `dashboard/src/viz/graph/{GraphCanvas,scene,renderer,layout,activationOverlay}.ts(x)` | retain Graphology for topology and current Sigma as legacy; place a new shared scene adapter beside the pure model, not inside page components |
| Loom | `dashboard/src/workspaces/loom/{LoomPage,WeaveCanvas,ThreadChain,ThreadPlayback,weave,tracks,playback}.ts(x)` | preserve current time-window/replay semantics and exact table; replace the primary rectangular weave renderer with deterministic temporal layout + shared scene |
| Loom reads | `/api/loom/temporal`, `/api/plugins/hermes-lcm/timeline`, selected `/api/plugins/hermes-lcm/session/{id}`; `loom_api.rs` and `lcm_api.rs` | extend with hooks, tools, files, tasks, agents, handoffs, commits, feedback, and explicit correlation grades through the journey projection |
| Delivery | `dashboard/src/workspaces/delivery/{DeliveryPage,DeliveryField,field,time}.ts(x)` | retain repository/pre-provider overview as a local-first state; add discovery, umbrella, journey, review, and why-to-code workspaces over shared scene + DOM evidence |
| Delivery reads | `/api/projects`, project-scoped `/api/delivery/overview`; `DeliveryOverviewV1`; `delivery_api.rs`; daemon `ProjectDeliveryReadPortV1` composition | preserve the existing eight independent Git/provider projections; join them into the journey only through named identities and grades |
| Exact evidence | existing LCM session reads, Delivery commit/PR/review/CI/release projections, Code graph/evidence routes, Git source, and native DOM tables | keep exact transcript/diff/thread/check/table views first-class and lazy-load large evidence behind stable selected IDs |
| Work and agents | `dashboard/src/workspaces/work/WorkPage.tsx`, `dashboard/src/workspaces/agents/AgentsPage.tsx`, Work graph/views/evidence routes, analytics subagent tree and handoff reads | compare planned task graph with observed work; source branch hierarchy and handoffs without proximity inference |
| Code and sessions | `dashboard/src/workspaces/code/CodePage.tsx`, `dashboard/src/workspaces/sessions/SessionsPage.tsx` and their graph/transcript readers | deep-link selected symbols, hunks, sessions, and persisted messages both into and out of Loom/Delivery |

The current `GraphCanvas` and `WeaveCanvas` remain valid reference and fallback
implementations during the spike. They do not become the new renderer through
incremental CSS effects.

## Brain final-state implementation map

All files below are under `01-brain/final/`. Brain shares the high-fidelity
scene runtime with Loom and Delivery, but its measured registry coordinates and
evidenced code topology remain separate layout modes. Hover, focus, selection,
scope, and admitted activity are independent inputs.

| Plate | DOM authority | Layout and scene | Overlay and worker | Backend/evidence |
|---|---|---|---|---|
| `01-registry-overview.png` | canonical project scope, registry ledger, field toolbar, source/state legend and exact accessible project table | measured recency-by-X and indexed-mass-by-Y project bodies render as a luminous orthographic registry field; repository hubs use named relations rather than force proximity | worker performs bounded label priority and density clustering without changing measured coordinates; overlay owns axes, counts, selection and minimap | project registry, project identity, repository/worktree membership, index mass/recency and graph availability from the project-scoped generated contracts |
| `02-project-hover.png` | focused registry row and inspect-only project summary remain keyboard-equivalent; hover never changes global scope | scene isolates the real selected project neighborhood and dims unrelated material without moving nodes or firing activity | focus halo and relation legend are overlay state; hover/focus causes no worker relayout | existing project and named relationship records only; inspection creates no activity or inferred scope |
| `03-repository-zoom.png` | breadcrumb, repository/project identities, Fit/back controls and exact membership table | controlled orthographic camera focuses a repository hub and its registered project/checkouts while retaining stable registry coordinates | overlay owns camera window, minimap, focus path and zoom readout; worker expands only the selected density cluster | repository identity and project/worktree membership must be explicit; a repository hub remains inspect-only and does not silently set project scope |
| `04-project-scoped.png` | explicit project scope/clear control, authority panels, graph ledger and inspector | scene switches to the selected project's graph/topology projection with stable identity-based selection, not a cropped all-project approximation | worker lays out or clusters the project graph for semantic zoom; overlay owns scope title, axes, selection and typed renderer state | selected project graph, memory, analytics, identity, checkout and freshness authorities remain independently qualified through the project gateway |
| `05-admitted-activity-synapse.png` | admitted-event ledger, feed state, exact event identity and textual one-hop continuation | the exact touched project/node blooms and at most one evidenced relation conducts; the base layout and camera remain unchanged | overlay owns timestamp, heat legend and static reduced-motion strike; worker performs identity lookup only and never invents a relation | real admitted SSE/activity plus stable project/node/relation identities; hover, loading, link health and selection can never create a synapse |

## Delivery final-state implementation map

All files below are under `08-delivery/final/`. DOM means accessible product
authority; Scene means the high-fidelity field; Layout/worker means stable
geometry and density; Backend means source projection requirements.

| Plate | DOM authority | Layout and scene | Overlay and worker | Backend/evidence |
|---|---|---|---|---|
| `01-global-pr-inbox.png` | project/agent filters, virtualized PR inbox, umbrella list, selected inspector, exact status text | umbrella and related-PR topology at outcome zoom | grouped counts, focus paths, minimap; cluster all projects without one row per relation | project registry + provider PR reads + explicit/inferred cross-project correlation |
| `02-project-scoped-inbox.png` | project scope, local PR list, correlated external PR list, filters and selection | selected project as primary rail; cross-project related PRs as qualified branches | distinguish in-project versus correlated; worker clusters by repository/outcome | project gateway + PR identity + agent-touched/correlation basis |
| `03-umbrella-delivery-graph.png` | outcome summary, risk/coverage, individual PR entry points, exact table fallback | multi-repository PR cluster with stable outcome spine | bundle supporting branches, preserve PR hover/focus/selection | umbrella identity, PR membership relation and grade, provider state per PR |
| `04-pr-journey-overview.png` | Start/Continue review, filters, progress, branch navigator, breadcrumbs | horizontal objective-to-delivery spine with episode and branch topology | time axis, density breaks, minimap, future/unknown gaps | ordered semantic episodes joining task, session, Git, code, check, review, and release sources |
| `05-temporal-replay.png` | play/pause, scrub, previous/next, speed, loaded-tail state and timestamp | reveal events only through cursor; maintain stable lanes | cursor, unrevealed-future mask, density summary and motion-independent direction | recorded timestamps/sequence, pagination, feed state, explicit undated records |
| `06-expanded-agent-branches.png` | virtualized branch navigator, search/filter/pin/focus-path controls | agent/subagent spawn, work, handoff, and rejoin branches | edge bundling, collapse brackets, selected neighborhood expansion | parent/child, task, session, handoff, worktree, commit and result identities |
| `07-honest-partial-unknown.png` | missing-source ledger, candidate identities, local adjudication entry point | retain visible gaps, ambiguous branches and stale segments | dashed/gapped paths, patterns and non-color labels | absent, not ingested, private, stale, ambiguous and inferred states remain distinct; adjudication appends |
| `08-review-coverage-diff-checks.png` | exact diff, review threads, check matrix, coverage filters and keyboard traversal | optional compact context field only; exact review DOM dominates | link selected hunk/check/thread back to its journey episode | repository diff, provider review/check reads, source-backed weak/unreviewed findings |
| `09-follow-the-story-review-workspace.png` | full Story/Code and Impact/Evidence/Feedback workspace, readable diff, previous/next, coverage map, local feedback | timeline stays orientation/context; selected causal neighborhood may occupy a resizable pane | source anchors, selected episode path, diff-to-decision highlights | sample is explicitly synthetic; production binds each statement and code selection to exact source IDs |
| `10-decision-to-code-pr743.png` | eight-step review, exact two-file YAML diff, resizable/collapsible journey/evidence panes, full-width code focus, inline local feedback | recovered producer/subagent journey and workflow dependency/guard topology | exact hunk anchors, source-class labels, review-finding links, merge/#707 continuation | verified PR #743 transcript, two-file diff, checks, three unresolved findings; private reasoning unavailable |
| `11-local-first-provider-not-configured.png` | repository field, local Git status/commit/worktree evidence, provider setup/unavailable explanation | existing measured repository field may remain bounded legacy renderer | exact local selection and accessible repository table | local authority remains useful; provider absence is not empty PR success |
| `12-dense-128-agent-delivery.png` | virtualized branch navigator, workstream/task/repository filters, search, pins, path-to-root/path-to-outcome focus, and synchronized exact tree/table fallback | deterministic workstream clusters and branch bundles render through the shared scene runtime; semantic zoom moves from outcome to workstream, agent, episode, and event | worker aggregation, reconciled bundle counts, representative frontier selection, edge bundling, density summaries, minimap, and focus-plus-context expansion | stable agent/subagent/task/session/worktree/commit identities and explicit relation grades; exact source records remain available without expanding every scene node |

The dense Delivery state must pass the shared 5,000/20,000-event qualification.
Its `128 agents` label is a concept population, not a hard product ceiling or a
production count. Implementation captures must state whether totals include
root agents and subagents, whether an identity can appear in more than one
workstream, and how displayed bundle totals reconcile with the unique-agent
total.

## Loom final-state implementation map

All files below are under `03-loom/final/`.

| Plate | DOM authority | Layout and scene | Overlay and worker | Backend/evidence |
|---|---|---|---|---|
| `01-follow-loaded-tail.png` | follow/pause control, printed feed/pagination state, filters, keyboard event list | horizontal execution path advances to newest event in the loaded page | loaded-tail cursor and current window; no false live animation | hooks, commands, messages, files, tasks, spawns, commits and checks available in the loaded projection |
| `02-temporal-replay.png` | play/pause, scrub, previous/next, speed and selected-event controls | only events at/before cursor are revealed; branches keep stable lanes | time ruler, cursor, unrevealed future, density breaks and minimap | recorded timestamp/sequence and loaded-page boundary; no completeness claim |
| `03-branching-execution.png` | branch collapse/expand, focus path and synchronized exact tree/list | spawn branches diverge; handoffs/results/commits rejoin with named relations | curved bundled paths and visible focus treatment | explicit parent/child and handoff/result records; muted collapsed branches retain table fallback and focus contrast |
| `04-dense-100-agents.png` | virtualized branch navigator with search/filter/pins and exact reconciled counts | outcome/workstream/task bundles first; expand selected neighborhood on demand | worker aggregation, edge bundling, density bodies, representative frontier nodes | counts define whether they include roots/subagents and whether cross-workstream membership is distinct or double-counted; totals must reconcile |
| `05-selected-event-evidence.png` | full selected-event workspace with hook/transcript/diff/task/code/evidence modes | causal neighborhood is context, not a tiny inspector | selected path and event anchors; scene yields room to exact evidence | stable event ID links exact source, affected artifacts, provenance and typed gaps |
| `06-feedback-continuation.png` | local comment/challenge/task feedback composer and status | feedback node links to later observed revision/test continuation | causal continuation is drawn only after a real linked later record | local TraceDecay feedback authority only; provider/GitHub unchanged without a real write path |
| `07-evidence-gaps.png` | missing/ambiguous/stale source ledger, candidate navigation and local adjudication | gaps remain spatially visible and selectable | dashed/gapped paths, stale pattern and non-color state | record resolution appends an adjudication; it never rewrites source facts or fabricates attribution |

## Remaining workspace final-state implementation map

These eleven final plates complete the 35-plate implementation reference.
`DOM-only` means the exact interactive product is intentionally implemented in
semantic DOM rather than forcing a scene renderer into a ledger or form. Small
SVG or ECharts views remain subordinate to the DOM authority and their exact
accessible fallback.

| Workspace and plate | DOM authority | Layout and scene | Overlay and worker | Backend/evidence |
|---|---|---|---|---|
| Explorer — `01-browse-query-lanes.png` | admitted query form, cancel state, four independently qualified result lanes, virtualized hit lists and full result inspector | DOM-first four-lane composition; no high-fidelity scene runtime | optional worker filters/sorts large served result sets; progress and source coverage are ordinary DOM, not decorative animation | `ExplorerPage.tsx`, controller/lane models, `/api/explorer/queries`, graph/LCM/memory sources and generated query-run/read-context contracts |
| Sessions — `01-session-provenance-inspector.png` | provider-qualified session list, search, pagination, transcript/summary inspector, token provenance and exact text | DOM-only list/inspector; a bounded timeline/density strip may use accessible SVG or Canvas with the same selection | virtualized session/message rows and optional density aggregation; no scene-only session identity | `SessionsPage.tsx`, `SessionInspector.tsx`, Hermes LCM overview/timeline/search/session reads and canonical paged session store |
| Agents — `01-delegation-topology.png` | agent/task list, handoff ledger, token/tool/failure details and synchronized exact tree | deterministic parent/child/dependency DAG in small accessible SVG; dense fan-out graduates to the shared clustering/layout model rather than one row per agent | overlay owns handoff/frontier labels; worker clusters large agent groups by task/workstream and preserves path-to-root/outcome | `AgentsPage.tsx`, subagent tree/handoff/failure models, analytics agent/subagent reads, Work graph and handoff application authority |
| Code — `01-semantic-cortex.png` | symbol search, node evidence, call/path/strata controls, diagnostics, freshness, inspector and exact code | Graphology remains the topology model; the winning shared scene runtime renders Cortex depth/curves/picking, while the specialized Trace view may retain its truthful renderer | worker handles graph layout/clustering/semantic zoom; overlay owns labels, camera, path selection and renderer state | `CodePage.tsx`, graph/structure/evidence/call-chain/diagnostics/freshness APIs and generated code-index contracts |
| Knowledge — `01-fact-provenance-cameras.png` | fact ledger/detail, trust history, curation controls, oplog and source coverage | DOM-first cameras; bounded geometry may use accessible SVG and real quantitative history may use the single ECharts host; no decorative neural scene | virtualized facts/oplog and bounded geometry aggregation; charts retain DOM summaries | `KnowledgePage.tsx`, knowledge views, memory queries, holographic fact/detail/trust/projection/similarity/oplog routes and real curator authority |
| Automations — `01-scheduler-run-ledger.png` | scheduler status and real pause/resume gate, job/skill/fact-receipt/run/artifact ledgers and exact failure detail | DOM-only ledger; no signature scene runtime | virtualized histories and bounded filters only; artifact integrity/state patterns remain DOM | `AutomationsPage.tsx`, `RunHistory.tsx`, scheduler/jobs/skills/fact-receipts/runs/artifacts/outcomes routes and daemon automation authorities |
| Observatory — `01-system-evidence-overview.png` | independent evidence sections for storage, code index, budgets, findings, hooks, adoption and topology; exact reasons and source times | DOM-first evidence stack; ECharts is limited to named measured time series and never becomes a global-health radial scene | optional time-series downsampling worker; no aggregation may collapse independent authorities into nominal health | `ObservatoryPage.tsx` and its inspector/evidence models, storage telemetry/findings, freshness, observatory, analytics, doctor and Work topology reads |
| Costs — `01-provider-spend-attribution.png` | provider spend ledger, attribution/coverage, pricing identity, range controls and exact unpriced/null states | DOM-first accounting surface; ECharts only for a real dated spend series | virtualized provider rows and bounded series aggregation; no high-fidelity scene | `CostsPage.tsx`, canonical costs/topology views, savings/costs/Work topology routes and generated spend/coverage contracts |
| Settings — `01-effective-configuration-review.png` | filterable effective values, provenance, writable/locked state, staged review dialog, CAS result, multi-root and Remote Brain panels | DOM-only forms/tables/dialogs; no scene runtime | list virtualization only when measured; no worker owns configuration truth or optimistic mutation | `SettingsPage.tsx`, editor state machine/mutation controller, settings/capabilities/multi-root/remote routes and canonical configuration CAS authority |
| Work — `01-task-dag-board.png` | projection switcher, board/task list, selected evidence, task commands and exact table fallback | deterministic small SVG DAG/timeline over one immutable graph revision; dense topology may reuse the worker layout but not the luminous scene unless measured utility requires it | worker lays out large DAG/task clusters and culls labels; overlay owns dependency paths and selection | `WorkPage.tsx`, Work board/views/commands/evidence/activity models and canonical Work graph/views/topology/attempt/mutation routes |
| Workflows — `01-definition-lifecycle-ledger.png` | definition ledger, immutable version/detail, steps, pinned policy/config/catalog digests, lifecycle controls, CAS conflict and run lookup | DOM-only table/detail/form workflow; no decorative lifecycle scene | virtualized definitions/history only; lifecycle state and revision remain DOM text | `WorkflowsPage.tsx`, workflow queries/routes and canonical definition/history/activate/retire/reject/get-run application contracts |

## Proof-of-capability renderer spike

The spike uses representative real Brain, Loom, and Delivery projection data,
including exact missing/ambiguous states and a 100+ agent fan-out. It compares:

1. the current Graphology/Sigma and SVG baseline;
2. React Three Fiber v9-compatible-family with Three.js; and
3. PixiJS v8.

Candidate packages may coexist only inside the isolated spike. The production
dependency set accepts one winner.

### Required workloads

- a 5,000-event history for interactive development and ordinary review;
- a 20,000-event history with 100+ agents/subagents, nested spawn branches,
  handoffs, rejoin edges, cross-repository PR rails, gaps and dense time spans;
- the real PR #743 two-file journey for exact picking and DOM evidence pivots;
- a Brain field with luminous bodies, measured activity, curved links and
  stable selection; and
- narrow, 200% zoom, reduced-motion and forced-colors variants.

### Measurements

| Capability | Pass evidence |
|---|---|
| Visual fidelity | reviewed 1440px captures show orthographic depth, restrained bloom, curved branch grammar, exact selected state and readable DOM hierarchy without screenshot backgrounds |
| Stable layout | identical projection/revision produces identical node, lane, bundle and minimap coordinates across reloads and candidate renderers |
| Density | 5k and 20k scenes retain navigable bundles, semantic zoom, correct counts and no unbounded DOM-node growth |
| Picking | pointer and keyboard selection resolve the same stable event/source ID; no stale selection after layout revision |
| Curves/bundles | nested spawn, handoff, rejoin and cross-repository paths remain distinguishable and source-qualified |
| Semantic zoom | outcome through event levels change information detail, not only camera scale; list/table stays synchronized |
| Playback | scrubbing deterministically reveals records through the selected timestamp and never reveals or animates future evidence |
| Responsive behavior | 1440, 768 and 320 CSS-pixel layouts remain operable; canvas yields to stacked/focus modes rather than shrinking body text |
| 200% zoom | navigation, pane controls, code, transcript, source labels and feedback reflow without clipping or page-level horizontal loss |
| Reduced motion | static states retain direction, selection, causality, activity and replay position; no required travelling effect |
| Runtime cost | record load/layout, settle time, pick latency, memory, GPU resources, idle frame cost and active playback frame cost for both densities |
| Failure behavior | context loss, worker failure, unsupported GPU, oversize input and renderer initialization failure expose exact DOM fallbacks and typed reasons |

Set budgets from measurements on supported hardware and browsers; this concept
document does not invent numeric thresholds. Record exact browser, device,
renderer versions, projection generation, counts, memory, timings and capture
paths beside every result. Verify the exact React Three Fiber/React 19/Three.js
or Pixi/Rsbuild package compatibility in the spike.

Choose the renderer from the complete matrix. Fidelity alone is insufficient;
so is a fast empty fixture. If neither candidate passes, report the failing
criteria and revise the scene design or runtime with evidence.

## Shared scene migration

After the spike selects a renderer:

1. Introduce one renderer-neutral `TemporalSceneModel` and one shared scene
   adapter under the dashboard visualization layer.
2. Build the worker layout and DOM synchronized fallback before page-specific
   visual polish.
3. Migrate one representative real journey end to end, including URL state,
   exact evidence pivots, typed failure, 20k density and reduced motion.
4. Migrate Loom and Delivery through the same scene/layout primitives.
5. Migrate Brain's signature field where the winner materially exceeds Sigma;
   retain or remove the legacy Sigma host according to actual remaining callers.
6. Remove a renderer dependency with its final production caller. Do not keep
   duplicate scene authorities indefinitely.

Page components compose DOM workspace modes and pass parsed domain models to
the scene. They do not create WebGL contexts directly, duplicate the layout,
decode unknown JSON, or assign evidence grades.

## Accessibility, density, and browser acceptance gates

Every final Delivery and Loom state must pass all of the following:

- every pointer action has a keyboard route with visible focus and logical
  order; branch search, previous/next, playback, selection, comments and pane
  controls are keyboard operable;
- every scene has a synchronized exact list/tree/table/transcript/diff fallback
  with the same stable selections and URL destinations;
- dense histories virtualize exact rows, lazy-load evidence, aggregate the
  scene, and never solve scale by shrinking text;
- bundle and header counts state their population semantics and reconcile;
- muted/collapsed branches retain sufficient contrast and a non-visual
  equivalent;
- at 200% zoom, side rails resize, collapse, stack, or yield to a dedicated
  code/evidence focus mode; exact long code lines scroll within their pane and
  are never silently clipped;
- at 320px, the scene becomes overview/context while review content uses a
  single-column mode; no essential action is canvas-only;
- reduced motion removes travel, bloom modulation and interpolated camera
  movement while preserving static heat, direction, selection, causality and
  cursor position;
- forced colors and non-color patterns preserve evidence/state/selection;
- screen-reader announcements are scoped to meaningful state changes and do
  not narrate every animated event;
- served empty, absent, partial, stale, offline, denied, redacted, ambiguous,
  unavailable and unsupported states remain distinct; and
- each visual audit visibly retains `CONCEPT / SYNTHETIC` unless the plate is a
  deterministic reconstruction with an explicit verified evidence packet.

## Integration acceptance

A browser slice is complete only when:

1. generated Rust/TypeScript contracts agree and no handwritten shadow DTO was
   introduced;
2. unit/DOM tests cover layout determinism, typed gaps, URL round trips,
   selection synchronization, branch count reconciliation and reduced motion;
3. production-bundle Playwright journeys cover keyboard, 1440/768/320, 200%
   zoom, pane resizing/collapse, exact fallback and renderer failure;
4. the renderer spike or selected runtime passes the 5k/20k matrix with
   recorded measurements;
5. a real enrolled daemon profile proves source identity, pagination,
   correlation grades, feedback persistence, and exact evidence pivots;
6. provider actions remain read-only unless their authenticated production
   mutation path is separately proven; and
7. full-resolution captures are reviewed against the final product briefs,
   with rejected/superseded concept assets excluded from the implementation
   reference.

Build, typecheck, contract, test and visual-audit commands must run from the
exact integration head. A green concept fixture, a compile-only result, or a
pretty empty scene is not completion evidence.
