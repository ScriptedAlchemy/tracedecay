---
description: Review the current PR or diff for impact, risk, and quality via the TraceDecay code graph.
---

# Review diff

Review the current working-tree diff, or the base ref / PR named in `$ARGUMENTS` if one was given. Read-only: no edits or test runs.

1. Get changed files — working tree, or `git diff --name-only <base>...HEAD` (use the repository's configured integration base).
2. Semantic change summary: working tree / file list → `tracedecay_diff_context` (`files`): modified symbols + dependents + affected tests; ref-to-ref PR → `tracedecay_pr_context` (`base_ref`, `head_ref`).
3. Go deeper only if needed: `tracedecay_impact` (`node_id`) to widen the blast radius on a high-risk changed symbol; `tracedecay_affected` (`files`) only when the test set is not enough.
4. When quality risk warrants a scan, use `tracedecay_redundancy`, `tracedecay_dead_code`, `tracedecay_coupling`, and `tracedecay_complexity` on the changed paths.
5. For unresolved coverage or unsafe-code risk: `tracedecay_test_risk` on changed paths; `tracedecay_unsafe_patterns` on changed files.

To verify behavior, hand off to `/tracedecay:test-changes`.

Output: findings grouped Critical / Warning / Note, the impacted areas, and the test set to run.
