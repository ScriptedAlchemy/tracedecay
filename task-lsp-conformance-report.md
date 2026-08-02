# LSP conformance: increment 1

Scope: daemon-gateway safety bounds and analyzer-broker refresh ownership only.

- Workspace admission is capped at eight roots.
- Dirty overlays debounce for 75 ms, force publication by 250 ms, and save still flushes immediately.
- The broker canonicalizes the admitted project root once per refresh batch; every document resolves from that canonical root, preserving symlink containment checks.
- Analyzer refreshes retain deterministic diagnostic order, run at most four independent workspace-root batches concurrently, and reject more than 128 batches before launching an analyzer.
- Existing protocol tests cover synchronous cancellation, stale-publication suppression, federated-root routing, and symlink-root isolation without wall-clock assertions.

Verification: `cargo test -p tracedecay-lsp --lib` (125 passed).

Deferred: real packaged Claude/OpenCode journeys, Cursor native-host process evidence, Kimi lifecycle, and dashboard/package work require the next approved slice.
