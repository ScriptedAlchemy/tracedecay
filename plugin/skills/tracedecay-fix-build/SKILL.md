---
name: tracedecay-fix-build
description: 'Use to fix build and type errors by running or parsing diagnostics, mapping them to symbols with callers, then fixing.'
---

# Fix build

Use to fix build and type errors by running or parsing diagnostics, mapping them to symbols with callers, then fixing.

Use `tracedecay:fixing-build-and-type-errors`.

- **Input:** if the user pasted `cargo`/`clippy` output, route it to `tracedecay_diagnose`; otherwise run `tracedecay_diagnostics` (scoped to a directory if one is named).
- Prefer pasted output when available. `tracedecay_diagnostics` runs the toolchain, so confirm before long checks.

Output: grouped diagnostics with enclosing symbols + callers, the applied fix, and a clean re-check.
