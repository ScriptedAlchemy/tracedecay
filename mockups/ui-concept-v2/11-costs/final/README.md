# Costs final state set

This folder is the authoritative implementation reference for Costs. The provider-spend state joins actual usage, canonical pricing, attribution, and budgets without erasing unpriced or unknown coverage.

The image is an interaction reference, not a runtime receipt. All pictured values are visibly `CONCEPT / SYNTHETIC DATA`.

## State manifest

| State | Image | Product brief | Status |
|---|---|---|---|
| Provider spend attribution | [01-provider-spend-attribution.png](01-provider-spend-attribution.png) | [01-provider-spend-attribution.md](01-provider-spend-attribution.md) | approved |

## Implementation rule

Never manufacture dollar values. Priced, unpriced, null/unknown, unavailable, stale, denied, and measured-empty usage remain independently visible, and totals disclose the included pricing coverage.
