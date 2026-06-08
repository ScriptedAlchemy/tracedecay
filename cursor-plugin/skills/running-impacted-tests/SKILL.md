---
name: running-impacted-tests
description: Run only the tests affected by your changes and map failures back to source. Use for "run the affected tests", "run impacted tests", or "tokensave test" to verify a change without a full test run. User-triggered so it never runs the toolchain automatically. (Rust/cargo projects.)
disable-model-invocation: true
---

# Running impacted tests

This skill runs the toolchain, so it is **user-triggered only** (`disable-model-invocation: true`). It will not auto-run `tokensave_run_affected_tests`.

## Workflow

1. **Changed paths** — working tree, or explicit `changed_paths`.
2. **Preview coverage:** `tokensave_affected` (`files`) and `tokensave_test_map` (which tests cover the changes).
3. **Run → `tokensave_run_affected_tests`** (`changed_paths`, `max_tests`, `profile`, `timeout_secs`): pass/fail per test, with the source nodes each test covers.
4. **On failure → `tokensave_diagnostics`** (structured errors) or **`tokensave_diagnose`** (paste raw `cargo check` / `clippy` stderr → mapped to nodes with callers attached).
5. **Coverage gaps → `tokensave_test_risk`** to recommend where the next test goes.

## Guardrails

- `tokensave_run_affected_tests` and `tokensave_diagnostics` **mutate / run the toolchain**: they invoke `cargo`, and the first `diagnostics` build can take minutes (forced target dir `.tokensave/target/`). Only run after the user explicitly asks; respect Cursor approval/run-mode.
- `tokensave_run_affected_tests` is cargo-only. For non-Rust repos use `tokensave_diagnostics` (tsc/pyright) and the project's own test runner.
- Steps 1–2 and 5 are read-only and safe to run first to preview scope before the user commits to a run.

## Output

- Pass/fail summary + failing-symbol mapping + suggested missing tests.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
