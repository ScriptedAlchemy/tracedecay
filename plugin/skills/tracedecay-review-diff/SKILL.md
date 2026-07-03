---
name: tracedecay-review-diff
description: 'Use to review the current PR or diff for impact, risk, and quality via the TraceDecay code graph.'
---

# Review diff

Use to review the current PR or diff for impact, risk, and quality.

Use `tracedecay:reviewing-changes`.

- **Scope:** the current working-tree diff, or the base ref / PR named if one is given.
- Read-only: no edits or test runs. To verify behavior, hand off to `tracedecay:assessing-impact`.

Output: findings grouped Critical / Warning / Note, the impacted areas, and the test set to run.
