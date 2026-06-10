---
name: tokensave-test
description: Run only the tests affected by the current changes and map failures back to source.
disable-model-invocation: true
---

# /tokensave-test

Apply the `tokensave:running-impacted-tests` skill.

- **Args:** interpret the text after the command as explicit changed paths; if absent, use the current working tree.
- Follow that skill's workflow and guardrails (`tokensave_run_affected_tests` and `tokensave_diagnostics` run cargo-backed checks — respect Cursor approval/run-mode; preview scope read-only first).

Output: pass/fail summary, failing-symbol mapping, and suggested missing tests.
