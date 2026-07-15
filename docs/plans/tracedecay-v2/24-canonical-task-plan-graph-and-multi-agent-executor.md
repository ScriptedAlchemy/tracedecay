# Retired Design: Canonical Task/Plan Graph and Multi-Agent Executor

**Status:** retired and removed

**Delivery:** none
**Replacement owners:** [16 scope](16-cross-project-repository-worktree-scope.md), [22 Context Scout](22-incremental-context-scout-and-suggestion-envelopes.md), [23 session/LCM retrieval](23-session-lcm-temporal-retrieval-and-evaluation.md), [32 workflows](32-dynamic-workflow-runtime-and-sdk.md), and [17 public API/SDK](17-official-public-api-and-sdks.md).

## Decision

TraceDecay V2 will not build the task-plan execution system previously described here. The design turned developer planning documents into a second product and delayed delivery of the actual storage, retrieval, context, workflow, and user-interface work.

The removal is final, not a deferred phase.

## Removed concepts

- Markdown or YAML plan parsing as runtime input;
- completion ledgers, plan trackers, next-ready gates, and PR-series executors;
- canonical initiative, versioned plan, or task databases;
- task journals, task projectors, task query algebra, saved task boards, and task APIs;
- decomposition, readiness, routing, fairness, retry, and critical-path policy;
- task offers, claims, leases, fence epochs, attempts, heartbeats, and executor adapters;
- generated task CLI/MCP/HTTP/SDK bindings;
- Kanban, DAG, workload, executor, plan-editing, and Orchestration Lab interfaces;
- frontmatter edit bundles, sharded plan workspaces, semantic plan diff/rebase/submit, and cleanup workers;
- compatibility import, shadow task scheduler, task cutover, and task-system deletion PRs.

No later plan may recreate these concepts under a different name or treat this file as an implementation dependency.

## Product ownership after retirement

- Plan 16 owns repository, project, checkout, worktree, ref, snapshot, and branch identity plus safe cross-scope resolution.
- Plan 22 owns bounded advisory nearby-agent and overlapping-work suggestions. These are evidence, never scheduling authority.
- Plan 23 owns retrieval of agent, session, Turn, handoff, and historical work evidence with exact anchors and temporal semantics.
- Plan 32 owns real user-visible typed workflow definitions, runs, nodes, state transitions, cancellation, recovery, and execution history.
- Plan 17 exposes Plan 32 workflows through the supported public API and SDK contracts.
- Existing capture and projector plans retain provider-native agent, tool, Git, and worktree observations needed by Plans 16, 22, 23, and 32.

Observed worktree or agent activity never creates a task, lease, executor route, cleanup authority, or plan state. Destructive worktree operations remain separately authorized system behavior; they are not recovered from this design.

## Developer workflow boundary

Codex, Claude, and other development agents may still decompose work, use subagents, maintain conversational plans, and coordinate through their native facilities. That is a development practice, not a TraceDecay runtime plan parser or canonical task authority.

Markdown plan files remain human documentation only. Tests may check links and formatting, but no parser interprets their prose as executable work.

## Cleanup requirements

- Plans 21–23 and all later plans must not depend on Plan 24 identities, stores, policies, surfaces, or PR slices.
- CLI/MCP work must not expose task-graph or edit-bundle bindings.
- Scout and LCM must consume ordinary observed evidence without task packets or global-board filtering.
- Plan 32 must define its own typed workflow identity and lifecycle without compiling or importing this retired task graph.
- Documentation must direct workflow product requirements to Plan 32/PR 17 and observational context requirements to Plans 16, 22, and 23.

## Done

This tombstone is complete when the plan set contains no live requirement for the removed parser, tracker, executor, task graph, Kanban, leases, or edit bundles, and every retained product requirement is owned by the replacement plans above.
