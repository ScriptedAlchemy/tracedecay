---
name: tokensave-review
description: Review the current PR/diff for impact, risk, and quality via the tokensave code graph.
disable-model-invocation: true
---

# /tokensave-review

Apply the `tokensave:reviewing-a-diff` skill.

- **Scope:** the current working-tree diff, or the base ref / PR named after the command if one was given.
- Follow that skill's read-only workflow and guardrails (no edits or test runs; to verify behavior, hand off to `tokensave:running-impacted-tests`).

Output: findings grouped Critical / Warning / Note, the impacted areas, and the test set to run.
