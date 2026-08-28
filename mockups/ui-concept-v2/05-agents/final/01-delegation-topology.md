---
design_status: current
evidence_class: concept_synthetic
---

# Agents: delegation topology

- **Asset:** `01-delegation-topology.png`
- **Lifecycle:** `current`

## User job

Understand who delegated work to whom, which agents and subagents are active, ended, failed, or ambiguous, what each branch produced, and where handoffs, tasks, Sessions, code, and Deliveries connect—without reducing a large collaboration to a flat agent list.

## Product behavior

- The central topology lays generations left to right. Parent-to-child edges require an evidenced spawn/delegation relation; handoff and rejoin use distinct glyphs and source records.
- Node size may encode a named measured quantity such as events in the selected window. Color, shape, labels, and line style jointly encode activity, failure, selection, and evidence grade.
- Selecting a node or edge opens its stable agent/session identity, delegator, project scope, task/work product, handoff token frontier, status/failure context, and exact cross-links.
- The event window and category/tool summaries explain what counts are in scope. Capped, paginated, partial, stale, and missing inputs remain visible rather than appearing complete.
- Cross-links preserve context into Sessions, Loom, Work, Code, and Delivery. A handoff never implies that the child authored every downstream commit or result.

## Scale and navigation

- At 100+ agents, the overview groups branches deterministically by task, workstream, repository, or outcome. Bundles show unique-agent and subagent counts, event volume, unresolved failures, and evidence gaps with unambiguous, reconcilable semantics.
- Semantic zoom progresses from outcome/workstream bundles to agent groups, individual agents, and exact events. It never allocates one permanent visible lane or DOM row per agent at overview scale.
- A virtualized branch navigator provides search, filters, keyboard traversal, pinned branches, unresolved/high-risk modes, and path-to-root/path-to-outcome focus.
- Focus-plus-context expands the selected causal neighborhood while unrelated branches compress into bundles; a minimap and exact tree/table fallback preserve global orientation.

## Production authorities

- `dashboard/src/workspaces/agents/AgentsPage.tsx` owns the workspace and decodes independently typed analytics payloads.
- `SubagentTree.tsx`, `AgentHandoffs.tsx`, `AgentHandoffTokens.tsx`, and `AgentFailureContext.tsx` own hierarchy, handoff, token, and failure views; no one response makes the others ready.
- Sessions and durable agent/delegation records own participant identity and parent/child relations. Work owns tasks and work products. Git/Code and Delivery own commits, symbols, and PR outcomes.
- The deterministic topology layout may render through the shared scene runtime, but React/DOM remains the authority for navigation, inspector text, exact tables/trees, keyboard controls, and accessibility.

## Evidence and truth states

- `EXACT`: stable agent/session identity, recorded spawn, handoff token, task identity, work product, commit, or source event from its owner.
- `EXPLICIT`: a persisted delegation, status report, result, or decision attributed to its visible artifact.
- `INFERRED`: a causal, authorship, handoff, or outcome link derived from a named correlation basis.
- `AMBIGUOUS`: multiple parent, agent, task, or work-product candidates remain unresolved.
- `STALE`: usage, diagnostics, token, or failure data exceeded its freshness boundary.
- `UNAVAILABLE`: source authority is missing, denied, private, unsupported, failed, or not ingested; the topology leaves an honest gap.

## Acceptance gates

- Keyboard traversal covers topology, bundle expansion, branch search/filter, node and edge selection, inspector, and cross-page pivots with visible focus.
- Reduced motion replaces path travel and animated expansion with stable before/after states while preserving generation and handoff meaning.
- At 200% browser zoom, the topology enters a focus mode and text regions reflow; labels, controls, and evidence are not canvas-scaled into illegibility.
- Exact virtualized tree/table, session list, event log, handoff ledger, and work-product fallback preserve selection and evidence grades.
- Dense-real-data tests cover 100+ unique agents, nested subagents, missing parents, cross-workstream participation, cycles/refusals, partial pages, stale diagnostics, and independently failed authorities.

## Truth boundary

This is a **CONCEPT / SYNTHETIC DATA** plate. Agent names, event counts, hierarchy, failures, tokens, and products are illustrative. Production topology may display only relations and states supplied by the real typed authorities; it must not invent delegation, authorship, activity, or success.
