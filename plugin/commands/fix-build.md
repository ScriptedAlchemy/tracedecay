---
description: Fix build and type errors by running or parsing diagnostics, mapping them to symbols with callers, then fixing.
---

# Fix build

Interpret `$ARGUMENTS`: if it contains pasted `cargo`/`clippy`/`rustc` output, route it to `tracedecay_diagnose`; otherwise run `tracedecay_diagnostics` (scoped to a directory if one was given). Prefer pasted output when available.

1. Already have raw output → `tracedecay_diagnose` (`cargo_output` required, optional `severity`, `include_callers`, `max_diagnostics`): each diagnostic maps to the smallest containing node with up to 5 callers pre-attached. No toolchain run — cheap and safe.
2. Need retained diagnostics → `tracedecay_diagnostics` (`scope`: `workspace` | `file` (needs `path`)): canonical clean-generation errors/warnings, each mapped to the enclosing graph node. Configured producers publish new diagnostics through their owned lifecycle.
3. Inspect the failing code and affected callers; use `tracedecay_impact` when dependent contracts need investigation.
4. Apply the fix, then run the applicable native build/typecheck and relevant behavioral checks. Retained diagnostics alone cannot verify an edit; report any unavailable fresh verification.

Output: grouped diagnostics with enclosing symbols + callers, the applied fix, and a clean re-check.
