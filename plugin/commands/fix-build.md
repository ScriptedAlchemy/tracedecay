---
description: Diagnose and fix build or type errors with source and dependency evidence.
---

# Fix build

Follow the bundled `fixing-build-and-type-errors` skill. If `$ARGUMENTS`
contains compiler output, map it with `tracedecay_diagnose`; otherwise use
retained diagnostics for the requested file or workspace. Inspect the root
diagnostic, failing contract, and relevant callers before editing.

Apply the smallest complete fix, then run the applicable native build or
typecheck and relevant behavioral checks. Retained diagnostics alone do not
verify an edit. Report the fix and fresh verification, or the exact reason fresh
verification was unavailable.
