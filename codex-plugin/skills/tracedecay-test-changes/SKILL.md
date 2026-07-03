---
name: tracedecay-test-changes
description: 'Use to test current changes by running only affected tests and mapping failures back to source.'
---

# Test changes

Use when asked to test current changes by running only the affected tests and mapping failures back to source.

Route this through the `tracedecay:assessing-impact` skill, using the affected-tests tools (`tracedecay_run_affected_tests`, `tracedecay_diagnostics`).

- **Input:** explicit changed paths if given; otherwise use the current working tree.
- Follow that skill's workflow and guardrails: `tracedecay_run_affected_tests` and `tracedecay_diagnostics` run cargo-backed checks, so confirm before running; preview scope read-only first.

Output: pass/fail summary, failing-symbol mapping, and suggested missing tests.
