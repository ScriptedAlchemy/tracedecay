# Sessions concept plates

## Purpose

Temporal session discovery and persisted transcript inspection with provider, coverage, redaction, source-availability, and Git/work provenance boundaries.

Route: `/sessions`.

## Authoritative final set

The reviewed implementation reference is [final/README.md](final/README.md). Historical lookbook plates remain in this folder for provenance but are superseded.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual and evidence-grade language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- `dashboard/src/workspaces/sessions/SessionsPage.tsx` reads typed Hermes LCM overview, timeline, and search routes; `SessionInspector.tsx` owns selected detail.
- Persisted transcript content, source availability, pagination, truncation, redaction, provider metadata, and Git/work correlations remain independently typed.

## Canonical semantic-state matrix

| Semantic state or interaction | Current explainer | Coverage |
|---|---|---|
| Message-volume temporal scope | [final/01-session-provenance-inspector.md](final/01-session-provenance-inspector.md) | Depicted. |
| Sortable, searchable session index | [final/01-session-provenance-inspector.md](final/01-session-provenance-inspector.md) | Depicted. |
| Complete, partial, unknown, served empty, unavailable, and transport failure | [final/01-session-provenance-inspector.md](final/01-session-provenance-inspector.md) | Depicted in list/legend. |
| Persisted transcript and source inspector | [final/01-session-provenance-inspector.md](final/01-session-provenance-inspector.md) | Selection is depicted; full detail workspace is specified by the brief. |
| Branch, worktree, commit, task, agent, Code, Loom, and Delivery links | [final/01-session-provenance-inspector.md](final/01-session-provenance-inspector.md) | Required drill-through contract. |
| Exact / explicit / inferred / ambiguous / stale / unavailable | [final/01-session-provenance-inspector.md](final/01-session-provenance-inspector.md) | Required product contract; private reasoning remains unavailable. |
| Keyboard, reduced motion, 200% zoom, dense data, exact fallback | [final/01-session-provenance-inspector.md](final/01-session-provenance-inspector.md) | Required acceptance gates. |

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
