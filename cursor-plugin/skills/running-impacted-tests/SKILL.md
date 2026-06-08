---
name: running-impacted-tests
description: Run only the tests affected by your changes and map failures back to source. Use for "run the affected tests", "run impacted tests", or "tokensave test" to verify a change without a full test run. (Rust/cargo projects.)
---

# Running impacted tests

Use this when impacted-test verification is relevant to the task. Respect Cursor approval/run-mode before invoking cargo-backed tools.

## Workflow

1. **Changed paths** — working tree, or explicit `changed_paths`.
2. **Preview coverage:** `tokensave_affected` (`files`) and `tokensave_test_map` (which tests cover the changes).
3. **Run → `tokensave_run_affected_tests`** (`changed_paths`, `max_tests`, `profile`, `timeout_secs`): pass/fail per test, with the source nodes each test covers.
4. **On compile/type failure → `tokensave_diagnose`** for captured cargo stderr, or `tokensave_diagnostics` when fresh structured diagnostics are needed. For standalone compile/type errors, use `tokensave:fixing-build-and-type-errors`.
5. **Coverage gaps → `tokensave_test_risk`** to recommend where the next test goes.

## Guardrails

- `tokensave_run_affected_tests` and `tokensave_diagnostics` run cargo-backed checks, and the first `diagnostics` build can take minutes (forced target dir `.tokensave/target/`). Respect Cursor approval/run-mode and avoid duplicate runs.
- `tokensave_run_affected_tests` is cargo-only. For non-Rust repos use `tokensave_diagnostics` (tsc/pyright) and the project's own test runner.
- Steps 1–2 and 5 are read-only and safe to run first to preview scope before the user commits to a run.

## Output

- Pass/fail summary + failing-symbol mapping + suggested missing tests.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
