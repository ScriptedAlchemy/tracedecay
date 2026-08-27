# Costs: v3 provider spend

- **Asset:** `v3-provider-spend.png`
- **Lifecycle:** `current`

## Intent

Actual spend, coverage, saved tokens, and unavailable pricing are distinct.

## Entry condition

Open `/costs` after spend and coverage authorities respond.

## Visible state

Unpriced/null remains explicitly non-zero or unknown.

## Supported interactions

- Depicted: priced spend, unpriced/null rows, absent values, count-only saved tokens, and a state legend.
- It does not execute a time-range change or provider refresh.

## Truth boundary

This is a `CONCEPT / SYNTHETIC` lookbook plate, not runtime evidence. It establishes no production data, authority availability, counts, health, freshness, persistence, or control. Any unavailable production path remains visibly unavailable.

## Lifecycle history

Pre-Task-1 canonical selection for Costs. Lifecycle is an explicit editorial decision; the version stem records iteration order only.
