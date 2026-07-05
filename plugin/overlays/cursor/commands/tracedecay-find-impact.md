---
name: tracedecay-find-impact
description: Find the blast radius of a change, including impacted symbols, files, and the tests to run.
---

# /tracedecay-find-impact

Use `tracedecay:assessing-impact`.

- **Args:** interpret `$ARGUMENTS` as the symbol, file, or change to analyze; if absent, use the current working-tree diff.
- Read-only: shallow `max_depth` first. Identify impact; do not run tests.

Output: impacted symbols + files, the test set to run, and any hub/coupling risk.
