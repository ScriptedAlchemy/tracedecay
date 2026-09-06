---
name: assessing-impact
description: 'What breaks if a symbol, signature, or set of files changes, and which tests to re-run. Covers dependents versus executed coverage, when a passing affected-test selection counts as evidence, and diff-scoped versus symbol-scoped impact. Call relationships alone are tracing-functions; diff review is reviewing-changes.'
---

# Assessing impact

Use changed-symbol context to connect a proposed change to dependents and tests.
For a working-tree change, `diff_context` composes that evidence across files;
for one symbol, start with shallow `impact` and widen when its dependents warrant it.
Resolve ambiguous symbols before interpreting the result.

`test_map`, `affected`, and `test_risk` provide structural test evidence. A graph
edge is not executed coverage: subprocesses, I/O, configuration, fixture loading,
macros, and external consumers can be absent. Supplement the selected tests with
the actual host or integration journey that exercises those boundaries. Zero
indexed callers does not establish that a public API is unused.

`run_affected_tests` runs the supported Rust selection; other languages use their
native runner. Check the reported selection and nonzero executed count. A passing
selection supports only the behavior it exercised, not every possible consumer.
