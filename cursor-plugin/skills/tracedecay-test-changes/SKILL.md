---
name: tracedecay-test-changes
description: 'Use to test current changes by running only affected tests and mapping failures back to source.'
disable-model-invocation: true
---

# /tracedecay-test-changes

Apply the `tracedecay:running-impacted-tests` skill.

- **Args:** interpret the text after the command as explicit changed paths; if absent, use the current working tree.
- Follow that skill's workflow and guardrails (`tracedecay_run_affected_tests` and `tracedecay_diagnostics` run cargo-backed checks — respect Cursor approval/run-mode; preview scope read-only first).

Output: pass/fail summary, failing-symbol mapping, and suggested missing tests.
