# Workflows: v3 definition ledger

- **Asset:** `v3-definition-ledger.png`
- **Lifecycle:** `current`

## Intent

Definition ledger with immutable versions, pinned digests, CAS activate/retire/reject, and on-demand run lookup.

## Entry condition

Open `/workflows` after the definition registry responds.

## Visible state

Lifecycle decisions and run-lookup boundaries are named.

## Supported interactions

- Definitions
- detail
- lifecycle CAS
- run lookup.

## Truth boundary

This is a `CONCEPT / SYNTHETIC` lookbook plate, not runtime evidence. It establishes no production data, authority availability, counts, health, freshness, persistence, or control. Any unavailable production path remains visibly unavailable.

## Lifecycle history

Pre-Task-1 canonical selection for Workflows. Lifecycle is an explicit editorial decision; the version stem records iteration order only.
