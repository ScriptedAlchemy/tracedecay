# TraceDecay V2 Dashboard Frontend

## Status / Role

Normative product plan. Every product PR ships its usable UI slice with its backend behavior. PR14 completes the shared shell and the full Brain, Explorer, Loom, Sessions, Agents, Code, Knowledge, Delivery, Automations, Observatory, Costs, and Settings experience. PR17 adds the first-class Work workspace and task-graph projections owned semantically by [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md).
PR17 also adds the execution-topology lens specified below: optional worktree
and stacked-branch lanes, dependency-commit and merge-order rails,
conflict/proximity evidence, integration proposals and receipts, test/CI state,
and temporal replay over the same canonical Work selection.

## Outcome

The dashboard is TraceDecay's world-class flagship product surface: a polished,
highly interactive connected brain across projects that preserves precise
repository, worktree, branch, session, agent, time, provenance, coverage, and
authority scope. Visual quality serves comprehension rather than novelty.

## Owns

- Navigation, responsive layout, accessibility, interaction state, and client-side presentation.
- Typed API consumption, query caching, optimism only for the closed
  presentation-state allowlist below, and SSE-driven refresh.
- Linked visual exploration across product data and provenance.
- Rendering typed configuration, diagnostics, Doctor findings, legal
  remediation choices, recovery guidance, and operation progress supplied by
  daemon/application owners.
- A renderer-neutral graph/timeline view model with stable node/edge IDs,
  typed relations, selection, filters, clusters, layouts, temporal frames,
  provenance, coverage, and accessible table/text equivalents. Renderer
  adapters own drawing and interaction acceleration only.

## Does not own

- Business rules, authorization decisions, storage, indexing, migration, or repair execution.
- The frontend never starts analyzers, opens LSP connections, merges
  diagnostics, or infers health; it only consumes typed daemon APIs defined by
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Doctor finding identity, diagnosis, aggregation, severity, health state, or
  remediation orchestration. The canonical Doctor application kernel shipped
  by the PR14 product slice owns those concerns; this dashboard only renders
  its findings and legal actions.
  [Plan 14](14-historical-failure-regression-matrix.md) is the direct
  regression contract for that kernel, not a runtime source of findings.
- An independent Kanban database, developer plan parser/executor, browser-side
  task scheduler, generic orchestration lab, or edit-bundle editor. PR17's
  Kanban/DAG/timeline/causal/workload views are projections over Plan 24
  application state and Plan 32 runtime receipts.
- Git status, worktree, branch-stack, merge, rebase, cherry-pick, ref, index,
  commit, lease, test, or CI authority. Components may submit only an
  application-supplied typed `dry_run`, `apply`, or `cancel` action reference
  and render the resulting receipt. They never construct Git arguments,
  relocate a hunk, infer a merge result, move a ref, acquire/reclaim a lease,
  launch a test, rerun CI, or treat a card move as repository mutation.
- Arbitrary JavaScript workflow authoring or execution.
- Generated compatibility views, route inventories, or a second model of backend behavior.
- Graph/query/storage authority, renderer-local ranking, health, readiness,
  scheduling, repair, or model-route calculations. Ship a permissive default
  renderer; Cosmograph or another commercial/GPU adapter is optional only
  after license, bundle, capability, offline, accessibility, fallback, and
  performance review.

## Binding implementation map

PR14 keeps `dashboard/` as the single npm package and adds these exact shared
surfaces rather than another dashboard package or route-local contract:

- `dashboard/shell/src/router.tsx`, `WorkspaceShell.tsx`,
  `PrimaryNavigation.tsx`, `ScopeBar.tsx`, and `DeepLinkCodec.ts` own the route
  manifest, responsive shell, active scope, and versioned URL encoding.
- `dashboard/lib/contracts/generated.ts` contains generated daemon DTOs;
  `dashboard/lib/contracts/dashboard.ts`, `planner.ts`, `evidence.ts`, and
  `visualization.ts` contain exhaustive frontend view contracts and runtime
  decoders. Components never define narrower copies of those unions.
- `dashboard/lib/state/DomainStateBoundary.tsx`,
  `MonotoneEventReducer.ts`, and `LinkedSelectionStore.ts` own exhaustive state
  rendering, revision-monotone HTTP/SSE application, and stable-ID selection.
- `dashboard/lib/evidence/EvidencePacketPanel.tsx`,
  `EvidenceTruthStrip.tsx`, `WhyThisResult.tsx`,
  `RetrieverContributionTable.tsx`, `EvidenceCoverage.tsx`,
  `EvidenceCitationList.tsx`, and `EvidenceExpansionDialog.tsx` form the one
  reusable provenance surface.
- `dashboard/lib/visualization/ProjectionViewport.tsx`,
  `ProjectionTable.tsx`, `TemporalPlayback.tsx`, `FilterBar.tsx`,
  `SelectionInspector.tsx`, `rendererAdapter.ts`,
  `defaultCanvasAdapter.ts`, and `projectionManifest.ts` form the
  renderer-neutral interaction surface.
- `dashboard/brain/src/BrainWorkspace.tsx`,
  `dashboard/explorer/src/ExplorerWorkspace.tsx`,
  `dashboard/explorer/src/PlannerQueryPanel.tsx`,
  `dashboard/explorer/src/ParallelSourceProgress.tsx`,
  `dashboard/loom/src/LoomWorkspace.tsx`,
  `dashboard/code-diagnostics/src/CodeDiagnostics.tsx`, and
  `dashboard/observatory/src/{ObservatoryWorkspace,DoctorRoute,RecoveryRoute}.tsx`
  own the flagship investigation journey.
