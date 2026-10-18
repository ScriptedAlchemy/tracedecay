# Dashboard task map

Issue #4220 asks which user task each dashboard surface serves, how users reach
it, what it reads, and what it costs to keep, so that overlapping surfaces can
be merged or removed. This page is that map, then the decision for every
surface. The "After" section records what shipped with the issue.

## Evidence and its limits

- **Usage telemetry does not exist for the dashboard.** `tracedecay_analytics`
  (queried across all projects, 60 days) counts MCP tool calls only. No event
  records a dashboard route view. This map therefore cannot say which surface
  is used. It says which surface is reachable, what it reads, what it can
  uniquely answer, and what it duplicates.
- **Entry points** were found by searching `dashboard/src` for links, the
  command palette, and `scopedWorkspacePath`, and `crates/`, `plugin/`,
  `skills/`, `docs/` and `README.md` for route paths and query parameters.
- **Why each was added** comes from `git log --follow` on each workspace. Most
  first commits carry no written rationale (`feat(dashboard): wire nine
  workspaces to live backend surfaces`, 2026-07-23, created ten of the
  fourteen). Where a commit body or an issue number gives a reason, it is cited.
- **Exclusive size** is the source reachable from `src/app/main.tsx` that stops
  being reachable when the workspace page is removed, so it includes shared
  `viz/` and `data/query/` modules used by nothing else. Test size is the
  colocated `*.test.*` files. Both were measured at master `334a226d60`.

### What points at the dashboard from outside it

Only two daemon-emitted dashboard routes exist. `delivery_api.rs` emits
`/code` and `/code?view=compare&head=...&head_revision=...` as `href`s in
delivery payloads. `tracedecay dashboard`, the `tracedecay_dashboard` MCP tool
and the Hermes wrapper print or open the dashboard root only. `README.md` and
`docs/dashboard.md` name no workspace. `docs/USER-GUIDE.md` mentions the Loom
view (test runs on a session lane) and the Settings page by name. No skill or
plugin file links to a workspace route. Every other link to a route lives
inside the dashboard.

Links between workspaces (the ones a redirect must keep working):

| From | To | Where |
|---|---|---|
| Sessions, Agents, Loom, Delivery | each other's session pivots (`/sessions`, `/loom?loomSession=`, `/agents`) | `sessions/SessionPivots.tsx`, `delivery/evidence.ts` |
| Delivery, Loom, Work proximity panel | `/code`, `/code?view=compare...`, `/code?view=shared-code&symbol=` | `delivery/review.ts`, `viz/proximity/ProximityPanel.tsx` |
| Explorer | `/code?symbol=` | `explorer/Inspector.tsx` |
| Delivery, Observatory | `/settings` | `delivery/LocalFirstWing.tsx`, `observatory/EvidenceInspector.tsx` |
| Observatory | `/agents` | `observatory/EvidenceInspector.tsx` |
| Proximity panel | `/sessions`, `/work`, `/delivery` | `viz/proximity/ProximityPanel.tsx` |
| Command palette | every workspace; a picked project goes to `/brain` | `app/shell/CommandPalette.tsx` |

## Before: the fourteen workspaces

Sizes are exclusive source LOC / colocated test LOC. "Writes" lists the
operations that must survive any consolidation.

