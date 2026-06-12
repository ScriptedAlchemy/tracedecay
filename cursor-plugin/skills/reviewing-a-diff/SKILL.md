---
name: reviewing-a-diff
description: Use when reviewing a PR, branch diff, or working-tree diff for impact, behavioral risk, quality issues, unsafe patterns, affected tests, or pre-merge readiness.
---

# Reviewing a diff

## Workflow

1. **Get changed files** — working tree, or `git diff --name-only <base>...HEAD` (default base `main`).
2. **Semantic change summary:**
   - Working tree / file list → `tokensave_diff_context` (`files`): modified symbols + dependents + affected tests.
   - Ref-to-ref PR → `tokensave_pr_context` (`base_ref`, `head_ref`).
3. **Go deeper only if needed:** `tokensave_diff_context` already returns dependents + affected tests — reuse those first. Use **`tokensave_impact`** (`node_id`) to widen the blast radius on a specific high-risk changed symbol, and **`tokensave_affected`** (`files`) only when you need the full test set beyond what step 2 surfaced.
4. **Quality scan of just the changed files → `tokensave_simplify_scan`** (`files`): duplications, dead code, coupling, complexity hotspots.
5. **Risk surfacing:** `tokensave_test_risk` on changed paths; `tokensave_unsafe_patterns` on changed files (unwrap/expect/panic/unsafe).

## Guardrails

- Read-only review. Do not edit or run tests from this skill; to verify behavior, hand off to the `tokensave:running-impacted-tests` skill.
- If diff context is truncated and includes a `handle`, narrow by file/symbol first when possible; call `tokensave_retrieve` with that `handle` only when the omitted risk detail is needed.

## Output

- Findings grouped **Critical / Warning / Note**, the impacted areas, and the test set to run.
- Pairs with the `pr-review-canvas` plugin if installed.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