- `dashboard/sessions/src/SessionsWorkspace.tsx`,
  `dashboard/agents/src/AgentsWorkspace.tsx`,
  `dashboard/knowledge/src/KnowledgeWorkspace.tsx`,
  `dashboard/delivery/src/DeliveryWorkspace.tsx`,
  `dashboard/automations/src/AutomationsWorkspace.tsx`,
  `dashboard/costs/src/CostsWorkspace.tsx`, and
  `dashboard/settings/src/SettingsPanel.tsx` own the remaining PR14
  workspaces. Their route contract/DOM cases live in
  `dashboard/test/workspace-route-contracts.vitest.tsx`; no workspace may ship
  as a navigation stub or fixture-only page.
- PR17 adds `dashboard/lib/contracts/executionTopology.ts`,
  `dashboard/work/src/WorkWorkspace.tsx`,
  `projections/{WorkKanban,WorkDag,WorkTimeline,WorkCausal,WorkloadProjection,ExecutionTopologyProjection}.tsx`,
  `WorkItemInspector.tsx`, `TaskProposalPreview.tsx`,
  `AuxiliaryAttemptInspector.tsx`, and the exact topology components under
  `dashboard/work/src/topology/`: `ExecutionTopologyToolbar.tsx`,
  `TopologyLaneBoard.tsx`, `WorktreeLane.tsx`, `BranchStackLane.tsx`,
  `DependencyCommitRail.tsx`, `MergeOrderRail.tsx`,
  `ConflictProximityHeatmap.tsx`, `ExecutionTruthStrip.tsx`,
  `ExecutionTopologyInspector.tsx`, `IntegrationProposalPanel.tsx`,
  `IntegrationOperationDialog.tsx`, and `IntegrationReceiptPanel.tsx`.
  `dashboard/work/src/topology/topologyManifest.ts` normalizes the visual,
  table, and playback representations; `topologyEventReducer.ts` applies the
  generated event union monotonically. All projections consume one Plan 24
  selection and graph version. None of these files imports a Git adapter,
  process/runtime provider, Plan 24 evaluator, Plan 32 scheduler, CI client, or
  persistence API.

The route manifest is `/brain`, `/explorer`, `/loom`, `/sessions`,
`/agents`, `/code`, `/knowledge`, `/delivery`, `/automations`,
`/observatory`, `/costs`, `/settings`, and PR17 `/work`. Entity inspectors use
child routes such as `/code/diagnostics/:findingId`,
`/observatory/doctor/:findingId`, `/observatory/recovery/:operationId`,
`/work/items/:workItemId`, `/work/proposals/:proposalId`, and
`/work/attempts/:attemptId`. A deep link carries opaque scope, selection,
entity/graph version, valid time, observation time, filter revision, lens, and
evidence-anchor IDs. It never carries source/prompt/output bytes, card indexes,
screen coordinates, PID, CWD, mutable labels, or renderer serialization.
Expired, revoked, ambiguous, denied, and stale links render typed states and
never fall back to the active checkout or current version.
The execution-topology lens uses the existing `/work/items/:workItemId` route
with `lens=execution-topology`; operation receipts use
`/work/operations/:operationId`. A topology deep link additionally pins the
work-item version, work-plan/graph version, topology revision,
repository-snapshot digest, worktree generation, branch/ref plus
base/head/merge-base object IDs when available, valid/observation time,
source-watermark digest, and evidence-anchor IDs. Missing or unauthorized
identity renders a typed state; it never resolves by title, branch label,
filesystem path, stack position, card order, or current checkout.

## Typed presentation contracts

`DashboardEnvelope<T>` always carries `schema_revision`, exact scope,
entity/graph version, valid and observation time, source watermark,
authorization, coverage, freshness, domain state, legal action references,
and payload. Unknown schema or union variants render `unsupported_schema`; no
default branch may render healthy or empty. TypeScript switches over domain
unions use an exhaustive `never` check.

`DashboardDomainState` is the discriminated union `loading |
complete_zero_findings | ready | partial | stale | locked | denied |
unauthorized | redacted | conflicting | offline | unknown | cancelled |
timed_out | error | unsupported_schema`. `complete_zero_findings` is legal
only when every required
source is supported and completed, coverage is complete, and the canonical
result count is zero. `unauthorized` means identity is absent or expired;
`denied` means a known identity lacks permission; `locked` means a current
lease/CAS/authority blocks an operation while read access may remain legal;
`redacted` retains only server-supplied `source_kind`, opaque source/anchor
ID, source revision, locator class, safe display label, reason code, and legal
action reference; source bytes, prompt/output excerpts, raw paths, arguments,
environment, and secrets are absent. Zero rows or a friendly illustration
never establish a domain state.

`ProjectionView` carries stable entity, edge, cluster, frame, selection, scope,
coverage, evidence-anchor, legal-action, and cursor identities plus accessible
node/relation/event rows. `ProjectionManifest` normalizes those semantics for
renderer and Work-lens parity. A projection-specific aggregation declares its
hidden stable-ID count and expansion cursor; it cannot silently drop selected
or inaccessible entities.

`PlannerQueryRequest` is a typed application query, not a browser DSL.
`PlannerQueryRun` carries `run_id`, request/plan/merge revisions, required
source IDs, budgets, cursor, and canonical ordering policy.
`PlannerSourceProgress` carries source ID, typed phase/outcome, completed and
total units when known, watermark, freshness, coverage, omissions, and an
application-defined non-secret error code plus user-safe message. The
[Plan 09](09-application-crate.md) application query coordinator owns source
selection, parallelism, ranking, deduplication, merge precedence, and finality
and exposes the run through [Plan 10](10-api-crate.md). The browser submits one
query, renders independent source progress and partial pages, cancels by run
ID, and ignores stale events. After reconnect it deduplicates by
run/event/revision and refetches on a revision gap.

`EvidencePacket` carries packet/query/result IDs and revision, exact scope,
authority outcomes, freshness, coverage, server rank, typed scores,
retriever contributions, server-authored why-this-result reasons, citations,
omissions, and late-context state. Every compact result renders an always
visible `EvidenceTruthStrip` with authority, coverage, freshness, citation
count, omission count, and score kind; none may be hidden only in a tooltip or
drawer.

