---
description: Review the current PR or diff for impact, risk, and quality via the TraceDecay code graph.
---

# /tracedecay-review-diff

Apply the `tracedecay:reviewing-changes` skill.

- **Scope:** the current working-tree diff, or the base ref / PR named in `$ARGUMENTS` if one was given.
- Follow that skill's read-only workflow and guardrails (no edits or test runs; to verify behavior, hand off to `tracedecay:assessing-impact`).

Output: findings grouped Critical / Warning / Note, the impacted areas, and the test set to run.
