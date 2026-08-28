# Automations concept plates

**Authoritative final set:** [final/README.md](final/README.md)

## Purpose

Scheduler, managed jobs, permissions, skips, retries, runs, receipts, and artifact integrity without fake execution controls.

Route: `/automations`.

## Production authorities

- [NAVIGATION.md](../NAVIGATION.md) owns shell, route, scope behavior, and persistent regions.
- [DESIGN-SYSTEM.md](../DESIGN-SYSTEM.md) owns visual/typed-state language; [INTERACTION-STATES.md](../INTERACTION-STATES.md) owns required coverage.
- At `975a0acb`, `dashboard/src/workspaces/automations/AutomationsPage.tsx`, `RunHistory.tsx`, and `AutomationsPage.transport.dom.test.tsx` own the scheduler/run read models and transport states.
- The concept plate remains synthetic; these source paths identify the production authority, not a claim that the pictured fixture data is live.

## Canonical semantic-state matrix

| Depicted semantic state or interaction | Current explainer | Entry condition |
|---|---|---|
| Scheduler configured/running/paused/unavailable | [final/01-scheduler-run-ledger.md](final/01-scheduler-run-ledger.md) | Read configuration and observed runtime authorities separately. |
| Due, overdue, running, skipped, partial, failed | [final/01-scheduler-run-ledger.md](final/01-scheduler-run-ledger.md) | Inspect named job and ledger windows. |
| Managed jobs, skills, and permissions | [final/01-scheduler-run-ledger.md](final/01-scheduler-run-ledger.md) | Select a job or run. |
| Attempts, retries, and concurrency lineage | [final/01-scheduler-run-ledger.md](final/01-scheduler-run-ledger.md) | Expand the selected run lineage. |
| Receipts, artifacts, and integrity | [final/01-scheduler-run-ledger.md](final/01-scheduler-run-ledger.md) | Inspect exact run evidence. |
| Denied, malformed, stale, disconnected, unavailable | [final/01-scheduler-run-ledger.md](final/01-scheduler-run-ledger.md) | Read each independent authority state. |
| A pause or resume mutation | No current plate | Required interaction/result is not depicted. |

“Depicted” means visible in the plate (including a labelled state legend), not executed by the still. “No current plate” is reserved for required behavior or result that no current plate pictures.

## Historical provenance

Superseded and rejected lookbook iterations were removed from the branch tip after the reviewed `final/` set became authoritative. Git history through `e9a30ad1d` remains the recovery source for those assets and sidecars.