- Retriever rows preserve server order and identify retriever/revision, stage,
  contribution/abstention/exclusion/unavailable state, score kind and
  descriptor/calibration revision, reason codes, coverage, and anchor IDs.
- “Confidence” is reserved for a calibrated probability or interval that names
  estimator, calibration revision, cohort, horizon, support, and drift
  validity. Lexical/vector/reranker/heuristic/ordinal values retain their raw
  unit, direction, comparison scope, and revision and never share a normalized
  progress bar or average.
- Coverage carries eligible, examined, matched, excluded, omitted, and unknown
  counts; known cap/sampling state; unit; denominator; and omission reasons.
  Unknown denominators never render `0%`, `100%`, a progress meter, healthy
  styling, or `complete_zero_findings` copy.
- Citations carry stable anchor/source/scope/revision identity, an
  application-supplied content-safe locator from the redacted metadata
  allowlist above,
  content digest when legal, and access state. Expansion rechecks
  authorization and returns `available | redacted | locked | unauthorized |
  denied | stale | revoked | expired | missing | corrupt | partial | error`.
  Stale may expose a successor anchor but never redirects automatically.
  Expanded payloads never enter URLs, local storage, analytics, query keys, or
  durable browser caches.
- Late context is revision-monotone and records pending source IDs plus added,
  removed, and superseded anchor IDs. It preserves focus and announces counts,
  source IDs, authority state, and revision changes only; stale-
  generation updates are discarded and revision gaps trigger a canonical
  refetch. Revocation or deletion immediately replaces already expanded
  content with the typed terminal state.

Client optimism is allowlisted to panel layout, viewport, transient brush,
unsaved query/form input, disclosure open/closed state, playback speed, and
focus restoration. No other optimistic state is permitted. It is prohibited
for health, readiness, rank, coverage, task/graph versions, configuration,
leases/attempts, proposal disposition, remediation, and recovery. Renderer
modules may emit selection, brush, expansion, viewport, and playback intents;
they cannot import command construction, policy/Doctor evaluators, task
evaluators, provider/runtime adapters, or persistence.

## Execution-topology presentation contract

`dashboard/lib/contracts/executionTopology.ts` exhaustively decodes the
application-owned `ExecutionTopologyViewV1` and
`ExecutionTopologyEventV1` generated DTOs. It does not redefine Plan 24,
Plan 32, Plan 36, or Plan 37 enums. The view pins one
`WorkProjectionSelection`, graph version, topology revision,
`RepositorySnapshot` digest, valid time, observation time, watermarks,
coverage, freshness, authorization, and canonical ordering policy. Its
payload contains:

- canonical work-item references and optional `worktree_lanes` and
  `branch_stack_lanes`; unsupported, unavailable, denied, partial, stale, or
  omitted lane families remain explicit and never disappear into the base
  Kanban;
- separate repository dirty state, native worktree lifecycle/lock state, Plan
  32 lease/authority state, task readiness, runtime state, and evidence health.
  `dirty`, `locked`, `leased`, `blocked`, and `conflicting` are never aliases;
- dependency-commit edges with exact object identity and coverage, and
  application-provided proposed/observed merge-order edges. Commit subjects,
  branch display names, and lane positions are labels only;
- mechanical conflict evidence from native Git intelligence and semantic
  conflict/proximity evidence from Plan 05/37 as independent dimensions with
  their own producer, score kind, calibration revision, coverage, freshness,
  omissions, and anchors. The heatmap never averages them or treats unknown as
  zero;
- integration proposal revisions, exact source/target repository snapshots,
  required dependency commits, predicted impact, required tests/checks,
  alternatives, expiry, evidence, and Plan 24 disposition; plus observed
  native-Git, Plan 32, test, and CI receipts with authority and coverage;
- immutable topology frames and cursors for valid-time/observation-time
  playback. Frames reference events and entities by stable ID and never
  interpolate repository history, invent causality, or replay an effect; and
- application-supplied `TopologyLegalActionV1` values. The action union is
  `RequestDryRun | RequestApply | RequestCancel | Inspect | ExpandEvidence |
  Refresh`; each mutating request carries operation/action ID, expected graph,
  work-item, repository-snapshot, runtime, lease-authority and policy versions
  as applicable, idempotency key, expiry, confirmation requirement, and safe
  reason schema.

The generated event union is exactly `SnapshotReplaced |
WorktreeStateChanged | BranchStackChanged | DependencyCommitChanged |
MergeOrderChanged | ConflictProximityChanged | IntegrationProposalChanged |
IntegrationOperationChanged | TestCheckChanged | TopologyFrameAppended`.
Every event carries stream/run identity, event and entity revision, scope,
observation time, source watermark, and coverage. `topologyEventReducer.ts`
deduplicates by stream/event/revision, rejects stale generations, retains
receipts already observed, and triggers one canonical refetch on a revision
gap. It never derives a branch stack, merge order, conflict result, readiness,
or legal action.

`TopologyLaneBoard` is a synchronized grouping of the canonical selection,
not another board. A work item has one stable selection identity even when
referenced by task, worktree, and stack lanes. Worktree and stack grouping can
be independently enabled only when their lane-family state is `available`;
their off state does not remove entities or change canonical totals.
`DependencyCommitRail` distinguishes required, present, missing, stale,
denied, and unknown commits. `MergeOrderRail` distinguishes proposed,
accepted-graph, observed-native, superseded, and unknown order; spatial order
never becomes an instruction.

`ConflictProximityHeatmap` exposes a synchronized accessible matrix with
separate mechanical and semantic columns, relationship paths, freshness,
coverage, omitted counts, and exact evidence expansion. Exact same-range or
symbol overlap remains distinct from configured-threshold proximity.
Denied/private cells expose neither hidden actor, root, address, count, nor
content. Partial or unknown mechanical coverage cannot render “clean merge”;
partial or unknown semantic coverage cannot render “no overlap.”

