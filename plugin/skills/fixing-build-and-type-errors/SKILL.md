---
name: fixing-build-and-type-errors
description: Use when a build, compile, or type check fails and the error spans several symbols or files. Skip when one compiler message already names the line to fix. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Fixing build and type errors

Follow these steps in order. Report a step you skip as `skip: <reason>`.

1. Map the errors to symbols: `tracedecay_diagnose` for compiler output you
   already have, `tracedecay_diagnostics` for retained diagnostics. Read
   [diagnostics sources](references/diagnostics-sources.md) when a read
   returns a typed problem instead of diagnostics.
2. Pick the root error, not the dependent errors it causes.
3. Follow its failing contract: signatures to callers, missing fields to
   constructor and field sites, trait bounds to implementations, and broken
   module paths to file dependents. Graph links narrow the search; Rust
   constructor discovery is best-effort.
4. Apply the smallest complete fix.
5. Run the native build or typecheck for the affected targets. The compiler,
   not the graph or retained diagnostics, decides whether the fix type-checks.
   skip: no toolchain can run here; say so.
6. Repeat from step 2 only for errors the fresh run still reports, rather than
   rebuilding broadly for every dependent error.

**Report:** the root error, the fix, and the fresh native build or typecheck
that passed, or why none could run. Do not cite retained diagnostics as
verification.
