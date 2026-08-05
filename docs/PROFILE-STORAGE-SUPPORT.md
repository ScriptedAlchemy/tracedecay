# Profile Storage Support Boundary

TraceDecay V2 accepts only the exact final shape for an admitted profile store.
Any other persisted shape returns `ResetRequired` and requires an explicit reset
or recreation; support guidance must not promise a V1 reader, conversion,
backfill, sidecar reconciliation, or profile-store migration.

## Planned Support Bundle Privacy

Support-bundle export is not implemented yet. When it lands, the redacted mode should default to metadata only and may include:

- Resolved active project identity, storage mode, store class, and resolution source.
- Exact-shape admission status, aggregate table counts, artifact sizes, health
  states, and lock or dirty indicators.
- Redacted aliases and path classes sufficient to explain which store was selected.
- Error codes and high-level diagnostics that do not embed payload contents.

Quota reporting is planned separately and should only be documented here once a concrete storage/status surface exists.

The redacted bundle must not include:

- Source code, rendered `read_cache` bodies, transcript text, memory fact content, LCM payload bodies, or response-handle bodies.
- Credential-bearing git remotes, API tokens, env override values, or raw adapter config contents.
- Response handles or payload refs when those identifiers can retrieve plaintext.
- Absolute paths by default when they reveal private directory names; use explicit `--include-paths` for full paths.

Any opt-in mode that includes paths or payload excerpts should mark the bundle as sensitive and require an explicit flag.
