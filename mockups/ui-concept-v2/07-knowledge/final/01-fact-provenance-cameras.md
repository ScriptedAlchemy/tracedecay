---
design_status: current
evidence_class: concept_synthetic
---

# Knowledge: fact provenance cameras

## User job

Determine what TraceDecay claims to know, inspect the retained source and observation history behind a fact, judge its trust and recency, and distinguish admitted knowledge from disputed, withheld, stale, or unavailable material.

## Product behavior

- Facts is the default camera: a searchable, sortable fact ledger joined to a constellation that groups related subjects without replacing the ledger.
- Selecting a fact opens canonical retained content, subject/type, repository and path provenance, first and last observation, trust history, availability, redaction state, and source verification.
- Trust is an evidence-weighted product signal, not a truth guarantee. The UI shows the policy/method and history behind it and never converts a score into an unsupported claim.
- Geometry is a separate camera and appears only when a real projection is served. An unsupported method or missing projection remains visibly unavailable; the UI never invents embeddings or geometry.
- Curation shows proposed, pending, applied, rejected, and partial operations with actor, policy, source, and receipt. Oplog preserves the append-only event history and its loading, partial, stale, and unavailable states.
- Redacted or withheld content keeps identity, reason, scope, and policy receipt where permitted, but never reconstructs or leaks the hidden value.
- Camera, query, filters, sort, selected fact, and page are URL-addressable. Navigation can pivot from a fact to its Code symbol, Sessions evidence, source transcript, or exact repository location when that relationship exists.

## Production authorities

- `dashboard/src/workspaces/knowledge/KnowledgePage.tsx` and generated memory overview, status, and fact-detail contracts own the Facts read model.
- `MemoryGeometry.tsx`, `CurationConsole.tsx`, and `MemoryOplog.tsx` own their independent camera reads; one camera's success cannot fabricate another's availability.
- Durable fact content, provenance, redaction policy, trust history, and oplog receipts remain distinct authorities and retain their native failure and freshness states.
- Source files, transcripts, and repository observations are evidence links, not mutable copies embedded into the visualization.

## Canonical evidence ladder

1. Exact retained fact content or an explicit withheld/redacted marker plus its durable fact identity.
2. Source record, repository/path or transcript reference, observation timestamps, and redaction/policy receipt.
3. Verified resolution to a current symbol or source plus tests/runtime observations, each bound to its own snapshot.
4. Trust, recency, grouping, or geometry projections labeled by method and source coverage.
5. Disputed, ambiguous, stale, partial, denied, missing, or unavailable evidence preserved without invention.

## Scale, navigation, and accessibility acceptance

- Large stores use a virtualized fact table with server-backed search, stable pagination or cursors, and independent constellation density aggregation.
- Keyboard users can switch cameras, search, sort, traverse rows, select a fact, inspect provenance, and return to the prior focus with a visible focus indicator.
- At 200% zoom, the fact ledger and inspector stack or become resizable/collapsible; canonical content and provenance never clip behind the constellation.
- Reduced motion removes constellation drift, camera flights, pulsing, and animated trust-history transitions while retaining meaning through labels, shapes, and contrast.
- Every camera has an exact text/table fallback. Geometry exposes its method and memberships as data; curation and oplog expose complete receipt/event rows.
- Dense-data tests cover long canonical content, many sources, thousands of facts, repeated observations, disputed facts, mixed redaction scopes, and unavailable camera authorities.

## Truth boundary

The image must retain the visible `CONCEPT / SYNTHETIC DATA` label. It approves the layout and camera model only; it does not prove the pictured facts, source paths, dates, trust values, availability, or graph relationships. Missing or private knowledge is never synthesized to fill the view.
