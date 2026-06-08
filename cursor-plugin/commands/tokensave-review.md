---
name: tokensave-review
description: Review the current PR/diff for impact, risk, and quality via the tokensave code graph.
---

# /tokensave-review

Apply the `tokensave:reviewing-a-diff` skill to the current working-tree diff (or to the base ref / PR given in `$ARGUMENTS` if provided).

Steps:
1. Determine changed files (working tree, or `git diff --name-only <base>...HEAD`).
2. Run `tokensave_diff_context` (or `tokensave_pr_context` for a ref-to-ref PR), then `tokensave_impact` + `tokensave_affected` for blast radius and tests, then `tokensave_simplify_scan`, `tokensave_test_risk`, and `tokensave_unsafe_patterns` on the changed files.
3. Do not edit files or run tests.

Output: findings grouped Critical / Warning / Note, impacted areas, and the test set to run. Report any `tokensave_metrics:` savings to the user.
