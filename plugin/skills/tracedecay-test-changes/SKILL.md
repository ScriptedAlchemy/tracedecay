---
name: tracedecay-test-changes
description: 'Use to test current changes by running only affected tests and mapping failures back to source.'
---

# Test changes

Use to test current changes by running only affected tests and mapping failures back to source.

Use `tracedecay:assessing-impact` with `tracedecay_run_affected_tests` and `tracedecay_diagnostics`.

- **Input:** explicit changed paths if given; otherwise use the current working tree.
- Preview scope read-only first. `tracedecay_run_affected_tests` and `tracedecay_diagnostics` run cargo-backed checks, so confirm before running.

Output: pass/fail summary, failing-symbol mapping, and suggested missing tests.
