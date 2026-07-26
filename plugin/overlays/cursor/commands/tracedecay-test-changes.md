---
name: tracedecay-test-changes
description: Test current changes by running only affected tests and mapping failures back to source.
---

# /tracedecay-test-changes

Use `tracedecay:assessing-impact`.

- **Args:** interpret `$ARGUMENTS` as explicit changed paths; if absent, use the current working tree.
- Preview scope read-only first with `tracedecay_affected` /
  `tracedecay_test_results`. `tracedecay_run_affected_tests`,
  `tracedecay_diagnostics`, and `tracedecay_diagnostics_read` run
  cargo-backed checks; respect Cursor approval/run-mode.

Output: pass/fail summary, failing-symbol mapping, and suggested missing tests.
