---
design_status: current
---

# Loom final state set

These seven plates are the authoritative Loom concept sequence. Together they define a horizontal, temporal execution weave for following recorded or currently loaded work, replaying it, expanding agent branches, inspecting exact evidence, and attaching local feedback.

All pictured data is **CONCEPT / SYNTHETIC DATA**. The plates specify product behavior and information hierarchy; they are not proof that the production dashboard currently serves the pictured data.

## State sequence

| State | Image | Brief | User outcome |
|---|---|---|---|
| 01 | [Follow loaded tail](01-follow-loaded-tail.png) | [Product brief](01-follow-loaded-tail.md) | Orient to the loaded page and follow newly loaded execution without claiming a global live tail. |
| 02 | [Temporal replay](02-temporal-replay.png) | [Product brief](02-temporal-replay.md) | Scrub or play the execution story while unrevealed future events remain hidden. |
| 03 | [Branching execution](03-branching-execution.png) | [Product brief](03-branching-execution.md) | Understand agent/subagent spawn, parallel work, handoff, and rejoin. |
| 04 | [Dense 100+ agents](04-dense-100-agents.png) | [Product brief](04-dense-100-agents.md) | Navigate large fan-out through workstream bundles and semantic zoom. |
| 05 | [Selected event evidence](05-selected-event-evidence.png) | [Product brief](05-selected-event-evidence.md) | Inspect exact hook, transcript, task, code, and causal-neighborhood evidence in a roomy workspace. |
| 06 | [Feedback continuation](06-feedback-continuation.png) | [Product brief](06-feedback-continuation.md) | Attach local TraceDecay feedback and see later work that acknowledges, acts on, or contradicts it. |
| 07 | [Evidence gaps](07-evidence-gaps.png) | [Product brief](07-evidence-gaps.md) | Distinguish ambiguity, staleness, missing data, and unavailable private reasoning without invention. |

## Shared interaction contract

- Time maps left to right. Pan, wheel/trackpad, scrub, keyboard seek, minimap navigation, and semantic zoom preserve the user's temporal position.
- `FOLLOW LOADED TAIL` follows the end of the currently loaded page. `RETURN TO LOADED TAIL` restores that position after inspection. `NOW` labels only the loaded page end; it never claims complete daemon, provider, host, or global coverage.
- Agent and subagent branches diverge at evidenced spawn events, carry their own events, and reconnect only through evidenced handoffs or result/rejoin events. Inferred relations look different from exact relations.
- Branches collapse into deterministic workstream bundles at overview scale. Exact agent trees, event tables, and transcripts remain available; the visualization never becomes the sole evidence surface.
- Cross-page pivots preserve selection and time context when opening Sessions, Agents, Work, Code, Delivery, or exact evidence.

## Evidence ladder

Every node, edge, annotation, and inspector field uses one of the canonical evidence classes:

1. `exact` — immutable source event or exact joined identity.
2. `explicit` — persisted user/agent statement, task decision, or declared handoff.
3. `inferred` — a correlation with a stated basis, never rendered as fact.
4. `ambiguous` — multiple plausible identities or relationships remain.
5. `stale` — the source was once available but is no longer fresh enough for the claim.
6. `unavailable` — missing, private, inaccessible, or not ingested; no substitute is invented.

Private chain-of-thought is never a product input. Only visible persisted messages, summaries, decisions, tool events, tasks, code, and provider-visible evidence may appear.

## Required acceptance gates for every state

- Full keyboard traversal, seeking, selection, branch expansion, and return-to-tail behavior with visible focus.
- Reduced-motion mode replaces animated travel/pulses with stable state changes and an explicit cursor.
- Exact table, tree, transcript, diff, and event-log fallbacks preserve the same selection and evidence labels.
- Dense-real-data testing covers long histories, missing pages, overlapping workstreams, and 100+ unique agents without one permanent lane per agent.
- At 200% browser zoom, controls and text reflow without clipped evidence or forced two-axis page scrolling; resizable/collapsible regions remain operable.
- The visible `CONCEPT / SYNTHETIC DATA` boundary remains present until a view is bound to authenticated production evidence.
