---
name: reviewing-a-diff
description: Review a PR or working-tree diff for impact, risk, and quality using the code graph. Use for "review this diff", "review this PR", "tokensave review", change-set risk review, or pre-merge checks.
disable-model-invocation: true
---

# Reviewing a diff

## Workflow

1. **Get changed files** — working tree, or `git diff --name-only <base>...HEAD` (default base `main`).
2. **Semantic change summary:**
   - Working tree / file list → `tokensave_diff_context` (`files`): modified symbols + dependents + affected tests.
   - Ref-to-ref PR → `tokensave_pr_context` (`base_ref`, `head_ref`).
3. **Blast radius → `tokensave_impact`** on the key changed symbols; **`tokensave_affected`** (`files`) for the test set.
4. **Quality scan of just the changed files → `tokensave_simplify_scan`** (`files`): duplications, dead code, coupling, complexity hotspots.
5. **Risk surfacing:** `tokensave_test_risk` on changed paths; `tokensave_unsafe_patterns` on changed files (unwrap/expect/panic/unsafe).

## Guardrails

- Read-only review. Do not edit or run tests from this skill; to verify behavior, hand off to the `tokensave:running-impacted-tests` skill (user-triggered).

## Output

- Findings grouped **Critical / Warning / Note**, the impacted areas, and the test set to run.
- Pairs with the `pr-review-canvas` plugin if installed.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
