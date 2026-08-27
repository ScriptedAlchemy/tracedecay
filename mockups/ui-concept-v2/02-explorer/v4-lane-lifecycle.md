---
design_status: current
---

# Explorer: v4 lane lifecycle

- **Asset:** `v4-lane-lifecycle.png`
- **Lifecycle:** `current`

## Intent

Four lanes with independent states and create, poll, cancel framing.

## Entry condition

Open `/explorer` and begin a supported search.

## Visible state

Each source state is separately labeled.

## Supported interactions

- Depicted: submitted create, running poll, available cancel, selected code hit, and lanes in ready, partial, measured-empty, and absent states.
- The legend labels additional states; the still does not execute a query or cancellation.

## Truth boundary

This is a `CONCEPT / SYNTHETIC` lookbook plate, not runtime evidence. It establishes no production data, authority availability, counts, health, freshness, persistence, or control. Any unavailable production path remains visibly unavailable.

## Lifecycle history

Pre-Task-1 canonical selection for Explorer. Lifecycle is an explicit editorial decision; the version stem records iteration order only.
