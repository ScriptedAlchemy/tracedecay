---
name: assessing-test-coverage
description: Map which tests cover which code and where coverage is missing — without running anything. Use for "which tests cover X", "is this function tested", "what tests should I run for these files", or "where should the next test go".
---

# Assessing test coverage

Read-only coverage intelligence from the graph (structural test↔source edges, not line coverage). To actually run the selected tests, hand off to `tracedecay:running-impacted-tests`.

## Workflow

1. **Symbol/file → its tests → `tracedecay_test_map`** (`file` or `node_id`): the direct coverage edges; an empty result means no test reaches it through the indexed graph.
2. **Changed files → affected tests → `tracedecay_affected`** (`files`): dependency-graph traversal to every test file that can see the change.
3. **Where the next test goes → `tracedecay_test_risk`** (`path?`, `limit?`): risk = (complexity + 1) × (fan_in + 1) × untested-multiplier — the prioritized gap list.
4. **Judging a diff's coverage → reuse `tracedecay_diff_context`** (`files`): it already bundles modified symbols + dependents + affected tests in one call — prefer it over separate lookups.

## Guardrails

- All read-only and parallel-safe; nothing here executes tests. Coverage is structural (call/use edges), so integration tests that reach code indirectly (through a binary, fixture, or IO boundary) can be missed — an empty `test_map` is strong but not absolute evidence of "untested".

## Output

- The tests covering the target, the affected-test set for a change, and the ranked coverage gaps with a recommendation for the next test.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
