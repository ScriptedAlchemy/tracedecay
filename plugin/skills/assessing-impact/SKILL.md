---
name: assessing-impact
description: Use when the user asks what breaks, what to re-test, or which tests cover code before changing a function, signature, type, field, or file. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Assessing impact

Connect a proposed change to dependents and tests. For specified files,
`diff_context` is prospective and analyzes every indexed symbol in those files.
Use `pr_context` for exact committed changes or `commit_context` for exact
staged/net HEAD-to-worktree declarations before tracing their consumers. For one
known symbol, start with shallow `impact` and widen when its dependents warrant
it. Keep the returned node identity for follow-up; resolve an ambiguous name
before interpreting its consumers.

`test_map`, `affected`, and `test_risk` provide structural test evidence. A graph
edge is not executed coverage: subprocesses, I/O, configuration, fixture loading,
macros, and external consumers can be absent. Supplement the selected tests with
the actual host or integration journey that exercises those boundaries. Zero
indexed callers does not establish that a public API is unused.

Use `affected` with `files` for a fresh file-based test selection. The similarly
named `affected_tests` reads a completed feedback cycle's selection and requires
its server-minted `request_handle`.

`run_affected_tests` runs the supported Rust selection; other languages use their
native runner. Check the reported selection and nonzero executed count. A passing
selection supports only the behavior it exercised, not every possible consumer.

**Report:** the dependents and the tests to run, each with its file, plus the
freshness line when it is not `fresh` and the consumers the graph cannot see. Do
not call a caller safe because the index shows no edge to it.
