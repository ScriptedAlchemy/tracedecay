---
description: Find the blast radius of a change, including impacted symbols, files, and the tests to run.
---

# /tracedecay-find-impact

Apply the `tracedecay:assessing-impact` skill.

- **Args:** interpret `$ARGUMENTS` as the symbol, file, or change to analyze; if absent, use the current working-tree diff.
- Follow that skill's read-only workflow and guardrails (shallow `max_depth` first; it identifies impact, it does not run tests).

Output: impacted symbols + files, the test set to run, and any hub/coupling risk.