`ExecutionTopologyInspector` is rooted in the opaque Plan 24 `TaskId`
(`WorkItemId`) and exact `WorkItemVersionId`. Every lane, rail, heat cell,
proposal, event, receipt, test, and check pivots through that root while
preserving graph/topology/scope/time/watermark/anchor identity. Expansion
rechecks authorization and returns the normal available/redacted/locked/
unauthorized/denied/stale/revoked/expired/missing/corrupt/partial/error
states. A compact card, summary, truncated event tail, or stack alias never
substitutes for the lossless TaskId drill-down.

`IntegrationOperationDialog` renders only legal actions returned for the
selected exact version. `RequestDryRun` is always effect-free and returns a
new immutable preview or a typed stale/denied/locked/unsupported result.
`RequestApply` is present only where an owning application operation has
mutation authority: current V2 Plan 36 native-Git merge/rebase/cherry-pick
plans remain read-only and therefore never expose apply, while Plan 24 graph
proposal application and Plan 36's three allowed index/commit operations may
do so through their own contracts. `RequestCancel` requests cancellation from
the owning operation/runtime and does not predict whether the native commit
point was crossed. The dialog never optimistically changes a lane, dirty
state, ref, proposal, run, test, or CI result. Reload by operation ID resumes
preview, queued, applying, cancelling, committed, cancelled, partial,
effect-unknown, failed, or recovered receipt state without redispatch.

## Required behavior

- Brain: whole-system and scoped summaries, health, activity, relationships, freshness, and coverage.
- Explorer: pivotable search across messages, sessions, facts, code, projects, repositories, worktrees, and time with provenance visible.
- Loom: interactive temporal and causal traces linking prompts, reasoning, tools, subagents, code changes, branches, commits, PRs, and outcomes.
- Brain, Explorer, Loom, Code, and Work provide zoom, pan, search,
  filtering, brushing, linked selection, semantic clustering, and temporal
  playback within the interaction budgets below over the renderer-neutral view
  model. Stable deep links and scope
  survive overview → finding/entity → investigation → evidence/action
  progressive disclosure.
- Explorer includes a planner-query composer with validation and a plan
  explanation, parallel-source progress, elapsed time and cancellation, typed
  source outcomes, partial result pages, canonical finality, and evidence
  packets. Pending state appears before results; percentages appear only for a
  known denominator. The browser never invents a source, rank, merge, or
  why-this-result explanation.
- Sessions: transcript search, LCM summaries, raw-message drill-down, compaction boundaries, replay context, and provider identity.
- Agents: agent/subagent trees, status, model/provider, handoffs, tool activity, outputs, and failure context.
- Code: symbol search, references, call paths, diagnostics, affected tests, code health, and branch-aware graph freshness; diagnostics show canonical provenance, coverage, freshness, analyzer/gateway state, and conflicts from typed daemon APIs.
- Code replaces any headline universal `quality_signal` with independently
  named typed quantifiers: raw value/unit and numerator/denominator, descriptor
  revision, eligible/covered/unknown/excluded counts, cohort descriptor when
  valid, temporal delta, provenance, and evidence class
  (`measurement | association | calibrated_prediction`). It computes no
  dashboard-local health grade.
- Knowledge: facts, memories, evidence, contradictions, supersession, curation, and cross-project relationships.
- Delivery: changes, commits, branches, worktrees, pull requests, CI, releases, and typed PR17 workflow runs tied to product delivery.
- Automations: schedules, run history, artifacts, approvals, generated skills, memory curation, session reflection, and bounded controls.
- Observatory: hook hints, event flow, latency, failures, daemon health, storage health, queues, and product diagnostics, including canonical analyzer/gateway state, conflicts, coverage, and freshness.
- Costs: provider/model usage, tokens, latency, estimated cost, cache effects, and time/project/session breakdowns.
- Settings: effective layered configuration and application-supplied typed
  patch preview/validation/CAS operations, provider integration, privacy
  controls, retention, and feature controls; it never constructs an
  unvalidated free-form configuration mutation.
- Work (PR17): initiative and work-item views, Kanban, dependency DAG and
  critical path, timeline/history, causal, workload/executor/model, and
  repository/delivery projections over one canonical Plan 24 selection. Every
  card and inspector preserves exact scope/version/evidence, links Plan 32
  lease/attempt/effect history, and renders only application-provided legal
  actions. A lane move never sets readiness directly.
- Work execution topology (PR17): optionally groups that same selection into
  worktree and stacked-branch lanes while preserving task lanes; shows exact
  dependency commits, proposed versus observed merge order, dirty/worktree/
  lease truth, mechanical conflict and semantic proximity side by side,
  integration proposals and receipts, required/observed tests and CI, and
  dual-time playback. The canonical accessible table exposes every entity,
  edge, state, omission, and action available in the visual lane/rail/heatmap
  composition.
- Cross-worktree or cross-branch integration remains a proposal/observation
  journey: exact source/target snapshots → impact/conflict/test evidence →
  application-supplied dry run → explicit legal apply when an owner supports
  it → receipt → independent native/test/CI observation. Unsupported apply,
  stale preview, changed head/base/merge base, dirty target, conflicting
  lease, denied scope, unknown effect, partial checks, and cancelled operation
  are first-class outcomes. The browser never calls Git or CI directly.
- Work task-intelligence views (PR17): task-shape dimensions and calibrated
  ranges; parent/child decomposition comparison and review; ranked eligible
  routes with exclusions, confidence/coverage, requested/actual identity, and
  deterministic fallback; independent-review/outcome evidence; estimate-versus-
  outcome calibration and model-version drift; and live
  split/merge/resize/re-route proposals with explicit accept/reject/supersede
  actions. `Abstained`, `FallbackRecommended`, stale, expired, censored,
  unknown, non-independent review, and insufficient-coverage states are
  visible product states, never blank cards or hidden tooltips.
