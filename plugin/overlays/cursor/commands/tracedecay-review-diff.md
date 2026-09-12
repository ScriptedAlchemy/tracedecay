---
name: tracedecay-review-diff
description: Review the current PR or diff for impact, risk, and quality via the TraceDecay code graph.
---

# /tracedecay-review-diff

Use `tracedecay:reviewing-changes`.

Review the current diff, or the base ref or PR named in `$ARGUMENTS`. This is
read-only: report actionable defects with locations, concrete failure modes,
evidence, and material coverage limits. Use `tracedecay:assessing-impact` when
the user asks to select or execute verification.
