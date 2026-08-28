---
design_status: current
evidence_class: concept_synthetic
---

# Automations: scheduler and run ledger

## User job

Understand what is scheduled, what is due or running, why work ran, skipped, retried, partially completed, or failed, what permissions it exercised, and whether its receipts and artifacts are trustworthy.

## Product behavior

- Scheduler status separates configured, running, paused, degraded, disconnected, and unavailable states and shows the authority, observed time, and next due window.
- Managed jobs expose schedule, last run, next due, state, skill, permission scope, concurrency policy, retry/backoff state, and the reason for every skip or denial.
- The latest-outcomes stream and durable run ledger distinguish queued, running, success, partial, skipped, cancelled, denied, failed, and stale. A retry is a new linked attempt, never a rewritten prior result.
- Selecting a run opens exact start/end times, trigger, attempt lineage, skill and authority, targets, fact receipt, logs/events, artifacts, integrity verdicts, and typed errors.
- Artifact presence and integrity are independent. A present artifact can be quarantined or mismatched; an absent artifact cannot be shown as verified.
- Pause, Resume, Retry, Cancel, or Run now may be enabled only when the shipping UI is connected to the real authenticated scheduler mutation path, policy permits the operation, and the result returns a durable receipt. Otherwise the control is absent or explicitly unavailable. The pictured controls do not authorize a fake handler.
- Destructive or broad-scope mutations show scope, permission, and confirmation before dispatch. Read-only users retain the complete ledger and evidence inspector.

## Production authorities

- `dashboard/src/workspaces/automations/AutomationsPage.tsx` and `RunHistory.tsx` own scheduler and run read models; their typed transport states remain visible.
- Scheduler configuration and observed runtime status are distinct authorities. Configuration cannot claim the scheduler is alive.
- Durable run records, attempt lineage, fact receipts, artifact manifests, integrity checks, permission decisions, and skill identity remain separately inspectable.
- A production mutation authority must authenticate, enforce policy, perform the operation, and persist a receipt end to end. Until that path exists, the concept is read-only for that action.

## Canonical evidence ladder

1. Durable run/attempt identity, trigger, timestamps, typed outcome, and immutable receipt.
2. Exact scheduler configuration, permission decision, skill identity, targets, and retry or concurrency lineage.
3. Logs/events, produced fact receipts, artifact manifests, checksums, and integrity/quarantine verdicts.
4. Aggregated due, skipped, success, partial, and failure summaries derived from named ledger windows.
5. Stale, ambiguous, denied, missing, malformed, disconnected, or unavailable authorities displayed without fallback success.

## Scale, navigation, and accessibility acceptance

- Long histories and large job fleets use virtualized tables, stable filters, and cursor pagination. Aggregates always declare their time window and reconcile to exact filtered ledger rows.
- Keyboard users can move through jobs and runs, filter outcomes, select attempts, inspect artifacts, and invoke only permitted controls with visible focus and confirmation.
- At 200% zoom, scheduler status, ledgers, and inspector reflow into resizable/collapsible regions without truncating reasons, permissions, or integrity evidence.
- Reduced motion disables live pulses, auto-scrolling, and animated run transitions; state remains legible through text, shape, and contrast. Follow-live can be paused for inspection.
- An exact text/table fallback for scheduler, job, run, attempt, receipt, artifact, permission, and event data is always available and exposes the same evidence to assistive technology.
- Dense-data tests cover thousands of runs, retry storms, simultaneous jobs, long errors, malformed jobs, skipped windows, denied writes, missing receipts, stale streams, and partial artifacts.

## Truth boundary

The image must retain the visible `CONCEPT / SYNTHETIC DATA` label. It approves the information hierarchy only; it does not prove the pictured scheduler is running, jobs executed, controls work, receipts exist, or artifacts passed integrity. Missing mutation or read authorities fail closed and remain visibly unavailable.