- Work exposes TaskId-rooted compact context, topology/partition alternatives,
  handoff, escalation, governed experience recall, route and attempt evidence,
  independent review, and exact anchored source expansion. Kanban is one
  projection; cards and summaries never substitute for canonical evidence.
- A proposal preview shows the old and proposed immutable graph versions,
  changed estimates/edges/scope, evidence anchors, expected runtime impact, and
  required separate Plan 32 control. The browser neither recomputes a grade nor
  applies a graph/runtime mutation optimistically.
- Auxiliary-attempt inspectors (PR17) separate the Plan 24 request from the
  Plan 32 lease/attempt. They show requested and actual
  provider/backend/executable/protocol/model/reasoning identity, negotiated
  capabilities and explicit fallback reason, exact worktree/parent lineage,
  sandbox/approval/capability class, bounded context coverage,
  progress/heartbeat and stream coverage, cancellation/kill stage, artifacts,
  resume/reconnect state, and typed terminal outcome. They never display raw
  argv/stdin, environment, secrets, or unredacted provider output.
- `Unsupported`, `Absent`, `Stale`, `Cancelled`, `TimedOut`, `Failed`,
  `Partial`, lost heartbeat, malformed stream, version drift, unknown
  termination, and resume unavailable are distinct visible states. A Claude
  route identifies native Claude Code; a Codex route distinguishes app-server
  from an explicitly approved CLI fallback.
- Work and Doctor views render the one
  canonical PR14 Doctor application finding family whose mandatory regression
  coverage is specified by
  [Plan 14](14-historical-failure-regression-matrix.md):
  Plan 20 desired/effective configuration, Plan 27 observed discovery/
  conformance and remediation references, Plan 32 lease/attempt runtime state,
  and Plan 26 coverage/measurements remain visibly attributable. The UI shows
  unsupported/absent/stale executable, protocol drift, invalid fallback,
  sandbox/capability mismatch, restart/resume failure, stuck lease/attempt, and
  provider availability with evidence, coverage, severity, and owner-specific
  legal actions. It does not infer health, merge findings by label, or invoke a
  private provider/Doctor probe.
- Recovery journeys preserve diagnosis, inert suggestion, canonical operation
  preview, explicit confirmation/human override, owner dispatch, receipt, and
  post-operation verification as separate states. The UI renders only
  application-supplied remediation references, submits a selected reference
  through the typed application API, and never derives remediation from
  diagnosis. Reload by operation ID resumes preview, dispatch, receipt, or
  verification without redispatch.
- Pinned Hermes dashboard and delegation UI evidence at `c48d53413aa2c`
  (`plugins/kanban/dashboard/plugin_api.py`,
  `ui-tui/src/components/agentsOverlay.tsx`) establishes useful visibility:
  familiar task lanes, task/run drawers, delegation trees, worker inspection,
  bounded output/event tail, termination, diagnostics, dispatch status, and
  live updates. The Work workspace provides those outcomes over canonical
  Plan 24/32 identities, plus explicit stale/partial/recovery state. Kanban is
  one synchronized projection; the delegation tree and attempt timeline are
  linked projections, not alternate task stores.
- Work inspectors expose applicable skill/hint/capability identities,
  provenance, availability, and whether each was delivered to the provider.
  They never render copied prompts, secret-bearing environment, profile
  strings as identity, or provider self-report as accepted completion.
- Every view preserves and displays active scope; cross-scope transitions are explicit.
- Severity/consequence and evidence quality are separate visual and semantic
  dimensions. Coverage, freshness, completeness, missingness,
  sampling/capping, provenance, and uncertainty never collapse into one
  red/amber/green signal.
- Data visualizations have accessible tabular or textual equivalents and keyboard operation.
- Loading, `complete_zero_findings`, ready, partial, stale, locked, denied,
  unauthorized, redacted, conflicting, offline, unknown, cancelled,
  timed-out, error, and unsupported-schema are distinct designed product
  states without color-only cues.
- Large results use server pagination or bounded virtualization; the client never loads an unbounded corpus.
- SSE updates invalidate or patch typed cached data without duplicating server business logic.
- Each product PR includes the UI, tests, and navigation needed to use its behavior; PR14 closes shared-shell and cross-workspace gaps.
- PR17 workflow UI uses typed forms and product operations for concrete workflows; it is not a general JS IDE or plan executor.

## Renderer-neutral interaction and fallback contract

The required default renderer is `defaultCanvasAdapter.ts` under the
repository's permissive license and works offline. `rendererAdapter.ts` has
only `mount`, `render`, `setViewport`, `setTransientSelection`, `focus`, and
`destroy`; callbacks emit stable-ID presentation intents. Semantic cluster
membership, labels, explanations, causal edges, rank, critical path, legal
actions, readiness, and coverage arrive in `ProjectionView`. An adapter may
position or visually bundle entities but may not create or persist those
semantics.

Filtering uses typed application descriptors. A local mask may preview
already-loaded visibility but cannot change authoritative counts, coverage,
saved selection, actions, or complete-zero status; server confirmation
replaces it. Brushing is a transient hit-test preview until committed as
stable IDs. Graph, accessible table, Kanban, DAG, timeline, causal, and
workload views resolve the same selection; missing entities remain labeled
`outside_projection`, `filtered`, `not_loaded`, `denied`, or `stale`.

Temporal playback keeps valid time and observation time separate, consumes
versioned frames/cursors, and supports pause, step, seek, speed, follow-live,
and return-to-live. The browser does not interpolate events into canonical
history or infer causality from proximity. Search, filter, linked selection,
cluster expansion, playback, evidence expansion, and lens changes preserve
scope and deep-link identity.
Execution-topology playback reuses this controller and
`topologyEventReducer.ts`. It can replay graph versions, worktree/stack
observations, dependency-commit and merge-order changes, conflict/proximity
evidence, leases/attempts, proposals, operation receipts, tests, and CI
observations. Playback controls are presentation-only: pausing or seeking does
not pause a run, cancel an operation, checkout a ref, or rerun a test.

