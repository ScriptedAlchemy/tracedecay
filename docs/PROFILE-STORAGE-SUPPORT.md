# Profile-backed storage

## Resolution

The repository `.tracedecay/` directory is an enrollment/configuration marker,
not a business database. The daemon resolves the selected profile and exact
registered project identity to a private owner shard such as
`<profile>/projects/<project_id>/`.

Each final V2 owner shard contains:

- SQLite authorities for relational/content records, manifests, journals,
  leases, receipts, redaction payloads, and execution fencing;
- one embedded Grafeo authority for graph topology, typed relations, and
  admitted vector indexes; and
- daemon-managed content-addressed payloads and generated artifacts required by
  the owning stores.

There are no branch databases, dashboard/payload sidecar databases, repo-local
fallback stores, host-specific TraceDecay stores, or environment-selected
business DB paths. Hermes/Codex/Claude/Cursor/Kimi/OpenCode homes and profiles
are host-owned inputs, never TraceDecay project identity.

## Fresh-store rule

Final V2 creates the exact final store shape. An incompatible TraceDecay
profile/project store returns `ResetRequired` and must be explicitly recreated.
No legacy reader, migration, relink, backfill, dual-write, census, or adoption
path exists. Historical host transcripts/logs and repository observations may
be ingested afterward through ordinary sanitized V2 capture.

## Privacy and support output

Status, Doctor, telemetry, and any support export use daemon/application read
models. Default output may include opaque owner/store classes, schema/capability
identity, aggregate counts, bounded sizes, watermarks, health states, and
redacted error classes. It must not include source, transcript/fact/payload
content, credentials, raw adapter configuration, retrievable handles, or
absolute private paths. Doctor is read-only.

## Test isolation

Fixtures isolate home, profile, project, session, registry, socket, SQLite, and
Grafeo paths and use the production identity/resolver/daemon authorities. They
never read or mutate the operator's profile or agent-host data.

Required cases include fresh creation, exact reopen, incompatible reset,
project/profile isolation, linked-worktree shared identity, moved/symlinked
roots, missing registry, unavailable/corrupt stores, concurrent readers,
serialized writer admission, shutdown, and historical host-data capture.
Tests do not fabricate old stores for migration or copy database files between
identities.
