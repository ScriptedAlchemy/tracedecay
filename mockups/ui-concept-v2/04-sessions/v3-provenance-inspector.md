---
design_status: current
---

# Sessions: v3 provenance inspector

- **Asset:** `v3-provenance-inspector.png`
- **Lifecycle:** `current`

## Intent

Provider-qualified sessions, token provenance, paged inspection, coverage, and redaction.

## Entry condition

Open `/sessions` after a timeline/list response.

## Visible state

`exists:false`, served empty, transport failure, and unavailable remain distinct.

## Supported interactions

- Depicted: search chrome, provider-qualified rows, selected raw inspector, page-two pagination, coverage/truncation, and redaction/store fields.
- The state legend is reference material; the still does not execute search or paging.

## Truth boundary

This is a `CONCEPT / SYNTHETIC` lookbook plate, not runtime evidence. It establishes no production data, authority availability, counts, health, freshness, persistence, or control. Any unavailable production path remains visibly unavailable.

## Lifecycle history

Pre-Task-1 canonical selection for Sessions. Lifecycle is an explicit editorial decision; the version stem records iteration order only.
