# Code concept plates

**Authoritative final set:** [final/README.md](final/README.md)

## Purpose

Cortex, Trace, and Core semantic lenses with symbol, file, call, impact, test, freshness, and diagnostic drill-through to exact evidence.

Route: `/code`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/code/CodePage.tsx` reads `/api/plugins/graph/{overview,search,subgraph}` with `GraphOverviewPayloadV1Schema`, `GraphSearchPayloadV1Schema`, and `GraphSubgraphPayloadV1Schema`; `CodeDiagnostics.tsx` and `IndexFreshness.tsx` own state readouts.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Loaded Cortex topology | [final/01-semantic-cortex.md](final/01-semantic-cortex.md) | Open `/code` with a served graph overview. |
| Search and selected symbol | [final/01-semantic-cortex.md](final/01-semantic-cortex.md) | Search or select a graph/table result. |
| Callers and callees | [final/01-semantic-cortex.md](final/01-semantic-cortex.md) | Inspect the selected symbol's explicit relationships. |
| File, source, impact, and tests | [final/01-semantic-cortex.md](final/01-semantic-cortex.md) | Drill from selection into exact evidence modes. |
| Fresh, stale, warming, partial, unavailable diagnostics | [final/01-semantic-cortex.md](final/01-semantic-cortex.md) | Read each independent authority state. |
| Loaded Trace or Core lens | No current plate | Required interaction/result is specified but not pictured as a separate still. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
