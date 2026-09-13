---
name: tracedecay-test-changes
description: Test current changes with relevant checks and map failures back to source.
---

# /tracedecay-test-changes

Use `tracedecay:assessing-impact`.

Interpret `$ARGUMENTS` as changed paths; if absent, use the working tree. Select
structural candidates, then respect Cursor approval and run mode while executing
the supported Rust tests. Confirm a nonzero count. Add relevant native or host
checks when graph selection misses integration, configuration, I/O, generated,
or external behavior. Report results, mapped failures, and material coverage
gaps.
