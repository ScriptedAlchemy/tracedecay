---
design_status: current
evidence_class: concept_synthetic
---

# Explorer: browse and query lanes

- **Asset:** `01-browse-query-lanes.png`
- **Lifecycle:** `current`

## User job

Ask one question across the code, Sessions, Knowledge, and semantic authorities; understand which sources answered, which are still working or degraded, and inspect the exact evidence behind any result without conflating independent source states.

## Product behavior

- Scope is explicit and URL-addressable: all registered projects, one project, or a narrower supported source scope.
- One submitted query creates independent lanes for exact code, persisted Sessions, Knowledge, and semantic retrieval. Each lane owns its own create, poll, cancel, pagination, freshness, and terminal state.
- Lane filters narrow source, result kind, time, and size without silently changing project scope.
- Selecting a result opens an exact-result inspector with stable identity, path or source record, snippet, enclosing symbols, calls/references when available, provenance, and the authority that supplied it.
- Code results remain exact lexical or indexed-source matches. Sessions results link to persisted transcript evidence. Knowledge results identify the fact/concept source. Graph or semantic results expose their correlation basis rather than masquerading as exact matches.
- A running or unavailable lane never blocks ready lanes. Served empty, indexing, partial, stale, cancelled, timed out, denied, failed, and unavailable remain distinct.

## Interaction model

- `Enter` submits; `Escape` closes the inspector before it cancels a query. Cancellation requires an explicit focused action.
- Arrow keys traverse lanes and results; `Enter` opens the selected result; a breadcrumb returns to the same query, scope, lane position, and filters.
- Result actions pivot into Code, Sessions, Knowledge, Brain, or the exact table/list fallback while preserving source identity.
- Long result sets virtualize by lane. Semantic zoom may aggregate graph context, but exact results and their rank/provenance remain inspectable.

## Production authorities

- `dashboard/src/workspaces/explorer/ExplorerPage.tsx` owns the workspace composition; its controller and lane model own independent lane lifecycle and selection.
- `controller.ts`, `laneModel.ts`, and `absence.ts` own create/poll/cancel and typed absence behavior; `Inspector.tsx` owns exact selected-result detail.
- The code index owns lexical/exact source matches; Sessions and Knowledge remain separately typed authorities. Graph and semantic authorities may enrich a result only with an explicit join and evidence grade.
- The shell scope register owns project scope. A lane cannot invent cross-project visibility or substitute a different project's data when its requested authority is unavailable.

## Evidence and truth states

- `EXACT`: a source occurrence, stable symbol identity, persisted transcript message, or fact record returned by its owning authority.
- `EXPLICIT`: persisted user or agent language quoted from a session or knowledge artifact.
- `INFERRED`: semantic similarity, graph relationship, or cross-source correlation with its basis named.
- `AMBIGUOUS`: multiple plausible identities, source records, or project matches remain selectable.
- `STALE`: a result is served from an index or source outside its declared freshness window.
- `UNAVAILABLE`: a lane is absent, denied, not indexed, unsupported, failed, or otherwise inaccessible; no empty success is fabricated.

## Acceptance gates

- Every query, lane, filter, cancellation, result, and inspector action has a keyboard path and visible focus.
- Reduced motion removes progress travel and graph animation while retaining lane lifecycle and selection.
- At 200% browser zoom, lane controls and inspector reflow into a single-column or focus mode; result text and code do not become canvas-scaled or clipped.
- Exact list/table, source snippet, transcript, and graph-neighborhood fallbacks retain the same selection and evidence grades.
- Dense-real-data tests cover long result sets, independent pagination, mixed terminal states, and one slow or unavailable lane without rendering thousands of DOM rows.

## Truth boundary

This is a **CONCEPT / SYNTHETIC DATA** plate. Names, counts, progress, snippets, timestamps, and readiness shown in the image are interaction examples, not runtime evidence. Production UI must render only results and authority states returned by the real query paths.
