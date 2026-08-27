# Automations: v3 scheduler ledger

- **Asset:** `v3-scheduler-ledger.png`
- **Lifecycle:** `current`

## Intent

Pause/resume, due/skip, jobs, skills, receipts, run ledger, artifacts, and integrity.

## Entry condition

Open `/automations` after scheduler and run-ledger authorities respond.

## Visible state

Artifacts and integrity evidence are separate.

## Supported interactions

- Depicted: paused/configured scheduler, write lock, unavailable retry, due/skip, malformed job, skill states, receipt outcomes, run ledger, and failed artifact integrity.
- It does not execute pause/resume or a run action.

## Truth boundary

This is a `CONCEPT / SYNTHETIC` lookbook plate, not runtime evidence. It establishes no production data, authority availability, counts, health, freshness, persistence, or control. Any unavailable production path remains visibly unavailable.

## Lifecycle history

Pre-Task-1 canonical selection for Automations. Lifecycle is an explicit editorial decision; the version stem records iteration order only.
