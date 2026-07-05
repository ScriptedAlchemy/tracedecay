---
name: tracedecay-fix-build
description: Fix build and type errors by running or parsing diagnostics, mapping them to symbols with callers, then fixing.
---

# /tracedecay-fix-build

Use `tracedecay:fixing-build-and-type-errors`.

- **Args:** if `$ARGUMENTS` contains pasted `cargo`/`clippy` output, route it to `tracedecay_diagnose`; otherwise run `tracedecay_diagnostics` (scoped to a directory if one was given).
- Prefer pasted output when available. `tracedecay_diagnostics` runs the toolchain, so respect Cursor approval/run-mode.

Output: grouped diagnostics with enclosing symbols + callers, the applied fix, and a clean re-check.