| Workspace | Task it serves | Reads | Writes | Size | Why added |
|---|---|---|---|---|---|
| Brain `/brain` (also `/`) | Unscoped, which projects the daemon looks after. Scoped, what TraceDecay holds for one project. | `/api/projects`, `/api/events`, graph `overview` and `subgraph`, holographic `status`, analytics `overview`, `/api/doctor/findings` | none | 4,285 / 1,808 | Landing page from the first dashboard commits. The scoped half draws the same graph slice as Code. |
| Explorer `/explorer` | One query answered separately by the code, session and fact stores. The semantic lane is a permanent typed absence. | `/api/explorer/queries` run API, session size and read-context, three overview routes | create and cancel a query run | 3,234 / 2,092 | `65f792c03d` fan-out across stores; `a685d76c8c` four independent lanes. Lanes duplicate Code, Sessions and Knowledge search. |
| Loom `/loom` | See every session as a lane on one time axis with tools, commits, edits, PRs and test runs, then replay one. | `/api/loom/temporal`, LCM `session/{id}` and `timeline`, `subagent-tree`, `POST /api/feedback/proximity` | none | 4,967 / 1,478 | `df70b34ebf` canvas; rebuilt as a temporal field in `1396c7891a`; proximity in #1356, tool and PR events in #2371, test runs in #2583. |
| Sessions `/sessions` | Find a retained session by volume, index or full text, then read transcript, compaction and Git relations. | LCM `timeline`, `overview`, `search`, `session/{id}`, `/api/loom/temporal` | none | 2,639 / 1,875 | First dashboard wiring commit; transcript search and drill-down added later; cross-workspace pivots in `caef6b983b`. |
| Agents `/agents` | Who delegated to whom: delegation tree, handoff and token frontier, attempt failures, tool usage. | analytics `usage`, `underused`, `diagnostics`, `agents`, `subagent-tree`, `POST /api/work/views`, handoff `list-task` | none | 6,856 / 3,166 | `4ea650e57a` over tool-usage analytics; `b1388f062d` added handoffs and failures; `a7572132fe` added the rings and timeline renderers. |
| Code `/code` | Browse the code graph and look up symbols. Lenses: cortex, trace, shared code, compare. | graph `overview`, `search`, `subgraph`, `strata`, `node/*`, `call-chain`, `path`, `shared-code`, `compare`, `/api/code-index/freshness`, code-diagnostics | refresh diagnostics, toggle language, set idle backfill | 12,993 / 5,727 | First wiring commit; Shared Code and Compare in #1331. Target of daemon-emitted hrefs. |
| Knowledge `/knowledge` | Inspect the project's fact store: ledger, geometry, curation outcomes, oplog. | holographic list, `status`, `fact/{id}`, `projection`, `similarity`, `oplog`, `/api/automation/runs` and `outcomes` | run the automatic curator | 5,902 / 3,938 | `d76ec6e1bc` trust-axis fact explorer; curation run receipts in #2350. |
| Delivery `/delivery` | Triage pull requests across projects: inbox, umbrella, journey, review. Read-only. | `/api/delivery/inbox`, `/api/projects/{id}/delivery/overview` | none | 6,965 / 1,700 | `38c61da18e` git surface; registry-aware inbox in #1350. Its Compare target is Code. |
| Automations `/automations` | Read the scheduler, managed jobs, skills, fact outcomes and the run ledger. | `/api/automation/scheduler/status`, `jobs`, `skills`, `automatic-fact-receipts`, `runs` and artifacts | pause and resume the scheduler | 1,928 / 1,345 | `5f3d9acc8e` scheduler surface; run ledger in `1349645e8c`; scope-gated writes in `5fec234158`. |
| Observatory `/observatory` | Is the system's evidence present and fresh, per authority, with Doctor findings and storage budgets. Carries the global Doctor dot. | `/api/observatory`, `/api/storage/telemetry`, `/api/doctor/findings`, code-index freshness, analytics `diagnostics` and `hints`, `/api/work/topology-metrics` | none | 8,230 / 5,299 | `df79dd8825` first real workspace (storage telemetry); eleven sources added over time. |
| Costs `/costs` | Provider spend and token usage, priced, by range and provider. | `/api/plugins/savings/overview` and `models`, `/api/costs`, `/api/work/topology-metrics` | none | 2,506 / 1,300 | `5f3d9acc8e` costs ledger; canonical observations in `1209783405`. |
| Settings `/settings` | Review and edit effective configuration with provenance and write gates. | `/api/settings`, `/api/remote/status`, `/api/capabilities` | three compare-and-swap PATCH routes | 5,968 / 2,868 | First wiring commit; provenance in `dabfdcb487`; validated edits in `cc81522fed`. |
| Work `/work` | Track tasks: the product graph as board, DAG and further projections, with a task inspector and create command. | `/api/work/views`, `list-attempts`, `execution-history`, `topology`, `topology-metrics`, evidence, `POST /api/feedback/proximity` | prepare and admit work commands | 12,990 / 5,539 | `2d2dedcfec` truthful contract gate (2026-07-29); projections added 2026-08-07 through 2026-09-24. |
| Workflows `/workflows` | Browse workflow definitions and versions; activate, retire or reject one; look up a run. | `/api/application/workflow/*` via Work's `callWork` | activate, retire, reject | 2,554 / 1,059 | `b5bf05b6b6` (2026-08-19), no stated reason. |

Observations that shaped the decisions:

