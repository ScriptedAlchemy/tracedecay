---
name: tracedecay-find-impact
description: Find the blast radius of a change, including impacted symbols, files, and the tests to run.
---

# /tracedecay-find-impact

Use `tracedecay:assessing-impact`.

Interpret `$ARGUMENTS` as a symbol, file, or change; if absent, use the current
diff. This is read-only. Begin with shallow impact and widen only when returned
dependents or the question require it. Report affected symbols and files,
candidate tests, coverage limits, and material hub or coupling risk.
