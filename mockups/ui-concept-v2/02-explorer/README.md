# Explorer concept plates

## Purpose

Scoped browsing and query across independently typed code, Sessions, Knowledge, and semantic/graph sources, with exact result drill-down.

Route: `/explorer`.

## Authoritative final set

The reviewed implementation reference is [final/README.md](final/README.md). Historical lookbook plates remain in this folder for provenance but are superseded.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and evidence-grade language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- `dashboard/src/workspaces/explorer/ExplorerPage.tsx` composes the controller, lane model, typed absence model, and inspector.
- The code index, Sessions, Knowledge, graph, and semantic sources remain independent. The concept is synthetic and cannot make an unavailable authority appear ready.

## Canonical semantic-state matrix

| Semantic state or interaction | Current explainer | Coverage |
|---|---|---|
| Scoped query and filters | [final/01-browse-query-lanes.md](final/01-browse-query-lanes.md) | Depicted. |
| Independent source lanes | [final/01-browse-query-lanes.md](final/01-browse-query-lanes.md) | Depicted with ready, partial, indexing, and running states. |
| Exact result selection and inspector | [final/01-browse-query-lanes.md](final/01-browse-query-lanes.md) | Depicted. |
| Create, poll, and cancel lifecycle | [final/01-browse-query-lanes.md](final/01-browse-query-lanes.md) | Running and cancel are depicted; terminal cancellation is specified, not pictured. |
| Exact / explicit / inferred / ambiguous / stale / unavailable | [final/01-browse-query-lanes.md](final/01-browse-query-lanes.md) | Required product contract; not every state is pictured simultaneously. |
| Keyboard, reduced motion, 200% zoom, dense data, exact fallback | [final/01-browse-query-lanes.md](final/01-browse-query-lanes.md) | Required acceptance gates. |

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
