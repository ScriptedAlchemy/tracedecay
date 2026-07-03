---
name: tracedecay-find-impact
description: 'Use to find the blast radius of a change, including impacted symbols, files, and the tests to run.'
---

# Find impact

Use to find a change's blast radius: impacted symbols, files, and tests to run.

Use `tracedecay:assessing-impact`.

- **Target:** the symbol, file, or change to analyze. If none is given, use the current working-tree diff.
- Read-only: shallow `max_depth` first. Identify impact; do not run tests.

Output: impacted symbols + files, the test set to run, and any hub/coupling risk.
