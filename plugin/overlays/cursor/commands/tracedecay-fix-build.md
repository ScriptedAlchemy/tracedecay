---
description: Fix build and type errors by running or parsing diagnostics, mapping them to symbols with callers, then fixing.
---

# /tracedecay-fix-build

Apply the `tracedecay:fixing-build-and-type-errors` skill.

- **Args:** if `$ARGUMENTS` contains pasted `cargo`/`clippy` output, route it to `tracedecay_diagnose`; otherwise run `tracedecay_diagnostics` (scoped to a directory if one was given).
- Follow that skill's guardrails: prefer pasted output when available; `tracedecay_diagnostics` runs the toolchain, so respect Cursor approval/run-mode.

Output: grouped diagnostics with enclosing symbols + callers, the applied fix, and a clean re-check.
