---
description: Find the blast radius of a change, including impacted symbols, files, and the tests to run.
argument-hint: "[symbol | path]"
---

# Find impact

Interpret `$ARGUMENTS` as the symbol, file, or change to analyze. If absent, use the current working-tree diff. This identifies impact read-only; it does not run tests.

1. Resolve the target to a node ID with `tracedecay_search` / `tracedecay_find_exact_symbol` / `tracedecay_by_qualified_name`.
2. Symbol blast radius → `tracedecay_impact` (`node_id`, small `max_depth` first, widen only if incomplete): direct + transitive dependents.
3. File-level fan-in → `tracedecay_file_dependents`; already have changed paths → `tracedecay_diff_context` (`files`): modified symbols + dependents + affected tests in one call.
4. Test set → `tracedecay_affected` (`files`) for every test that can see the change; `tracedecay_test_map` for direct coverage of one symbol/file.
5. Structural fragility (optional) → `tracedecay_coupling` / `tracedecay_dependency_depth` to see if the target is a high-fan-in hub.

Output: impacted symbols + files, the test set to run, and any hub/coupling risk. If any result includes a `tracedecay_metrics:` line, report the savings.
