---
name: tracedecay-review-diff
description: 'Use to review the current PR or diff for impact, risk, and quality via the TraceDecay code graph.'
---

# Review diff

Use when asked to review the current PR or diff for impact, risk, and quality.

Route this through the `tracedecay:reviewing-changes` skill.

- **Scope:** the current working-tree diff, or the base ref / PR named if one is given.
- Follow that skill's read-only workflow and guardrails: no edits or test runs; to verify behavior, hand off to `tracedecay:assessing-impact`.

Output: findings grouped Critical / Warning / Note, the impacted areas, and the test set to run.
