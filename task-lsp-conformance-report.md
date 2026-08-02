# LSP conformance: increment 1

Scope: daemon-gateway safety bounds and analyzer-broker refresh ownership only.

- Workspace admission is capped at eight roots.
- Dirty overlays debounce for 75 ms, force publication by 250 ms, and save still flushes immediately.
- The broker canonicalizes the admitted project root exactly once per refresh, fails closed with a typed error when that root disappears, and never creates a client slot in that case.
- Analyzer refreshes retain deterministic diagnostic order. Per-broker queue permits reserve at most 128 batches across prepared work, while run permits cap all active batches at four and release on completion, cancellation, or drop.
- The broker keeps configuration/state ownership only; shared-client semantic authority, refresh scheduling, and broker tests live in cohesive child modules under 400 lines.
- Existing protocol tests cover synchronous cancellation, stale-publication suppression, federated-root routing, and symlink-root isolation without wall-clock assertions.

Verification: `cargo test -p tracedecay-lsp --lib` (130 passed) and `cargo test -p tracedecay-lsp --test analyzer_runtime` (30 passed).

Deferred: real packaged Claude/OpenCode journeys, Cursor native-host process evidence, Kimi lifecycle, and dashboard/package work require the next approved slice.
