---
name: fixing-build-and-type-errors
description: Diagnose build and type errors that span symbols or files with TraceDecay dependency evidence.
---

# Fixing build and type errors

When compiler output already exists, `diagnose` can map it to symbols without
rerunning the build. Retained diagnostics belong to their clean generation;
reading them does not run producers or refresh stale evidence. Each TypeScript
project (every package `tsconfig.json` in a monorepo, and the configs they
reference) whose package or workspace root has `node_modules/.bin/tsc` is
checked by the daemon after each complete generation, so `diagnostics` is
populated without a paste; any other toolchain publishes through `diagnose`. A
file read reports on the tsconfig that owns the file. A read with
no publication returns a typed problem whose message names the exact next step
(install the compiler, paste output, or retry while the producer runs). Use the
native build check when fresh compiler evidence is required.

Follow the failing contract: signatures to callers, missing fields to constructor
and field sites, trait bounds to implementations, and broken module paths to
file dependents. These graph links narrow investigation; the compiler remains
the authority on whether the fix type-checks. Rust constructor discovery is
best-effort and does not replace compiling affected targets.

LSP server inspection is informational: listing a server neither installs nor
starts it. Preserve unavailable producer states instead of reporting an empty
successful diagnostic result. Verify the root error first, then the affected
behavior rather than repeatedly running a broad build for every dependent error.
