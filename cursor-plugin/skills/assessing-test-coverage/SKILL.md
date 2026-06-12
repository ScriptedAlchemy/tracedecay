---
name: assessing-test-coverage
description: Use when answering which tests cover code, whether a function or file is tested, which tests are affected by changed files, or where the next test should go without running tests.
---

# Assessing test coverage

Read-only coverage intelligence from the graph (structural test↔source edges, not line coverage). To actually run the selected tests, hand off to `tokensave:running-impacted-tests`.

## Workflow

1. **Symbol/file → its tests → `tokensave_test_map`** (`file` or `node_id`): the direct coverage edges; an empty result means no test reaches it through the indexed graph.
2. **Changed files → affected tests → `tokensave_affected`** (`files`): dependency-graph traversal to every test file that can see the change.
3. **Where the next test goes → `tokensave_test_risk`** (`path?`, `limit?`): risk = (complexity + 1) × (fan_in + 1) × untested-multiplier — the prioritized gap list.
4. **Judging a diff's coverage → reuse `tokensave_diff_context`** (`files`): it already bundles modified symbols + dependents + affected tests in one call — prefer it over separate lookups.

## Guardrails

- All read-only and parallel-safe; nothing here executes tests. Coverage is structural (call/use edges), so integration tests that reach code indirectly (through a binary, fixture, or IO boundary) can be missed — an empty `test_map` is strong but not absolute evidence of "untested".

## Output

- The tests covering the target, the affected-test set for a change, and the ranked coverage gaps with a recommendation for the next test.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
