---
name: tracedecay-find-impact
description: 'Use to find the blast radius of a change, including impacted symbols, files, and the tests to run.'
---

# Find impact

Use when asked to find the blast radius of a change, including impacted symbols, files, and the tests to run.

Route this through the `tracedecay:assessing-impact` skill.

- **Target:** the symbol, file, or change to analyze. If none is given, use the current working-tree diff.
- Follow that skill's read-only workflow and guardrails: shallow `max_depth` first; it identifies impact, it does not run tests.

Output: impacted symbols + files, the test set to run, and any hub/coupling risk.
