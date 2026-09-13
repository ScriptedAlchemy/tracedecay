---
name: tracedecay-fix-build
description: Diagnose and fix build or type errors with source and dependency evidence.
---

# /tracedecay-fix-build

Use `tracedecay:fixing-build-and-type-errors`.

Map compiler output in `$ARGUMENTS` with `tracedecay_diagnose`; otherwise inspect
retained diagnostics for the requested scope. Fix the root failing contract and
relevant callers. Retained diagnostics do not verify edits, so respect Cursor
approval and run mode while completing the native build or typecheck and
relevant behavioral checks.
