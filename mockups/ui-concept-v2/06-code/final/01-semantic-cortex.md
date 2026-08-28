---
design_status: current
evidence_class: concept_synthetic
---

# Code: semantic cortex

## User job

Find a symbol or subsystem, understand how it is connected, assess what a change can affect, and reach the exact source and tests without losing repository or snapshot context.

## Product behavior

- Cortex is the broad semantic topology. Trace narrows to a selected symbol's callers, callees, uses, and data or control relationships. Core focuses the smallest causally relevant neighborhood.
- Search accepts symbols, files, paths, and qualified names. Results retain project, repository snapshot, language, and symbol identity.
- Selecting a node focuses its causal neighborhood and opens its exact path, range, kind, owning module, callers, callees, references, diagnostics, impact, and mapped tests.
- Hover previews identity and relationship type without changing selection. Click pins selection; double-click or Enter drills into the next semantic lens. Back returns to the prior graph camera.
- Filters cover symbol kind, language, module, relationship, diagnostic state, and freshness. URL state preserves project, lens, query, selection, and camera.
- Graph layout and luminosity communicate topology, kind, selection, and freshness; neither visual prominence nor rank is evidence of importance on its own.

## Production authorities

- `dashboard/src/workspaces/code/CodePage.tsx` and the generated graph overview, search, and subgraph contracts own served graph reads.
- The indexed repository snapshot owns symbol identity, path, range, and relationship provenance.
- `CodeDiagnostics.tsx` and `IndexFreshness.tsx` own diagnostic and freshness states; stale diagnostics never masquerade as current.
- Impact and test-map projections may enrich a selection only when their source snapshot and correlation grade are explicit.
- Exact source, diff, and table views remain DOM-rendered evidence surfaces. The visual scene is a projection, not a replacement authority.

## Canonical evidence ladder

1. Exact source bytes, file/range, repository identity, and snapshot or commit.
2. Index-resolved symbol identity and explicit parser-derived relationships.
3. Snapshot-bound diagnostics, impact dependents, and mapped tests with their own freshness.
4. Ranked, clustered, or inferred relationships labeled with method and confidence class.
5. Ambiguous, stale, partial, denied, or unavailable evidence shown as such and never silently omitted.

## Scale, navigation, and accessibility acceptance

- Dense repositories aggregate into stable module and workstream clusters; semantic zoom progresses from modules to symbols to exact edges without creating thousands of DOM nodes.
- A searchable virtualized tree/table provides the complete graph fallback and supports path-to-root, path-to-selection, callers-only, callees-only, impacted-only, and tests-only focus.
- All search, lens, filter, selection, expansion, and drill-through operations work by keyboard with a visible focus state. Focus order follows shell → controls → graph/list → inspector.
- At 200% browser zoom, controls and evidence reflow without clipping; the graph may occupy a focus mode while the inspector becomes resizable or collapsible.
- Reduced motion disables camera flights, pulsing, and animated graph transitions while preserving state through shape, text, and contrast.
- The exact source/table fallback is available at every zoom level and exposes the same selection, relationships, freshness, and evidence grades to assistive technology.

## Truth boundary

The image must retain the visible `CONCEPT / SYNTHETIC DATA` label. It approves the visual and interaction model only: it does not prove the pictured symbols, counts, graph rank, diagnostic state, index freshness, or availability. Unserved production paths fail closed.
