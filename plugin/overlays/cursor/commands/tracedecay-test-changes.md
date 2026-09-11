---
name: tracedecay-test-changes
description: Test current changes with relevant checks and map failures back to source.
---

# /tracedecay-test-changes

Use `tracedecay:assessing-impact`.

- **Args:** interpret `$ARGUMENTS` as explicit changed paths; if absent, use the current working tree.
- Select structural candidates with `tracedecay_affected`; `tracedecay_run_affected_tests` executes the supported Rust selection. Diagnostics reads do not run the toolchain. Respect Cursor approval/run-mode for execution, confirm a nonzero executed count, and supplement with relevant native or host checks when graph selection misses the behavior.

Output: pass/fail summary, failing-symbol mapping, and suggested missing tests.
