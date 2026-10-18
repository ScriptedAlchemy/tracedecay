# Where diagnostics come from

`tracedecay_diagnose` maps compiler output you already have to symbols without
rerunning the build. Retained diagnostics (`tracedecay_diagnostics`) belong to
their clean generation; reading them does not run producers or refresh stale
evidence.

The daemon checks each TypeScript project (every package `tsconfig.json` in a
monorepo, and the configs they reference) whose package or workspace root has
`node_modules/.bin/tsc` after each complete generation, so `diagnostics` is
populated without a paste. Any other toolchain publishes through `diagnose`. A
file read reports on the tsconfig that owns the file.

A read with no publication returns a typed problem whose message names the exact
next step: install the compiler, paste output, or retry while the producer runs.
Preserve that unavailable state instead of reporting an empty successful
result.

LSP server inspection is informational: listing a server neither installs nor
starts it.
