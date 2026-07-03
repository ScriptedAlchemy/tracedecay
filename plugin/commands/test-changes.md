---
description: Test current changes by running only affected tests and mapping failures back to source.
---

# Test changes

Interpret `$ARGUMENTS` as explicit changed paths. If absent, use the current working tree. Preview scope read-only first, then run.

1. Preview affected tests → `tracedecay_diff_context` (`files`) or `tracedecay_affected` (`files`): the test set that can see the change.
2. Run → `tracedecay_run_affected_tests` (`changed_paths`, `max_tests`, `profile`, `timeout_secs`): pass/fail per test, with the source nodes each test covers. Cargo-backed — respect approval/run-mode.
3. On compile/type failure → `tracedecay_diagnose` for captured cargo stderr, or run `tracedecay_diagnostics`.
4. Where the next test goes → `tracedecay_test_risk` (`path`, `limit`): prioritized coverage gaps.

Coverage is structural (call/use edges): integration tests that reach code indirectly can be missed, so an empty result is strong but not absolute evidence of "untested".

Output: pass/fail summary, failing-symbol mapping, and suggested missing tests. If any result includes a `tracedecay_metrics:` line, report the savings.
