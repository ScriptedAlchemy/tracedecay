---
description: Find the blast radius of a change, including impacted symbols, files, and the tests to run.
argument-hint: "[symbol | path]"
---

# Find impact

Interpret `$ARGUMENTS` as a symbol, file, or proposed change; if absent, use the
working-tree diff. Follow the bundled `assessing-impact` skill. Resolve exact
symbols, begin with shallow impact, and widen only when the question or returned
dependents require it. Use diff context for multiple changed paths and structural
test evidence for the relevant verification candidates.

This command is read-only and does not run tests. Report impacted symbols and
files, candidate tests, coverage limits, and material hub or coupling risk.
