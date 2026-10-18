---
name: pricing-change-review
description: Use before or while reviewing a change to order pricing, line-item totals, fees, or discounts in this crate.
---

# Pricing change review

Check each item and say what the change affects:

1. Both total functions. `compute_grand_total` is a byte-identical copy of
   `compute_total`; a change to one must land in both or the totals diverge.
2. Recorded pricing decisions. The 2026-06 pricing review caps discounts at
   25 percent (`apply_discount`); look up recorded decisions before judging.
3. Units. Every amount is integer USD cents; keep arithmetic in cents.
4. Tests. `tests/pricing_test.rs` covers `compute_total` only; name what the
   change leaves untested.

End with the concrete risk and what to verify.