Cosmograph may be evaluated only as an optional lazy-loaded adapter after
license and transitive-license review. It is never on the default critical
path, never required for a feature, ships no telemetry/remote assets/runtime
downloads, and passes CSP, offline, keyboard, accessible-table, bundle,
semantic parity, WebGL-loss, and memory-pressure gates. GPU picking maps only
to stable model IDs. Unsupported GPU, two context losses in 60 seconds,
initialization over two seconds, restoration failure over one second, or a
representative-tier frame p95 above 33 ms in each of five consecutive
one-second windows falls back within one second without losing URL, scope,
filters, selection, evidence, temporal frame, or legal actions.

## Responsive, accessibility, performance, and usability gates

WCAG 2.2 AA is mandatory. Automated tests cover 320×568, 390×844, 768×1024,
1024×768, 1280×720, and 1440×900 CSS pixels, 200% and 400% zoom,
`prefers-reduced-motion`, `prefers-contrast: more`, and forced colors. At 320
pixels and 400% zoom there is no page-level horizontal scroll, clipped truth
state, lost scope/provenance, or inaccessible action; labeled code/table/graph
regions may scroll internally. Touch targets are at least 44×44 CSS pixels.

Skip links and named landmarks are required. Graph/table widgets use one
active descendant: arrows move deterministically, Home/End move to bounds,
PageUp/PageDown move a viewport, Enter opens, Space selects, and Escape closes
and restores focus. Tab reaches toolbars rather than every graph node. Focus
survives SSE, pagination, virtualization, route/lens and renderer changes.
Canvas/WebGL is supplementary to a synchronized semantic node/relation/event
view; `role="application"` is forbidden. Reduced-motion starts playback paused
and replaces animated layout/zoom/pan with immediate changes. State,
selection, severity, evidence quality, uncertainty, and edge direction use
text/shape/border/pattern as well as color. Query and late-context live regions
coalesce routine updates and announce no more than once per second.

Server pages default to 100 and cap at 500. Virtualization starts above 200
rows, mounts at most 250 row-like elements plus one inspector, preserves
focused/selected entities, and always offers a nonvirtualized paginated mode
of at most 100 rows. Visible metadata includes total when known, loaded range,
sort/filter, cap/partial status, and next-page availability.

Deterministic graph tiers are small 1,000/2,000 nodes/edges, representative
10,000/25,000, large 50,000/150,000, and overflow 100,000/300,000. Raw
rendering above large is forbidden; overflow uses daemon-provided
clustering/slicing or semantic pagination. On a pinned Playwright Chromium
1.60.0 runner from `dashboard/package-lock.json` with 4 vCPU, 8 GiB, 4× CPU
throttling, 20/10 Mbit/s, and 40 ms RTT, startup uses five discarded warmups
then ten cold-cache runs and reports the median; interaction uses five
discarded warmups then 30 warm-cache runs and reports p95. Cold-cache runs
clear HTTP cache, service workers, IndexedDB, and browser storage; warm-cache
runs start from one settled representative projection. The artifact records
browser/build revision, machine profile, samples, median/p95, failures, and
threshold:

- initial shell HTML, critical CSS, and executable JavaScript ≤250 KiB Brotli;
  initial workspace chunk ≤200 KiB; initial API response ≤256 KiB compressed
  and 1 MiB decoded; initial graph payload ≤1 MiB compressed and 4 MiB decoded;
- LCP and keyboard-ready ≤2.5 s, CLS ≤0.1, pending acknowledgement ≤100 ms,
  loaded selection/filter/brush ≤150 ms, linked selection ≤200 ms, and cached
  evidence inspector ≤200 ms;
- the representative execution-topology tier is exactly 500 visible work
  items, 64 worktree/stack lanes, 2,000 dependency/merge-order edges, and
  4,096 conflict cells. Its initial payload is ≤512 KiB compressed and ≤2 MiB
  decoded; larger selections use server grouping/paging. Lane/rail selection,
  keyboard movement, and one playback step are ≤150 ms p95; cross-lens linked
  selection and TaskId inspector open are ≤200 ms p95; return-to-live after a
  settled refetch is ≤500 ms p95. At most 250 row-like DOM elements, 64 lane
  headers, 512 edge hit targets, and 256 heat cells are mounted at once;
  accessible pagination exposes all remaining rows without canvas-only data;
- representative graph frame ≤33 ms p95, large frame ≤50 ms p95, no long task
  >200 ms, and tasks >50 ms total ≤500 ms during a ten-second journey;
- first planner progress ≤500 ms, stage/elapsed/cancel/coverage visible after
  500 ms, progress at least once per second, and first local-daemon result page
  ≤1 s p95 or remains explicitly pending/partial;
- representative heap ≤256 MiB and large heap ≤512 MiB; after forced GC the
  settled first representative frame is the retention baseline, and ten
  workspace cycles or a drained 30-minute SSE run retain no more than 32 MiB
  or 10% of that baseline, whichever is smaller;
- sustain 100 SSE events/s for ten minutes and 1,000/s for ten seconds,
  coalesce to at most ten renders/s/view, keep input ≤200 ms p95, and bound the
  queue to 5,000 events or 10 MiB. Overflow marks the projection stale and
  performs one canonical invalidation/refetch.

