---
description: Test current changes with relevant checks and map failures back to source.
---

# Test changes

Interpret `$ARGUMENTS` as changed paths; if absent, use the working tree. Follow
the bundled `assessing-impact` skill to select relevant tests, then execute the
supported Rust selection with `tracedecay_run_affected_tests`. Respect the
host's approval and run mode. Map captured compiler failures with
`tracedecay_diagnose`; retained diagnostics do not run a fresh check.

Confirm that the selection executed a nonzero test count. Structural coverage
can miss integration tests, configuration, I/O, generated code, and external
consumers, so add the native or host journey that exercises those boundaries
when relevant. Report results, mapped failures, and material coverage gaps.
