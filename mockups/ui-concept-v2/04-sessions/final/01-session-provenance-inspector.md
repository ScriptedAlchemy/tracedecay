---
design_status: current
evidence_class: concept_synthetic
---

# Sessions: temporal list and provenance inspector

- **Asset:** `01-session-provenance-inspector.png`
- **Lifecycle:** `current`

## User job

Find a recorded session, understand when and where it ran, judge transcript coverage and source availability, and open the persisted conversation and its branch, worktree, commit, task, agent, code, or Delivery relationships without implying access to private chain-of-thought.

## Product behavior

- A message-volume time field and date controls set the temporal scope; the table remains the exact, sortable session index.
- Search targets persisted transcripts and messages separately from row filters. Provider, project, time, status, coverage, and source availability stay visible.
- Each row exposes stable session identity, provider/model identity when recorded, project, start/end, message count, token provenance, coverage, and typed lifecycle.
- Selecting a row opens a spacious inspector or dedicated detail route with persisted messages, source path/authority, pagination and truncation, redaction, branch/worktree, commits, linked tasks, agent/subagent identities, and code or Delivery pivots.
- Transcript text is shown only when ingested and permitted. Provider links, retained summaries, tool events, and source messages are distinct source classes. Private chain-of-thought is never reconstructed or presented.
- Complete, partial, unknown, served empty, unavailable, redacted, transport failure, stale, active, ended, and failed states remain distinct.

## Interaction model

- Keyboard users traverse the timeline, filters, virtualized table rows, pagination, and inspector regions with visible focus and preserved selection.
- Date brushing or a custom range filters the list; opening and closing detail returns to the same row, sort, page, and temporal viewport.
- Cross-links open Loom at the selected time, Agents at the attributed participant, Work at the linked task, Code at an exact changed symbol, or Delivery at the associated PR/commit.
- Dense timelines aggregate by bucket and expose exact counts on focus; long tables and transcripts virtualize and page without pretending loaded coverage is complete history.

## Production authorities

- `dashboard/src/workspaces/sessions/SessionsPage.tsx` owns the session list and reads the typed Hermes LCM overview, timeline, and search routes.
- `SessionInspector.tsx` owns selected-session detail. The owning session store and canonical redaction/content authority own persisted message bodies and availability.
- Git, Work, Agents, Code, and Delivery links require stable joined identities. A time or path coincidence alone is an inferred correlation, not exact attribution.
- Provider source availability, pagination, truncation, redaction, and transport status remain independent from whether a session identity exists.

## Evidence and truth states

- `EXACT`: session identity, persisted source message, source path, recorded tool event, branch/worktree identity, commit SHA, or direct durable link from its owner.
- `EXPLICIT`: a visible persisted user/agent statement, summary, decision, or handoff record attributed to its source and time.
- `INFERRED`: a session-to-task, session-to-code, or session-to-Delivery relation based on named temporal/Git correlation.
- `AMBIGUOUS`: multiple sessions, branches, agents, or commits plausibly match; candidates remain visible.
- `STALE`: provider or derived metadata exceeded its freshness contract even if the transcript remains durable.
- `UNAVAILABLE`: missing, private, not ingested, denied, redacted, unsupported, or failed source material leaves an explicit gap.

## Acceptance gates

- Full keyboard traversal, search, date-range selection, paging, row expansion, and cross-page pivots with visible focus.
- Reduced motion replaces animated timeline changes with static bucket and cursor updates.
- At 200% browser zoom, the overview, table, and inspector reflow or enter dedicated focus modes; transcript and identifier text remain readable.
- Exact table, transcript, source metadata, and event-log fallbacks preserve selection and evidence labels.
- Dense-real-data tests cover thousands of sessions, long transcripts, partial pages, mixed providers, missing source files, redaction, and ambiguous Git links.

## Truth boundary

This is a **CONCEPT / SYNTHETIC DATA** plate. Session IDs, providers, counts, dates, coverage, and statuses are illustrative. The production surface must be driven by authenticated typed authorities and must never expose or invent private reasoning.