Usability acceptance uses exactly 12 participants: at least four keyboard-
only users, three screen-reader users, three users who work at 200–400% zoom
or high contrast, and two regular dashboard/IDE users; cohorts may overlap but
all four cohorts must be represented. Participants must not have implemented
the tested slice. Tasks cover scope identification,
complete-zero versus partial/stale/unknown, exact evidence, graph/table parity,
keyboard query/filter/brush/expansion, truthful query delay and cancellation,
supplied remediation through verified recovery, handoff resume, uncertainty,
unavailable actions, topology lane/table/TaskId parity, mechanical versus
semantic conflict disagreement, stale integration preview, operation-receipt
resume, and valid-time versus observation-time playback. There are zero
wrong-scope, hidden-state, illegal-action, browser-owned Git/CI, or
dispatch-as-recovery outcomes; at least 11/12 complete scope,
evidence, parity, recovery, and action-authority tasks unassisted and 10/12
complete every other task; every screen-reader participant completes the
graph-equivalent task; median Single Ease Question is ≥6/7 and SUS is ≥80.

## Delivery milestones and test assets

1. **PR14 Gate A — contract freeze:** generated DTOs/decoders, route manifest, domain
   state matrix, projection manifest, and sanitized fixtures.
2. **PR14 Gate B — shell:** scope-preserving responsive navigation, deep links,
   state boundary, inspector, and monotone cache/SSE reducer.
3. **PR14 Gate C — flagship exploration:** Brain, Explorer planner query, Loom,
   linked selection, clustering, playback, evidence packets, and late context.
4. **PR14 Gate D — diagnosis and recovery:** Code diagnostics, Observatory, Doctor,
   and resumable preview → confirmation → dispatch → receipt → verification.
5. **PR14 Gate E — workspace completion:** Sessions, Agents, Knowledge, Delivery,
   Automations, Costs, and Settings with cross-workspace evidence links.
6. **PR14 Gate F — hardening:** responsive/a11y/manual assistive-technology records,
   virtualization, renderer parity/fallback, budgets, SSE churn, and usability.
7. **PR17 Gate A — Work projections:** one Plan 24 selection across Kanban, DAG,
   timeline, causal, workload, repository, delegation, and attempt lenses.
8. **PR17 Gate B — Work intelligence/runtime:** proposal diffs, route/exclusion
   evidence, requested/actual identity, attempts, receipts, and recovery.
9. **PR17 Gate C — execution topology:** worktree/stack lane-family states,
   dependency commits, merge order, dirty/lease truth, conflict/proximity,
   tests/CI, TaskId drill-down, and dual-time playback over the same selection.
10. **PR17 Gate D — governed integration controls:** dry-run/apply/cancel
    request rendering, stale/denied/locked/unsupported/effect-unknown outcomes,
    crash-safe receipt resume, authority-negative tests, topology performance,
    and Plan 26 metric parity.

Fixtures live under `dashboard/test/fixtures/` and include
`dashboard-state-taxonomy.json`, `planner-parallel-source-progress.json`,
`evidence-packet-matrix.json`, `evidence-expansion-states.json`,
`late-context.ndjson`, `deep-link-state-matrix.json`,
`projection-parity.json`, `renderer-fallback.json`,
`work-projection-matrix.json`, `auxiliary-attempt-matrix.json`,
`execution-topology-matrix.json`, `execution-topology-events.ndjson`,
`integration-operation-matrix.json`,
`doctor-source-disagreements.json`, `github-feedback-matrix.json`,
`dashboard/test/fixtures/generators/graphScale.mjs`, and
`dashboard/test/fixtures/generators/sseChurn.mjs`. Plan 14 defines their exact
fixture IDs and assertions; these names are the one canonical manifest.
Generated load bytes are never a product authority.

Vitest + Testing Library own contract/DOM tests, MSW owns HTTP/SSE fault
injection, Playwright Chromium/Firefox/WebKit owns keyboard/responsive/smoke
and ARIA snapshots, `@axe-core/playwright` owns automated WCAG checks,
Lighthouse CI owns startup/bundle gates, and CDP Performance/Tracing/
HeapProfiler owns frame/long-task/memory gates. Manual NVDA/Firefox and
VoiceOver/Safari records are required; axe and screenshots cannot substitute
for semantic assertions or assistive-technology completion.

PR14 retains `build`, `test:node`, `test:dom`, `smoke`, and `smoke:mobile`;
adds exact dev dependencies `msw`, `@playwright/test`,
`@axe-core/playwright`, and `@lhci/cli`; and adds
`dashboard/playwright.config.ts`, `dashboard/lighthouserc.cjs`,
`dashboard/test/msw/handlers.ts`, `dashboard/test/perf/reference-profile.json`,
`dashboard/test/perf/dashboard.perf.spec.ts`,
`dashboard/test/perf/runner.mjs`, `dashboard/test/acceptance.mjs`,
`dashboard/test/manual/nvda-firefox-protocol.md`,
`dashboard/test/manual/voiceover-safari-protocol.md`,
`dashboard/test/manual/result.schema.json`,
`dashboard/test/manual/results/nvda-firefox-pr14.json`,
`dashboard/test/manual/results/voiceover-safari-pr14.json`,
`dashboard/test/usability/protocol.md`,
`dashboard/test/usability/result.schema.json`, and
`dashboard/test/usability/results/pr14.json`. PR17 adds
`dashboard/test/execution-topology-matrix.vitest.tsx`,
`dashboard/test/execution-topology-actions.vitest.tsx`,
`dashboard/test/execution-topology-playback.vitest.tsx`, and
`dashboard/test/e2e/execution-topology.spec.ts`.
`test:acceptance` executes, in
order, build, test:node, the narrowed legacy test:dom suite, contracts, a11y,
responsive, renderer parity, authority-negative, work-topology, performance,
Lighthouse, SSE, e2e, smoke, and smoke:mobile, then validates both manual
records and the usability results against their schemas and thresholds. The
Playwright configuration includes an exact `work-topology` project on
Chromium, Firefox, and WebKit with the responsive and keyboard matrices above.
Test and measurement
scripts fail when zero cases or samples execute and print fixture/state/sample
counts; build and Cargo check fail on compile or validation error.

The implementation adds these exact script bodies to
`dashboard/package.json`:

