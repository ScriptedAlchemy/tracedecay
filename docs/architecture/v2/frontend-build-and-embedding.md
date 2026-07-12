# ADR-007: Vite Frontend Build and Hermetic Embedding

## Status
Accepted for V2 Phase 0 after a measured Rsbuild-versus-Vite comparison.

## Decision
Select Vite for the initial V2 dashboard. A repository-local comparison on the current React/TypeScript shell considered cold/hot build, route-chunk output, embedded asset manifest stability, CSP compatibility, source-map control, test integration, dependency footprint, and migration cost. Rsbuild/Rspack is faster on large rebuild-oriented corpora and remains a future candidate, but Vite wins now because the existing dashboard already uses Vite-family conventions, Vitest integrates directly, static manifest/base-path behavior is simpler for Axum embedding, and adopting Rsbuild before V2 routes exist adds migration surface without measured user benefit. Re-evaluate only with the same recorded corpus and thresholds.

The build is hermetic and lockfile-pinned: no network at runtime/build verification, no timestamps or absolute paths in artifacts, deterministic content hashes, explicit base path, route-level chunks, local fonts/assets, production source maps disabled by default, and a checked asset manifest. Axum serves exact hashed assets with immutable caching and HTML with no-cache plus CSP/nonces; history fallback applies only to known V2 routes and never asset-like misses. Standalone and host-wrapped embeddings use the same bundle and generated API client.

Test infrastructure is equally hermetic: no process-global mutable test state; each test owns temporary stores, frozen/injected clocks and RNG, scoped environment, reserved ephemeral ports, isolated profile/config/key roots, and deterministic task/process shutdown. Tests declare libtest/nextest grouping, retries, timeouts, platform capabilities, and serial resource groups instead of depending on order. Linux, macOS, and Windows run the contract matrix; unsupported capability is explicit, not a silent skip.

## Rejected alternatives
- Rsbuild now: promising measured rebuild speed, but premature migration and less direct current Vitest/embed continuity.
- Runtime CDN assets or dev-server-dependent packaging.
- One monolithic bundle, catch-all HTML for missing assets, production inline source maps, or global test fixtures/env/ports.

## Compatibility, rollback, and removal gates
Keep the V1 bundle and router behind the shell flag until base-path, CSP, route-chunk, cache, source-map, standalone/wrapper, direct reload, and legacy redirect tests pass. Rollback switches the manifest atomically. Remove V1 assets/build scripts only after package/release integrity and offline rebuild receipts pass.