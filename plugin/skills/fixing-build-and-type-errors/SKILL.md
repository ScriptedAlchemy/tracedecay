---
name: fixing-build-and-type-errors
description: 'Interpret compiler diagnostics with TraceDecay symbol and dependency evidence, especially signature, field, trait, or module failures spanning files.'
---

# Fixing build and type errors

When compiler output already exists, `diagnose` can map it to symbols without
rerunning the build. Retained diagnostics belong to their clean generation;
reading them does not run producers or refresh stale evidence. Use the native
build check when fresh compiler evidence is required.

Follow the failing contract: signatures to callers, missing fields to constructor
and field sites, trait bounds to implementations, and broken module paths to
file dependents. These graph links narrow investigation; the compiler remains
the authority on whether the fix type-checks. Rust constructor discovery is
best-effort and does not replace compiling affected targets.

LSP server inspection is informational: listing a server neither installs nor
starts it. Preserve unavailable producer states instead of reporting an empty
successful diagnostic result. Verify the root error first, then the affected
behavior rather than repeatedly running a broad build for every dependent error.