```json
{
  "test:dom": "vitest run test/code-graph-explorer-hooks.vitest.tsx test/curation-data.vitest.tsx test/curation-panel.vitest.tsx test/pending-automation-counts.vitest.tsx test/semantic-map-interactions.vitest.ts test/settings-panel-patches.vitest.ts test/settings-panel.vitest.tsx",
  "test:contracts": "vitest run test/dashboard-contracts.vitest.ts test/dashboard-state-matrix.vitest.tsx test/workspace-route-contracts.vitest.tsx",
  "test:a11y": "playwright test --config=playwright.config.ts --project=a11y",
  "test:responsive": "playwright test --config=playwright.config.ts --project=responsive",
  "test:renderer-parity": "vitest run test/projection-parity.vitest.tsx && playwright test --config=playwright.config.ts --project=renderer",
  "test:authority-negative": "vitest run test/authority-negative.vitest.ts",
  "test:work-topology": "vitest run test/execution-topology-matrix.vitest.tsx test/execution-topology-actions.vitest.tsx test/execution-topology-playback.vitest.tsx && playwright test --config=playwright.config.ts --project=work-topology",
  "test:perf": "node test/perf/runner.mjs",
  "test:lighthouse": "lhci autorun --config=lighthouserc.cjs",
  "test:sse": "vitest run test/monotone-events.vitest.ts test/late-context.vitest.tsx && node test/perf/runner.mjs --suite=sse",
  "test:e2e": "playwright test --config=playwright.config.ts --project=chromium --project=firefox --project=webkit",
  "test:acceptance": "node test/acceptance.mjs"
}
```

The individual scripts above are focused developer entrypoints.
`test:acceptance` is the sole aggregate frontend invocation and does not invoke
itself. The release gate is exactly:

```bash
npm --prefix dashboard run test:acceptance
cargo test --all-features --test dashboard_api_test
cargo check --all-features
```

## Acceptance

- The original twelve named workspaces are complete, navigable, responsive,
  and accessible by PR14; Work meets the same bar in PR17.
- Cross-links preserve scope and provenance across all twelve PR14 workspaces
  and PR17 Work.
- Unit, DOM, accessibility, and smoke tests cover critical journeys and all state classes.
- Performance tests bound initial payloads, long lists, graph rendering, and live-update churn.
- `npm --prefix dashboard run test:acceptance` executes contract, DOM,
  accessibility, responsive, renderer/semantic parity, authority-negative,
  performance, SSE, and end-to-end gates and reports fixture/state counts.
- Representative graph-size acceptance measures interaction latency for zoom,
  pan, filter, search, brushing, linked selection, clustering, playback, and
  evidence expansion. Renderer parity compares semantic selection, scope,
  coverage, anchors, state, and keyboard behavior rather than pixels.
- Task-based usability and accessibility tests cover retaining scope across
  progressive disclosure, distinguishing complete-zero from partial, tracing a
  finding to exact evidence, resuming a handoff, understanding uncertainty,
  applying and overriding only legal actions, and distinguishing dispatch from
  verified recovery.
- PR17 DOM/accessibility/parity fixtures cover decomposition review, routing
  explanation, fallback/abstention, independent-review status, calibration,
  exact model-version drift, censored/unknown outcomes, stale live proposals,
  and explicit human override without browser-local scoring.
- PR17 auxiliary-attempt fixtures cover provider negotiation, request versus
  attempt lineage, progress/stream truncation, explicit fallback,
  cancellation escalation, restart/resume, artifacts, and all terminal states
  without browser-local process execution, output parsing, provider selection,
  or graph/runtime mutation.
- PR17 execution-topology fixtures cover optional/unsupported worktree and
  stack lanes, exact dependency commits, proposed versus observed merge order,
  every dirty/worktree/lease state, mechanical versus semantic conflict
  disagreement, required/observed tests and CI, drift/retarget, concurrent
  edit proximity, crash/restart receipt recovery, branch retention, and
  dual-time playback without browser-local Git, scheduler, test, or CI logic.
- Every visual topology reference round-trips through the same TaskId,
  work-item/plan/graph/topology versions, exact repository/worktree/branch
  snapshot, valid/observation time, watermarks, and anchors as its accessible
  row and inspector. Missing, stale, partial, denied, locked, redacted, and
  unsupported data remains visible and cannot fall back to path, lane, branch
  label, current checkout, or latest graph version.
- Authority-negative tests prove `RequestDryRun`, `RequestApply`, and
  `RequestCancel` are submitted only from application-supplied action
  references with exact expected versions and idempotency identity; duplicate
  clicks return one receipt, stale previews cannot apply, cancellation never
  rewrites a committed receipt, and an unsupported Git merge/rebase/
  cherry-pick operation never gains an apply control.
- PR17 dashboard fixtures render each canonical auxiliary-provider finding and
  cross-owner disagreement from Plan 14, preserve Plan 20 desired/observed
  revisions and Plan 27/32/26 provenance, and invoke only the supplied typed
  remediation reference. No component-local health formula, implicit config
  write, host repair, lease reclaim, or attempt cancellation is allowed.
- Pinned-Hermes-derived DOM/accessibility fixtures cover familiar derived
  lanes, task/run/delegation drill-down, event tail, diagnostics,
  capacity/blocker reasons, terminal protocol violation, termination,
  skills/hints discoverability, and deterministic restart/recovery without
  browser-owned card status, PID claim, profile routing, or business logic.
- No independent Kanban/task store, developer-plan executor, orchestration lab,
  workflow JavaScript, generated inventory, browser-side model scoring, or
  backend policy duplication remains.
- AST/import-boundary tests fail if renderer or Kanban code computes identity,
  rank, semantic clusters, causality, readiness, critical path, health,
  severity, coverage, routes, legal actions, or remediation; persists a board,
  task/runtime state, expanded evidence, or adapter layout; treats lane/order/
  process exit/heartbeat/provider output as canonical state; silently rebases a
  stale link/proposal; or changes semantics/actions between visual and
  table/text renderers.
