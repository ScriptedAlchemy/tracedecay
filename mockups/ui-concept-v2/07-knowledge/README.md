# Knowledge concept plates

**Authoritative final set:** [final/README.md](final/README.md)

## Purpose

Independent Facts, Geometry, Curation, and Oplog cameras for source, redaction, trust, recency, and durable fact history without invented memory.

Route: `/knowledge`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/knowledge/KnowledgePage.tsx` uses generated `MemoryOverviewPayloadV1Schema`, `MemoryStatusPayloadV1Schema`, and `MemoryFactDetailPayloadV1Schema`; `MemoryGeometry.tsx`, `CurationConsole.tsx`, and `MemoryOplog.tsx` own camera reads.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Facts ledger and constellation | [final/01-fact-provenance-cameras.md](final/01-fact-provenance-cameras.md) | Open `/knowledge` with served fact overview authority. |
| Selected fact content and provenance | [final/01-fact-provenance-cameras.md](final/01-fact-provenance-cameras.md) | Select a fact row or constellation node. |
| Trust history and recency | [final/01-fact-provenance-cameras.md](final/01-fact-provenance-cameras.md) | Inspect the selected fact's sourced observation history. |
| Redacted, withheld, disputed, stale, or unavailable | [final/01-fact-provenance-cameras.md](final/01-fact-provenance-cameras.md) | Read the fact and camera authorities independently. |
| Geometry unavailable | [final/01-fact-provenance-cameras.md](final/01-fact-provenance-cameras.md) | No supported projection is served. |
| Curation and Oplog cameras | [final/01-fact-provenance-cameras.md](final/01-fact-provenance-cameras.md) | Switch camera; behavior is specified by the final brief. |
| A completed curation mutation | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
