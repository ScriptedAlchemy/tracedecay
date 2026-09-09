# Concept interaction-state matrix

The image set is organized by semantic state. Each screen README explicitly
chooses a current same-stem explainer for every covered state and records every
PNG exactly once as `current`, `superseded`, or `rejected`. Filenames record
iteration order only. Every PNG has an exact same-stem Markdown explainer.

All sequences use the normalized shell, trace-tail logo, route order, color
semantics, and interaction language in `DESIGN-SYSTEM.md` and `NAVIGATION.md`.
Hover inspects; click scopes or selects only where a production path exists;
only real admitted activity blooms. Missing, served empty, partial, stale,
offline, denied, and unavailable remain visibly distinct.

Every state also applies the canonical evidence-grade ladder from
`DESIGN-SYSTEM.md`: `EXACT`, `EXPLICIT`, `INFERRED`, `AMBIGUOUS`, `STALE`, or
`UNAVAILABLE`. Source classes—including `RETAINED`, `OBSERVED`, `PR BODY`,
`COMMIT`, `TRANSCRIPT`, and `CHECK RESULT`—identify provenance and do not
replace the grade. State sets must remain fully operable by keyboard, retain
non-color encodings and exact text/table/transcript fallbacks, preserve meaning
under reduced motion, and reflow or provide focus modes at 200% browser zoom.

## Priority sequences

### Brain

1. Registry overview: measured all-project field; recency across, indexed mass
   upward, real repository hubs, accessible registry rail, admitted-activity
   HUD with its Feed/Authority state separate.
2. Project hover/focus: isolate the drawn neighborhood and dim outsiders.
   Hover inspects only.
3. Project narrow: click a project body or registry row to set scope. A
   repository hub does not narrow.
4. Scoped project: replace the registry field with what TraceDecay knows about
   that project—code graph plus graph, memory, analytics, identity, and checkout
   authorities.
5. Return to all projects: explicit scope clear.
6. Real activity: bloom exact `project_id`; conduct across at most one
   evidenced, drawn relation. A shared-repository hop does not light sibling checkouts.
7. Honest states: registry loading, empty, partial, unavailable, inconsistent,
   or truncated; stream connecting/offline; graph empty/renderer unavailable.

### Loom — accepted final target

The [final manifest](03-loom/final/README.md) and its seven same-stem briefs
own this horizontal target. The older vertical layout below is an implementation
inventory, not permission to substitute legacy geometry for the final plates.

1. Follow loaded tail: time runs left to right; the cursor follows the end of
   the currently loaded LCM page. Pan or select pauses follow; return-to-tail
   resumes it. `NOW` means the loaded page end, never a global stream.
2. Temporal replay: seek/play a cursor within the loaded page. Future events
   remain withheld, not merely dimmed. Playback and follow are distinct state
   dimensions: paused playback may still follow newly admitted loaded events.
3. Branching execution: parent → evidenced spawn → parallel agent/subagent
   branches → evidenced handoff/rejoin. Collapse preserves counts and grades.
4. Dense overview: deterministic workstream bundles; semantic zoom reveals
   groups → agents → events. Unique agents and participations remain distinct.
5. Selected event: a roomy evidence workspace with exact transcript, task,
   diff, source identity, and graded causal neighborhood, plus exact fallbacks.
6. Feedback continuation: local feedback and evidenced later acknowledgement,
   action, or contradiction; no inferred acknowledgement rendered as fact.
7. Evidence gaps: loading, empty, partial, stale, offline, denied, ambiguous,
   unavailable sources and linked boundaries remain separate typed states.

Pan, fit, minimap, keyboard seek and semantic zoom retain temporal context.
These are accepted target interactions; a shipping route must expose their
actual availability rather than claiming integration from the concept alone.

### Loom — legacy implementation baseline

The earlier weave maps measured time down and hosts across, with strand width
by message count and recorded/open/unknown ends. Its zoom/pan/fit, selected
thread table, canonical thread chain and loaded-page replay remain useful
behavioral evidence. Their presence does not establish the final horizontal
hierarchical execution layout. Preserve loaded-page bounds, separate source
coverage, initial paused playback and 0.5–4x presentation speed during migration.

## Production inventory and behavioral evidence

- Brain project-body/registry-row scoping, repository-hub exclusion, and exact
  activity identity/one-hop propagation:
  `dashboard/src/workspaces/brain/BrainPage.tsx`,
  `dashboard/src/workspaces/brain/ScopedBrain.tsx`,
  `dashboard/src/workspaces/brain/ScopedBrain.dom.test.tsx`, and
  `dashboard/src/viz/graph/GraphCanvas.propagation.dom.test.tsx`.
- Loom time-window zoom/pan/fit and visible-window culling:
  `dashboard/src/workspaces/loom/WeaveCanvas.tsx` and
  `dashboard/src/workspaces/loom/WeaveCanvas.dom.test.tsx`.
- Loom loaded-page replay, pause/play, previous/next, seek, 0.5-4x presentation
  speed, follow-loaded-tail, and return-to-loaded-tail:
  `dashboard/src/workspaces/loom/ThreadPlayback.tsx`,
  `dashboard/src/workspaces/loom/playback.ts`,
  `dashboard/src/workspaces/loom/playback.test.ts`, and
  `dashboard/src/workspaces/loom/LoomPage.dom.test.tsx`.
- Loom thread-chain sources and separate coverage states:
  `dashboard/src/workspaces/loom/ThreadChain.tsx` and
  `dashboard/src/workspaces/loom/LoomPage.dom.test.tsx`.

## Remaining workspace coverage

| Workspace | Canonical interaction plates | Honest-state concepts |
|---|---|---|
| Explorer | browse lanes; search progress/cancel; result inspector | complete empty, partial, stale, offline, source unavailable, cancelled, error |
| Sessions | timeline/list; transcript search; paged inspector | empty window, partial page, offline, store/temporal unavailable, token count unknown |
| Agents | overview; delegation/handoff; failure context | empty store, no delegation, partial attempt coverage, denied, unavailable |
| Code | Cortex; graph hover; Trace; Core | index empty, stale/warming index, renderer/diagnostics unavailable |
| Knowledge | Facts; Geometry; Curation; Oplog | empty store, partial coverage, detail/geometry unavailable, curation locked/partial |
| Delivery | repository field; selected repository; pipeline | empty registry, stale, failed, rate-limited, denied, unavailable, unknown branches |
| Automations | overview; scheduler pause/resume; run artifacts | empty, partial list, offline, denied, unavailable, artifact mismatch |
| Observatory | overview; index progress; storage findings | empty, partial, stale, baseline pending, unsupported, blocked progress |
| Costs | actual spend and coverage | empty ledger, partial coverage, source/pricing unavailable, identity unknown |
| Settings | effective config; filter/focus; review; conflict | no match, read-only, Remote unconfigured/partial/unavailable, CAS conflict |
| Work | board; projection switch; selected task; topology selection | empty board, stale generation, runtime/attempt/topology unavailable, stream offline |
| Workflows | definitions; detail; lifecycle CAS; run lookup | empty registry, unknown, runtime unavailable, CAS conflict, concealed run |

Concepts never add hover, zoom, drill-down, filters, or controls that the
shipping product does not expose. Typed-state plates may share a composition
when the visible distinction is fully documented, but no typed state may be
collapsed into an empty or healthy-looking panel.
