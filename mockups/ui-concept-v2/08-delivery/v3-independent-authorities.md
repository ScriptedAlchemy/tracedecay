---
design_status: superseded
---

# Delivery: v3 independent authorities

- **Asset:** `v3-independent-authorities.png`
- **Lifecycle:** `superseded`

## Intent

Independent local Git and provider panels with labeled recency and non-green failure, denial, and publication states.

## Entry condition

Open `/delivery` after registry response or explicit repository selection.

## Visible state

Not-published, denied, rate-limited, stale, and unavailable remain visible.

## Supported interactions

- Depicted: ready local Git, unavailable pull requests/reviews, rate-limited CI, stale retained evidence, not-published releases, and ready index freshness.
- It does not execute a provider refetch or repository selection.

## Truth boundary

This is a `CONCEPT / SYNTHETIC` lookbook plate, not runtime evidence. It establishes no production data, authority availability, counts, health, freshness, persistence, or control. Any unavailable production path remains visibly unavailable.

## Lifecycle history

Historical repository-authority study. It was replaced by the approved multi-state PR discovery, journey, replay, and review set in `final/`.
