# ADR-007: Rsbuild/Rspack Frontend Build and Hermetic Embedding

## Status
Accepted. This ADR records the build system already used by the TraceDecay
dashboard; it does not derive product authority from historical transcripts or
cross-project conformance scenarios.

## Decision
Retain the repository's existing Rsbuild/Rspack pipeline. The checked source of
truth is `dashboard/package.json`, `dashboard/package-lock.json`,
`dashboard/build.mjs`, and `dashboard/build.shared.mjs`: production and
development builds use Rsbuild, with Rspack configuration applied by the shared
builder. V2 extends this existing boundary rather than introducing a second
bundler or treating a historical scenario as a migration request.

Rspack/Rsbuild/React Router and Vite references in research transcripts remain
valuable conformance evidence for project disambiguation, temporal retrieval,
and cross-repository context packets. They are not current TraceDecay product
requirements. A future bundler migration requires a separate, explicitly
approved proposal with representative measurements and rollback evidence; it
is not part of PR 1 or PR 25A by default.

The build is hermetic and lockfile-pinned: no network at runtime/build verification, no timestamps or absolute paths in artifacts, deterministic content hashes, explicit base path, route-level chunks, local fonts/assets, production source maps disabled by default, and a checked asset manifest. Axum serves exact hashed assets with immutable caching and HTML with no-cache plus CSP/nonces; history fallback applies only to known V2 routes and never asset-like misses. Standalone and host-wrapped embeddings use the same bundle and generated API client.

Test infrastructure is equally hermetic: no process-global mutable test state; each test owns temporary stores, frozen/injected clocks and RNG, scoped environment, reserved ephemeral ports, isolated profile/config/key roots, and deterministic task/process shutdown. Tests declare libtest/nextest grouping, retries, timeouts, platform capabilities, and serial resource groups instead of depending on order. Linux, macOS, and Windows run the contract matrix; unsupported capability is explicit, not a silent skip.

## Rejected alternatives
- Introduce Vite solely because it appeared in historical evidence: rejected;
  scenario material cannot override the live repository architecture.
- Run an unsolicited Rsbuild-versus-Vite bakeoff: rejected until an approved
  migration proposal establishes a product need and representative corpus.
- Runtime CDN assets or dev-server-dependent packaging.
- One monolithic bundle, catch-all HTML for missing assets, production inline source maps, or global test fixtures/env/ports.

## Compatibility, rollback, and removal gates
Keep the V1 bundle and router behind the shell flag until base-path, CSP, route-chunk, cache, source-map, standalone/wrapper, direct reload, and legacy redirect tests pass. Rollback switches the manifest atomically. Remove V1 assets/build scripts only after package/release integrity and offline rebuild receipts pass.