- Sessions, Loom and Agents share one selection model (`sessions/model.ts`,
  `SessionPivots`) and two endpoints. Loom and Sessions both read
  `/api/loom/temporal` and LCM `session/{id}` and both draw a session's
  commits, edited files and branch spans. Loom's lanes and Agents'
  `DelegationTimeline` draw the same tree, time across and delegation down,
  from the same `subagent-tree` read.
- Knowledge's curation camera mounted `automations/RunHistory.tsx` (233 lines),
  a compact copy of Automations' `RunLedger` and `RunInspector` over the same
  `/api/automation/runs` and artifact routes. Its own comment says so.
- Costs' `TopologyMetricsCosts` and Observatory's `ExecutionTopologyMetrics`
  render the same `useWorkTopologyMetrics` read.
- Explorer, Knowledge, Costs, Automations, Observatory and Workflows have no
  inbound link from any other view, the daemon, a doc or a skill. Their only
  entry is the rail and the command palette.

## Before: the eight Work projections

All eight are derived from the one `/api/work/views` read; "extra reads" lists
what a projection issued on top of it. The switcher offered all eight in one
tab list, with `Concurrent Attempts` shown only when the selected task had
more than one observed attempt.

| Projection | Task it serves | Extra reads | Exclusive source | Decision |
|---|---|---|---|---|
| Board | Where is each task: grouped by the furthest gate passed. Default. | none | `WorkBoard` 308 | Keep. |
| DAG | What blocks what: strata, critical path, gating edges, backward dependencies. | none | `WorkDagView`, `WorkDagBoard`, fitted field, relation layer, layout: about 1,750 | Keep. |
| Matrix | The same declared relations as a cell grid, for dense graphs. | none | `DagMatrixField` 265, `workDsmModel` 97 | Remove. Same relations as the DAG; the DAG already fits dense graphs and has an exact table. |
| Timeline | Runs woven across tasks they attached evidence to. | `list-attempts` | `WorkTimelineView` 532, `WorkExecutionRecord` 455, `workAttemptModel` 407 | Remove. A task's attempts and evidence are in the inspector (`WorkEvidencePanel`, attempts row) and the Work activity ledger. |
| Causal | Declared causal candidates against evidence at both ends. | none | `WorkCausalView` 596, causal model about 150 | Remove. Candidates are declared data, so the view repeats the graph read. Dependencies are in the DAG. |
| Workload | Runs as regions sized by tasks touched. | none | `WorkWorkloadView` 719, workload model about 120 | Remove. A region chart over counts the board and inspector already state. |
| Topology | Attempts placed onto executors and worktrees under one topology generation; integration and stack cards. | `topology`, `topology-metrics` | `WorkTopologyView` 372, `WorkTopologyAccounting` 487, accounting models about 1,540, topology models 350 | Remove the view. The selected task's placement stays in the inspector (topology read issued once a task is selected). Aggregate metrics stay in Observatory and Costs, which read the same hook. |
| Concurrent Attempts | Overlapping attempts of one task joined to provider and session proximity. | `execution-history`, evidence, `POST /api/feedback/proximity` | view 82, model 170, query 80 | Remove. Visible only for a task with more than one attempt; the proximity panel stays in Loom, which is where sessions are compared. |

Entry points for the removed projections: only the `?view=` switcher and
saved URLs. No other view, daemon payload, doc or skill links to
`/work?view=<name>` (searched). The server routes behind them (`list-attempts`,
`execution-history`, `topology`, `topology-metrics`, the MCP and CLI Work
tools) are untouched.

## Task families and decisions

Five top-level destinations replace fourteen. A family page carries a visible
tab strip of its views; each view keeps its own page.

