# TraceDecay V2 Dashboard Frontend

## Status / Role

Normative product plan. Every product PR ships its usable UI slice with its backend behavior. PR14 completes the shared shell and the full Brain, Explorer, Loom, Sessions, Agents, Code, Knowledge, Delivery, Automations, Observatory, Costs, and Settings experience. PR17 adds the first-class Work workspace and task-graph projections owned semantically by [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md).

## Outcome

The dashboard presents TraceDecay as one connected brain across projects while preserving precise repository, worktree, branch, session, agent, and time scope.

## Owns

- Navigation, responsive layout, accessibility, interaction state, and client-side presentation.
- Typed API consumption, query caching, optimistic UI only where safe, and SSE-driven refresh.
- Linked visual exploration across product data and provenance.
- User-facing configuration, diagnostics, recovery guidance, and operation progress.

## Does not own

- Business rules, authorization decisions, storage, indexing, migration, or repair execution.
- The frontend never starts analyzers, opens LSP connections, merges
  diagnostics, or infers health; it only consumes typed daemon APIs defined by
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- An independent Kanban database, developer plan parser/executor, browser-side
  task scheduler, generic orchestration lab, or edit-bundle editor. PR17's
  Kanban/DAG/timeline/causal/workload views are projections over Plan 24
  application state and Plan 32 runtime receipts.
- Arbitrary JavaScript workflow authoring or execution.
- Generated compatibility views, route inventories, or a second model of backend behavior.

## Required behavior

- Brain: whole-system and scoped summaries, health, activity, relationships, freshness, and coverage.
- Explorer: pivotable search across messages, sessions, facts, code, projects, repositories, worktrees, and time with provenance visible.
- Loom: interactive temporal and causal traces linking prompts, reasoning, tools, subagents, code changes, branches, commits, PRs, and outcomes.
- Sessions: transcript search, LCM summaries, raw-message drill-down, compaction boundaries, replay context, and provider identity.
- Agents: agent/subagent trees, status, model/provider, handoffs, tool activity, outputs, and failure context.
- Code: symbol search, references, call paths, diagnostics, affected tests, code health, and branch-aware graph freshness; diagnostics show canonical provenance, coverage, freshness, analyzer/gateway state, and conflicts from typed daemon APIs.
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
- Work and Doctor consume the one Plan 14 auxiliary-provider health finding:
  Plan 20 desired/effective configuration, Plan 27 observed discovery/
  conformance and remediation references, Plan 32 lease/attempt runtime state,
  and Plan 26 coverage/measurements remain visibly attributable. The UI shows
  unsupported/absent/stale executable, protocol drift, invalid fallback,
  sandbox/capability mismatch, restart/resume failure, stuck lease/attempt, and
  provider availability with evidence, coverage, severity, and owner-specific
  legal actions. It does not infer health, merge findings by label, or invoke a
  private provider/Doctor probe.
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
- Data visualizations have accessible tabular or textual equivalents and keyboard operation.
- Loading, empty, partial, stale, offline, unauthorized, and error states are designed product states.
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
- PR17 DOM/accessibility/parity fixtures cover decomposition review, routing
  explanation, fallback/abstention, independent-review status, calibration,
  exact model-version drift, censored/unknown outcomes, stale live proposals,
  and explicit human override without browser-local scoring.
- PR17 auxiliary-attempt fixtures cover provider negotiation, request versus
  attempt lineage, progress/stream truncation, explicit fallback,
  cancellation escalation, restart/resume, artifacts, and all terminal states
  without browser-local process execution, output parsing, provider selection,
  or graph/runtime mutation.
- PR17 Doctor/UI fixtures render each canonical auxiliary-provider finding and
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
