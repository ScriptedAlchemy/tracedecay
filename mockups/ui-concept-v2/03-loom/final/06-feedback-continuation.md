---
design_status: current
---

# Loom 06 — Feedback continuation

- **Asset:** `06-feedback-continuation.png`
- **Lifecycle:** `current`
- **Boundary:** `CONCEPT / SYNTHETIC DATA`

## User job

Comment on produced code, challenge a visible persisted assumption or decision, or attach feedback to an event/task, then see whether later recorded work acknowledged, acted on, contradicted, or left that feedback open.

## Production authorities

- The local TraceDecay feedback store owns append-only feedback targets and `open`, `acknowledged`, `acted-upon`, or `contradicted` lifecycle records; it never rewrites the targeted source artifact.
- Sessions/LCM and admitted hook/events own transcript and execution evidence, Agents owns parentage, Work owns task identity, local Git/Code owns code/diff/commit/test evidence, and Delivery owns PR/outcome links.
- The deterministic layout projection visualizes feedback continuations without asserting unsupported causality. Provider/GitHub state remains read-only and unchanged, and private reasoning stays unavailable.

## Local-only boundary

Feedback is written to TraceDecay's local feedback authority. Its lifecycle is `open`, `acknowledged`, `acted-upon`, or `contradicted`. It does **not** post to GitHub, the agent provider, or another external system; provider and GitHub state remain unchanged until a real production write path explicitly supports that action.

## Interaction model

- Attach feedback to an exact code hunk, persisted reasoning artifact, explicit decision, task, or event.
- Choose a local review marker such as understood, risky, needs clarification, or challenge.
- Later events may connect to the feedback only when an exact/explicit link exists or as a separately labeled inference.
- The continuation path can be replayed from feedback creation through acknowledgement, revision, test, or contradiction.
- Immutable source evidence remains unchanged; feedback is an appended local record.

## Evidence and honesty

The feedback record, its author/time, and its target are exact local facts. “Acted upon” requires evidenced later work; proximity alone is not enough. Provider acknowledgements, private reasoning, and external comment delivery are never fabricated. The canonical evidence ladder applies to every continuation edge.

## Scale and accessibility acceptance

Feedback targets and lifecycle controls are keyboard-operable, support reduced motion (including a reduced-motion state), and remain readable at 200% zoom. Exact feedback table, transcript, code/diff, and branch-tree fallbacks remain available. Dense-real-data tests cover multiple feedback records and competing continuations. `CONCEPT / SYNTHETIC DATA` stays visible.
