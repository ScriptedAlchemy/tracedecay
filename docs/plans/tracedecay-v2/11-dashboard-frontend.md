# TraceDecay V2 Dashboard Frontend

## Status / Role

Normative product plan. Every product PR ships its usable UI slice with its backend behavior. PR14 completes the shared shell and the full Brain, Explorer, Loom, Sessions, Agents, Code, Knowledge, Delivery, Automations, Observatory, Costs, and Settings experience.

## Outcome

The dashboard presents TraceDecay as one connected brain across projects while preserving precise repository, worktree, branch, session, agent, and time scope.

## Owns

- Navigation, responsive layout, accessibility, interaction state, and client-side presentation.
- Typed API consumption, query caching, optimistic UI only where safe, and SSE-driven refresh.
- Linked visual exploration across product data and provenance.
- User-facing configuration, diagnostics, recovery guidance, and operation progress.

## Does not own

- Business rules, authorization decisions, storage, indexing, migration, or repair execution.
- A Kanban board, developer plan parser, task executor, scheduler console, orchestration lab, or edit-bundle editor.
- Arbitrary JavaScript workflow authoring or execution.
- Generated compatibility views, route inventories, or a second model of backend behavior.

## Required behavior

- Brain: whole-system and scoped summaries, health, activity, relationships, freshness, and coverage.
- Explorer: pivotable search across messages, sessions, facts, code, projects, repositories, worktrees, and time with provenance visible.
- Loom: interactive temporal and causal traces linking prompts, reasoning, tools, subagents, code changes, branches, commits, PRs, and outcomes.
- Sessions: transcript search, LCM summaries, raw-message drill-down, compaction boundaries, replay context, and provider identity.
- Agents: agent/subagent trees, status, model/provider, handoffs, tool activity, outputs, and failure context.
- Code: symbol search, references, call paths, diagnostics, affected tests, code health, and branch-aware graph freshness.
- Knowledge: facts, memories, evidence, contradictions, supersession, curation, and cross-project relationships.
- Delivery: changes, commits, branches, worktrees, pull requests, CI, releases, and typed PR17 workflow runs tied to product delivery.
- Automations: schedules, run history, artifacts, approvals, generated skills, memory curation, session reflection, and bounded controls.
- Observatory: hook hints, event flow, latency, failures, daemon health, storage health, queues, and product diagnostics.
- Costs: provider/model usage, tokens, latency, estimated cost, cache effects, and time/project/session breakdowns.
- Settings: effective layered configuration, safe edits, validation, provider integration, privacy controls, retention, and feature controls.
- Every view preserves and displays active scope; cross-scope transitions are explicit.
- Data visualizations have accessible tabular or textual equivalents and keyboard operation.
- Loading, empty, partial, stale, offline, unauthorized, and error states are designed product states.
- Large results use server pagination or bounded virtualization; the client never loads an unbounded corpus.
- SSE updates invalidate or patch typed cached data without duplicating server business logic.
- Each product PR includes the UI, tests, and navigation needed to use its behavior; PR14 closes shared-shell and cross-workspace gaps.
- PR17 workflow UI uses typed forms and product operations for concrete workflows; it is not a general JS IDE or plan executor.

## Acceptance

- All twelve named workspaces are complete, navigable, responsive, and accessible by PR14.
- Cross-links preserve scope and provenance across Brain, Explorer, Loom, Sessions, Agents, Code, and delivery artifacts.
- Unit, DOM, accessibility, and smoke tests cover critical journeys and all state classes.
- Performance tests bound initial payloads, long lists, graph rendering, and live-update churn.
- No Kanban/plan executor, orchestration lab, workflow JavaScript, generated inventory, or backend policy duplication remains.
