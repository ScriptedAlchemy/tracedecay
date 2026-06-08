---
name: finding-impacted-areas
description: Determine the blast radius of a change — every symbol/file that depends on a target and which tests to run. Use for "what breaks if I change X", impact/regression analysis, pre-refactor safety checks, or judging the risk of an edit.
---

# Finding impacted areas

## Workflow

1. **Resolve the target → node ID** with `tokensave_search` / `tokensave_find_exact_symbol` / `tokensave_by_qualified_name`.
2. **Symbol blast radius → `tokensave_impact`** (`node_id`, small `max_depth` first, widen if needed): all direct + transitive dependents.
3. **File-level fan-in → `tokensave_file_dependents`** (every file that imports the changed file).
4. **Already have changed paths → `tokensave_diff_context`** (`files`): modified symbols + dependents + affected tests in one call.
5. **Tests to run:**
   - `tokensave_affected` (`files`) → affected test files via dependency traversal.
   - `tokensave_test_map` (`file` / `node_id`) → which tests cover the target.
   - `tokensave_test_risk` → high-risk, weakly-tested dependents (where coverage gaps bite).
6. **Structural fragility (optional):** `tokensave_coupling` / `tokensave_dependency_depth` to see if the target is a high-fan-in hub.

## Guardrails

- Read-only analysis. This skill identifies impact and the test set; it does **not** run tests.
- Start with a shallow `max_depth` and widen only when the picture is incomplete.

## Handoff

- To actually run the tests, use the `tokensave:running-impacted-tests` skill (user-triggered; it runs cargo).

## Output

- (a) impacted symbols + files, (b) the test set to run, (c) any hub/coupling risk.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
