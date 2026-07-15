# PR5: sanitized observation vertical

PR4 established the daemon-owned transcript persistence boundary. PR5 moves one
existing provider end to end through typed capture, mandatory sanitization,
authoritative persistence, replay, and restart.

## Product slice

Use one existing production provider path. Preserve its current parser and V1
behavior while adding the V2 observation path behind the same daemon and
`GlobalDb` authority.

The vertical path is:

```text
provider record
  -> bounded parse
  -> typed sanitizer input
  -> sanitization receipt and observation identity
  -> daemon command
  -> atomic observation, source cursor, and projection enqueue
  -> replay/read with explicit coverage
```

## Required behavior

- Define ordinary Rust types for source identity, source generation/cursor,
  canonical observation identity, sanitizer disposition, receipt, payload
  reference, and idempotency key.
- Parse structure before scanning content. Apply the mandatory sanitizer before
  any database, payload file, log, metric, dead letter, replay, or export sink.
- Persist only receipt-bound sanitized content. Rejected or quarantined content
  must not leak through error messages or recovery artifacts.
- Send observations to the daemon. Hooks and provider adapters may frame and
  spool events but cannot open or mutate SQLite.
- Reuse the already-open `GlobalDb` and `tracedecay-store` boundary. Commit the
  observation, durable source cursor/offset, and projection enqueue atomically.
- Keep project observations in the canonical project-wide store and
  profile-wide observations in the profile store. Do not derive ownership from
  CWD, provider cache paths, or transcript filenames alone.
- Resolve worktrees through the project registry and Git common directory.
  Missing, ambiguous, stale, or unavailable authority fails closed.
- Defer partial records without advancing the durable cursor. An exact retry is
  idempotent; the same identity with different content is a typed collision.
- Bound record size, decoding, queues, retry state, and error output. Preserve
  explicit cancellation; add no automatic workflow or agent timeout.
- Expose enough replay/read behavior to prove the stored observation is usable;
  do not stop at schemas or unused scaffolding.
- Record a bounded representative baseline for parse/sanitize/commit/replay
  latency, throughput, CPU, memory, bytes written, and no-op replay work using
  the canonical observability contract. This is input to [PR20](33-end-to-end-performance-optimization.md),
  not a reason to widen PR5 into a performance project.

## Direct tests

- valid record persists and replays with its receipt and source identity;
- secret-shaped fields are rejected, redacted, or quarantined before every sink;
- malformed and partial records do not advance the cursor;
- duplicate replay inserts nothing and preserves the original receipt;
- identity/digest collision fails without overwriting either record;
- crash before commit advances neither data nor cursor;
- crash after commit before acknowledgement replays idempotently;
- suffix resume neither skips nor duplicates observations;
- restart catch-up uses the daemon-owned authority;
- stale owner, missing project/profile authority, and worktree ambiguity fail
  without creating a fallback database;
- concurrent clients still produce one committed observation sequence;
- Linux and Windows stock Cargo tests pass.

## Prohibited scope

- no plan parser, completion tracker, PR DAG, next-ready controller, rewrite
  executor, compatibility inventory, generated plan view, or baseline packet;
- no Claude workflow JavaScript or other host-specific rewrite workflow;
- no macro DSL, procedural macro, parallel schema model, or crate created only
  to satisfy a plan document;
- no second database connection authority, fallback writer, network-mounted
  SQLite, direct hook write, or source-adjacent durable store;
- no broad provider rewrite, semantic search, dashboard redesign, or remote
  replication in this slice.

## Done

PR5 is complete when one real provider record can be captured, sanitized,
committed, replayed, and recovered through the production daemon; every failure
boundary preserves atomic data/cursor state; no unsanitized durable byte or
fallback writer exists; direct focused tests and relevant Linux/Windows stock
Cargo gates pass.
