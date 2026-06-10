---
name: tokensave-impact
description: Determine the blast radius of a change — impacted symbols, files, and the tests to run.
disable-model-invocation: true
---

# /tokensave-impact

Apply the `tokensave:finding-impacted-areas` skill.

- **Args:** interpret the text after the command as the symbol, file, or change to analyze; if absent, use the current working-tree diff.
- Follow that skill's read-only workflow and guardrails (shallow `max_depth` first; it identifies impact, it does not run tests).

Output: impacted symbols + files, the test set to run, and any hub/coupling risk.
