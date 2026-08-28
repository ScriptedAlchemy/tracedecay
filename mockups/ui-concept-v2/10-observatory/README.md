# Observatory concept plates

## Purpose

Observatory sources with independent coverage and failure boundaries, never an invented heartbeat.

Route: `/observatory`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/observatory/ObservatoryPage.tsx` reads `/api/storage/telemetry`, `/api/code-index/freshness`, `/api/observatory`, and analytics diagnostics; `DoctorInspector.tsx`, `CanonicalObservations.tsx`, `PerformanceBudgets.tsx`, and `HookHints.tsx` own panels.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Doctor findings | [final/01-system-evidence-overview.md](final/01-system-evidence-overview.md) | Open `/observatory`; select a Doctor finding to bind the inspector. |
| Doctor partial/stale | [final/01-system-evidence-overview.md](final/01-system-evidence-overview.md) | A Doctor source reports incomplete coverage or an expired observation time. |
| Measured/partial/stale/denied/unavailable sources | [final/01-system-evidence-overview.md](final/01-system-evidence-overview.md) | Each daemon, store, provider, index, and runtime source reports its own typed state. |
| Budget under/over/unmeasured | [final/01-system-evidence-overview.md](final/01-system-evidence-overview.md) | A named performance budget and comparison are served, or measurement is absent. |
| Hook coverage partial | [final/01-system-evidence-overview.md](final/01-system-evidence-overview.md) | Hook evidence supplies a covered/total boundary and rejected-argument categories. |
| Store measured or unavailable | [final/01-system-evidence-overview.md](final/01-system-evidence-overview.md) | Storage telemetry responds with measurements or a typed failure. |
| Code-index stage progress | [final/01-system-evidence-overview.md](final/01-system-evidence-overview.md) | The index freshness/progress authority reports per-stage status. |
| Timeline-range filtering and finding inspection | [final/01-system-evidence-overview.md](final/01-system-evidence-overview.md) | Select a canonical observation/range or finding while preserving project scope. |
| A refetch or recovery result | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