| Family | Task | Views | Decision for each |
|---|---|---|---|
| **Code** `/code` | Look up and compare code, and see what is shipping. | Code, Delivery | Code: keep (daemon emits its hrefs). Delivery: merge in as `/code/delivery`; its only outbound data link is Compare in Code, and it shares no read with any other workspace. |
| **Memory** `/memory` | Retrieve what TraceDecay remembers: projects, facts, sessions, agent runs. | Brain, Explorer, Knowledge, Sessions, Loom, Agents | All merge in. Brain is the family landing (`/` opens it). Explorer is kept: its run API and cancel control and per-lane typed absence have no other home. Sessions is kept: only it finds by text, volume and page. Loom is kept: only it plots many sessions on one axis and replays one. Agents is kept for handoffs, failures and usage, which nothing else shows. Remove Agents' timeline view (Loom draws it) and Knowledge's duplicate run history (Automations owns the ledger). |
| **Work** `/work` | Track tasks and the workflows that execute them. | Work, Workflows | Work: keep with board and DAG only. Workflows: merge in as `/work/workflows`; it already runs on Work's `callWork` plumbing, and the Work page itself says execution belongs to it. |
| **Operations** `/operations` | Debug the running system: health, scheduled automation, spend. | Observatory, Automations, Costs | Observatory is the family landing and keeps the Doctor dot. Automations and Costs merge in. Costs' topology accounting card stays: Plan 26 places accounting facts beside spend, and it is a different presentation of a read Observatory also draws (recorded below as a follow-up, not removed). |
| **Settings** `/settings` | Review and change configuration. | Settings | Keep top-level. Delivery and Observatory link here, and it is the only write surface for configuration. |

Redirects (query string and hash kept, including `scope`):

| Old | New |
|---|---|
| `/brain`, `/` | `/memory` |
| `/explorer` | `/memory/explorer` |
| `/knowledge` | `/memory/knowledge` |
| `/sessions` | `/memory/sessions` |
| `/loom` | `/memory/loom` |
| `/agents` | `/memory/agents` |
| `/agents?view=timeline&sessionId=S&sessionProvider=P` | `/memory/loom?loomSession=[P,S]` |
| `/delivery` | `/code/delivery` |
| `/workflows` | `/work/workflows` |
| `/work?view=matrix` | `/work?view=dag` (handled in the page) |
| `/work?view=timeline`, `causal`, `workload`, `topology`, `concurrent-attempts` | `/work` board, selected task kept (handled in the page) |
| `/observatory` | `/operations` |
| `/automations` | `/operations/automations` |
| `/costs` | `/operations/costs` |
| `/code`, `/work`, `/settings` | unchanged |

## After

Measured against master `334a226d60`.

| Measure | Before | After |
|---|---|---|
| Top-level rail entries | 14 | 5 (Code, Memory, Work, Operations, Settings) |
| Views reachable | 14 workspaces | the same 14, each one click from its family's visible tab strip and one keystroke from the command palette |
| Work projections | 8 | 2 (board, DAG) |
| Work production source (non-test TS/TSX) | 13,528 lines | 6,196 lines |
| Work test source | 5,539 lines | 2,990 lines |
| Dashboard production source | 117,597 lines | 109,280 lines (-8,317) |
| Dashboard vitest | 219 files, 2,125 tests | 217 files, 2,049 tests (after merging newer master, which added tests of its own) |
| Retired paths with a redirect | none | 11 paths, plus `/`, the Agents timeline link, and every removed `/work?view=` value |

What was removed, and why (evidence is in the tables above):

- **Work: matrix, timeline, causal, workload, topology and concurrent-attempts
  projections**, with their view components, the attempt-record model, the
  topology accounting cards and models, the causal, workload and weave readings
  in `workViewsModel`, the attempt and execution-history query hooks and their
  routes, and their tests. A task's attempts, evidence and placement are read
  in the inspector; the placement read is now issued when a task is selected
  rather than when a camera is chosen. The Work server routes, MCP and CLI
  tools are untouched, and Observatory and Costs keep `useWorkTopologyMetrics`.
- **Agents: the timeline view.** Loom draws the same tree against time from the
  same read. `/agents?view=timeline` opens Loom on the linked session.
- **Knowledge: the embedded run history.** The curation camera keeps the
  curator control and the outcomes, and links to the one run ledger in
  Automations.

Kept on purpose, with the overlap recorded for a later change that has a
replacement to point at:

- Scoped Brain draws the same graph slice as Code.
- Explorer's lanes repeat the search each of Code, Sessions and Knowledge has.
  It stays because its run API, cancel control and typed absences have no
  other home.
- Loom and Sessions both render a session's commits, files and branch spans.
- Costs' topology accounting card and Observatory's render one read.
- Agents and Observatory fetch `/api/plugins/analytics/diagnostics` under two
  cache keys.

Operations and callability preserved: every write control (scheduler pause and
resume, curator run, settings PATCH routes, workflow activate, retire and
reject, explorer run and cancel, work prepare and admit, code-diagnostics
refresh and toggles) is still on a view, and no daemon route, MCP tool or CLI
command was touched.
