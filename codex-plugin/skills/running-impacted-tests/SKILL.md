---
name: running-impacted-tests
description: Use when running only tests affected by changed Rust files, mapping failures back to source, verifying a change without a full suite, or handling cargo-backed impacted-test checks.
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
---

# Running impacted tests

Use this when impacted-test verification is relevant to the task. Respect Cursor approval/run-mode before invoking cargo-backed tools.

## Workflow

1. **Changed paths** — working tree, or explicit `changed_paths`.
2. **Preview coverage:** `tracedecay_affected` (`files`) and `tracedecay_test_map` (which tests cover the changes).
3. **Run → `tracedecay_run_affected_tests`** (`changed_paths`, `max_tests`, `profile`, `timeout_secs`): pass/fail per test, with the source nodes each test covers.
4. **On compile/type failure → `tracedecay_diagnose`** for captured cargo stderr, or `tracedecay_diagnostics` when fresh structured diagnostics are needed. For standalone compile/type errors, use `tracedecay:fixing-build-and-type-errors`.
5. **Coverage gaps → `tracedecay_test_risk`** to recommend where the next test goes.

## Guardrails

- `tracedecay_run_affected_tests` and `tracedecay_diagnostics` run cargo-backed checks, and the first `diagnostics` build can take minutes (forced target dir `.tracedecay/target/`). Respect Cursor approval/run-mode and avoid duplicate runs.
- `tracedecay_run_affected_tests` is cargo-only. For non-Rust repos use `tracedecay_diagnostics` (tsc/pyright) and the project's own test runner.
- Steps 1–2 and 5 are read-only and safe to run first to preview scope before the user commits to a run.
- Pure coverage questions ("which tests cover X", "is this tested", "where should the next test go") that don't need a run → `tracedecay:assessing-test-coverage`.

## Output

- Pass/fail summary + failing-symbol mapping + suggested missing tests.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
