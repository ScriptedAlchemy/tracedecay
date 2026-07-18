# TraceDecay V2 Dashboard Frontend

## Status / Role

Normative product plan. Every product PR ships its usable UI slice with its backend behavior. PR14 completes the shared shell and the full Brain, Explorer, Loom, Sessions, Agents, Code, Knowledge, Delivery, Automations, Observatory, Costs, and Settings experience. PR17 adds the first-class Work workspace and task-graph projections owned semantically by [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md).

## Outcome

The dashboard is TraceDecay's world-class flagship product surface: a polished,
highly interactive connected brain across projects that preserves precise
repository, worktree, branch, session, agent, time, provenance, coverage, and
authority scope. Visual quality serves comprehension rather than novelty.

## Owns

- Navigation, responsive layout, accessibility, interaction state, and client-side presentation.
- Typed API consumption, query caching, optimistic UI only where safe, and SSE-driven refresh.
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
  remediation orchestration. The sole kernel for those concerns is
  [Plan 14](14-historical-failure-regression-matrix.md); the dashboard only
  renders its findings and legal actions.
- An independent Kanban database, developer plan parser/executor, browser-side
  task scheduler, generic orchestration lab, or edit-bundle editor. PR17's
  Kanban/DAG/timeline/causal/workload views are projections over Plan 24
  application state and Plan 32 runtime receipts.
- Arbitrary JavaScript workflow authoring or execution.
- Generated compatibility views, route inventories, or a second model of backend behavior.
- Graph/query/storage authority, renderer-local ranking, health, readiness,
  scheduling, repair, or model-route calculations. Ship a permissive default
  renderer; Cosmograph or another commercial/GPU adapter is optional only
  after license, bundle, capability, offline, accessibility, fallback, and
  performance review.

## Required behavior

- Brain: whole-system and scoped summaries, health, activity, relationships, freshness, and coverage.
- Explorer: pivotable search across messages, sessions, facts, code, projects, repositories, worktrees, and time with provenance visible.
- Loom: interactive temporal and causal traces linking prompts, reasoning, tools, subagents, code changes, branches, commits, PRs, and outcomes.
- Brain, Explorer, Loom, Code, and Work provide smooth zoom, pan, search,
  filtering, brushing, linked selection, semantic clustering, and temporal
  playback over the renderer-neutral view model. Stable deep links and scope
  survive overview → finding/entity → investigation → evidence/action
  progressive disclosure.
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
- Settings: effective layered configuration, safe edits, validation, provider integration, privacy controls, retention, and feature controls.
- Work (PR17): initiative and work-item views, Kanban, dependency DAG and
  critical path, timeline/history, causal, workload/executor/model, and
  repository/delivery projections over one canonical Plan 24 selection. Every
  card and inspector preserves exact scope/version/evidence, links Plan 32
  lease/attempt/effect history, and renders only application-provided legal
  actions. A lane move never sets readiness directly.
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
  [Plan 14](14-historical-failure-regression-matrix.md) canonical
  auxiliary-provider finding family:
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
  Plan 14-supplied remediation references, submits a selected reference through
  the typed application API, and never derives remediation from diagnosis.
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
- Loading, healthy-empty, partial, stale, denied, locked, offline,
  unauthorized, conflicting, unknown, and error states are distinct designed
  product states without color-only cues.
- Large results use server pagination or bounded virtualization; the client never loads an unbounded corpus.
- SSE updates invalidate or patch typed cached data without duplicating server business logic.
- Each product PR includes the UI, tests, and navigation needed to use its behavior; PR14 closes shared-shell and cross-workspace gaps.
- PR17 workflow UI uses typed forms and product operations for concrete workflows; it is not a general JS IDE or plan executor.

## Acceptance

- The original twelve named workspaces are complete, navigable, responsive,
  and accessible by PR14; Work meets the same bar in PR17.
- Cross-links preserve scope and provenance across Brain, Explorer, Loom, Sessions, Agents, Code, and delivery artifacts.
- Unit, DOM, accessibility, and smoke tests cover critical journeys and all state classes.
- Performance tests bound initial payloads, long lists, graph rendering, and live-update churn.
- Representative graph-size acceptance measures interaction latency for zoom,
  pan, filter, search, brushing, linked selection, clustering, playback, and
  evidence expansion. Renderer parity compares semantic selection, scope,
  coverage, anchors, state, and keyboard behavior rather than pixels.
- Task-based usability and accessibility tests cover retaining scope across
  progressive disclosure, distinguishing clean-empty from partial, tracing a
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
