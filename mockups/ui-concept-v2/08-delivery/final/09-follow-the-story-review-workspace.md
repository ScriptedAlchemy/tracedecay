---
design_status: current
evidence_class: concept_synthetic
---

# Follow the Story review workspace

## Approval scope

The full-size interaction layout is approved. The pictured `Compiler cache / PR #8127` case is explicitly synthetic and must not be treated as evidence for any real PR or session.

## User job

Complete a large PR review one meaningful semantic change episode at a time while reading exact code and preserving the causal story.

## Product behavior

- Start or Continue review opens this dedicated workspace from the journey overview.
- Previous and Next step through Objective, Observation, Decision, Implementation, Verification, Feedback or Revision, and Outcome.
- Persistent progress reports reviewed episodes, code coverage, unresolved questions, named `test_risk` or `unsafe_patterns`, weak evidence, contradictions, and unreviewed changes. It never substitutes a generic numeric risk score.
- Story, Code and Impact, Evidence, and Feedback are the four primary modes.
- The dominant pane synchronizes an exact diff/hunk with a before/after temporal semantic graph.
- Selecting a visible decision highlights produced code and tests; selecting code pivots back to its source artifact.
- Local actions include comment on hunk, challenge visible decision, attach feedback to episode or task, and mark understood, risky, or needs clarification.
- Local feedback has an explicit lifecycle: `open` → `acknowledged` → `acted-upon` or `contradicted`. Acted-upon feedback creates a visible later task, revision, test, and outcome continuation; contradicted feedback retains the counter-evidence and resolution source.

## Scale and truth

Hundreds of raw turns and agents are curated into sourced semantic episodes with drill-down provenance. Adjustable panes, focus-plus-context, branch bundles, and a virtualized navigator replace the rejected fixed narrow inspector. Private chain-of-thought is never implied.

## Access gates

- Keyboard navigation reaches Previous and Next, the progress map, all four modes, exact code, source evidence, feedback actions, and resizable-pane controls.
- Reduced motion removes graph travel, animated causal continuation, and pane transitions while preserving step order, selection, and feedback state.
- At 200% zoom, Story, Code and Impact, Evidence, and Feedback become focused or stacked regions; exact code never shrinks into unreadable microtext.
- Exact diff, semantic-change table, transcript, episode tree, evidence table, and feedback-history fallbacks preserve the full review path.

## Production authorities

- Local Git and Code own exact diff hunks, symbols, before/after graphs, and test relationships.
- Work, Sessions, and Agents own objectives, tasks, persisted claims and summaries, agent attribution, and event provenance; none supplies private chain-of-thought.
- Provider PR, review, CI, release, and freshness projections remain independently typed and read-only in this concept.
- Local feedback storage owns TraceDecay comments, challenges, review marks, and the `open` → `acknowledged` → `acted-upon`/`contradicted` lifecycle; it cannot impersonate a provider write.
- The review workspace joins these authorities without losing `exact`, `explicit`, `inferred`, `ambiguous`, or `unavailable` source grades.
