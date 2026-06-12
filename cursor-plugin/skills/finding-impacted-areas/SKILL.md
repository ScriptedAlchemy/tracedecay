---
name: finding-impacted-areas
description: Use when estimating blast radius: what depends on a symbol or file, affected tests, risk of a change, impacted areas for a refactor, or what could break if a target changes.
---

# Finding impacted areas

## Workflow

1. **Resolve the target → node ID** with `tracedecay_search` / `tracedecay_find_exact_symbol` / `tracedecay_by_qualified_name` (see `tracedecay:searching-for-code` for the full resolver ladder).
2. **Symbol blast radius → `tracedecay_impact`** (`node_id`, small `max_depth` first, widen if needed): all direct + transitive dependents.
3. **File-level fan-in → `tracedecay_file_dependents`** (every file that imports the changed file).
4. **Already have changed paths → `tracedecay_diff_context`** (`files`): modified symbols + dependents + affected tests in one call.
5. **Tests to run:**
   - Reuse `tracedecay_diff_context` affected tests first.
   - Need more detail? `tracedecay_affected` (`files`) finds affected tests; `tracedecay_test_map` (`file` / `node_id`) shows direct coverage.
   - Use `tracedecay_test_risk` for high-risk, weakly-tested dependents.
6. **Structural fragility (optional):** `tracedecay_coupling` / `tracedecay_dependency_depth` to see if the target is a high-fan-in hub.

## Guardrails

- Read-only analysis. This skill identifies impact and the test set; it does **not** run tests.
- Start with a shallow `max_depth` and widen only when the picture is incomplete.

## Handoff

- To run the selected tests, use the `tracedecay:running-impacted-tests` skill.
- For deeper read-only coverage questions, `tracedecay:assessing-test-coverage`; for a mechanical refactor (rename, signature/field change) where impact analysis becomes an edit checklist, `tracedecay:refactoring-safely`.

## Output

- (a) impacted symbols + files, (b) the test set to run, (c) any hub/coupling risk.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
