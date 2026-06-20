# Profile Storage Support Requirements

This document captures support, privacy, and test-fixture requirements for profile-backed project storage.

## Storage wording

User-facing docs and generated guidance should describe the **resolved active project store** instead of assuming every project writes graph data to `<repo>/.tracedecay/tracedecay.db`.

Repo-local behavior remains the default:

- New projects create `<repo>/.tracedecay/` with `tracedecay.db`, `sessions.db`, branch metadata, response handles, and dashboard sidecars.
- Legacy projects with `.tokensave/` continue to use that directory in place when `.tracedecay/` is absent.
- `~/.tracedecay/global.db` remains user-level accounting/registry state, not the canonical graph DB.

Profile storage adds profile-sharded code-project stores such as `~/.tracedecay/projects/<project_id>/`. A repository may then have only an enrollment marker while graph DBs, sessions, payloads, response handles, branch DBs, and dashboard sidecars live in the profile shard. Hermes profile stores remain separate from code-project shards.

## Planned Support Bundle Privacy

Support-bundle export is not implemented yet. When it lands, the redacted mode should default to metadata only and may include:

- Resolved active project identity, storage mode, store class, and resolution source.
- Store manifests, schema versions, aggregate table counts, artifact sizes, health states, lock or dirty indicators, and migration manifest IDs.
- Redacted aliases and path classes sufficient to explain which store was selected.
- Error codes and high-level diagnostics that do not embed payload contents.

Quota reporting is planned separately and should only be documented here once a concrete storage/status surface exists.

The redacted bundle must not include:

- Source code, rendered `read_cache` bodies, transcript text, memory fact content, LCM payload bodies, or response-handle bodies.
- Credential-bearing git remotes, API tokens, env override values, or raw adapter config contents.
- Response handles or payload refs when those identifiers can retrieve plaintext.
- Absolute paths by default when they reveal private directory names; use explicit `--include-paths` for full paths.

Any opt-in mode that includes paths or payload excerpts should mark the bundle as sensitive and require an explicit flag.

## Fixture Contract

Migration and storage-status tests should share fixture builders instead of reimplementing core storage behavior in tests.

Reusable fixtures should cover:

- Repo-local `.tracedecay/` stores with graph DB, sessions DB, branch metadata, response handles, and dashboard sidecars.
- Legacy `.tokensave/` stores that remain active in place.
- Profile-sharded code-project stores with a repo enrollment marker and private profile shard.
- Hermes profile stores under `<hermes_home>/.tracedecay/` that are not code-project shards.
- Stale or unregistered registry rows, moved repos, worktrees, symlinked roots, dirty sentinels, sync locks, and `.branch-add.lock`.
- Seeded `lcm-payloads/`, response handles, curation previews, WAL/SHM sidecars, and `TRACEDECAY_GLOBAL_DB` overrides.

Suggested helper shape:

```rust
struct StorageFixture {
    temp: tempfile::TempDir,
    project_root: PathBuf,
    store_root: PathBuf,
    storage_mode: StorageMode,
    store_class: StoreClass,
}
```

Helpers should create files and SQLite databases directly enough for inventory/status tests, but resolver-open verification should use the real resolver once that API exists. Destructive cleanup, migration apply, and rollback fixtures must be backup-first and should assert that source stores are retained unless an explicit cleanup command runs.
