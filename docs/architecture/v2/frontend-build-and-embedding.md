# ADR-007: Vite Frontend Build and Hermetic Embedding

## Status
Pending measured Rsbuild-versus-Vite comparison; Vite remains the provisional
Phase 0 baseline and this ADR cannot advance to Accepted without the receipt below.

## Decision
Use Vite only as the provisional initial V2 dashboard baseline because the
existing shell already uses Vite-family conventions. No measured Rsbuild claim
is made yet. Acceptance requires a checked benchmark receipt that records the
exact lockfile and commit, Node/OS/CPU/RAM, dashboard corpus and route count,
both production commands, both hot-rebuild commands, five cold and twenty hot
runs, median/p95 wall time, peak RSS, output bytes/chunks, asset-manifest digest,
CSP/base-path/source-map checks, and dependency delta. The threshold is: no
candidate may regress hermetic embedding or deterministic output; select
Rsbuild only if hot-rebuild median improves by at least 20% without production
build median, peak RSS, or output bytes regressing by more than 10%. Otherwise
accept Vite. Store raw commands/results in
`docs/architecture/v2/evidence/frontend-build-comparison.md` before changing
this status to Accepted.

The build is hermetic and lockfile-pinned: no network at runtime/build verification, no timestamps or absolute paths in artifacts, deterministic content hashes, explicit base path, route-level chunks, local fonts/assets, production source maps disabled by default, and a checked asset manifest. Axum serves exact hashed assets with immutable caching and HTML with no-cache plus CSP/nonces; history fallback applies only to known V2 routes and never asset-like misses. Standalone and host-wrapped embeddings use the same bundle and generated API client.

Test infrastructure is equally hermetic: no process-global mutable test state; each test owns temporary stores, frozen/injected clocks and RNG, scoped environment, reserved ephemeral ports, isolated profile/config/key roots, and deterministic task/process shutdown. Tests declare libtest/nextest grouping, retries, timeouts, platform capabilities, and serial resource groups instead of depending on order. Linux, macOS, and Windows run the contract matrix; unsupported capability is explicit, not a silent skip.

## Rejected alternatives
- Rsbuild now: deferred until the required reproducible comparison receipt exists.
- Runtime CDN assets or dev-server-dependent packaging.
- One monolithic bundle, catch-all HTML for missing assets, production inline source maps, or global test fixtures/env/ports.

## Compatibility, rollback, and removal gates
Keep the V1 bundle and router behind the shell flag until base-path, CSP, route-chunk, cache, source-map, standalone/wrapper, direct reload, and legacy redirect tests pass. Rollback switches the manifest atomically. Remove V1 assets/build scripts only after package/release integrity and offline rebuild receipts pass.