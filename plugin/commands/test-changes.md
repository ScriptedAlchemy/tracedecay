---
description: Test current changes with relevant checks and map failures back to source.
---

# Test changes

Interpret `$ARGUMENTS` as explicit changed paths. If absent, use the current working tree. Preview scope read-only first, then run.

1. Preview affected tests → `tracedecay_diff_context` (`files`) or `tracedecay_affected` (`files`): the test set that can see the change.
2. Run → `tracedecay_run_affected_tests` (`changed_paths`, `max_tests`, `profile`, `timeout_secs`): pass/fail per test, with the source nodes each test covers. Cargo-backed — respect approval/run-mode.
3. On compile/type failure → `tracedecay_diagnose` for captured cargo stderr. `tracedecay_diagnostics` reads retained diagnostics; it does not run a fresh check.
4. Where the next test goes → `tracedecay_test_risk` (`path`, `limit`): prioritized coverage gaps.

Coverage is structural (call/use edges): integration tests and external consumers may be missed. An empty selection proves neither that code is untested nor that verification passed. Confirm a nonzero executed count and supplement with the relevant native or host journey when structural selection misses it.

Output: pass/fail summary, failing-symbol mapping, and suggested missing tests.
