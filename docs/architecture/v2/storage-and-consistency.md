# ADR-003: Local Shards, Packed Generations, and Consistency

## Status
Accepted for V2 Phase 0.

## Context
One logical Brain needs explicit ownership, privacy, concurrency, rollback, and multi-machine behavior without pretending one database transaction spans everything.

## Decision
Host-local SQLite owns mutable catalog, profile activity, and repository/privacy-domain project shards. `catalog.db` contains content-free identity allocation/routing and receipts; `activity.db` owns provider/agent/session/Turn/message/task and cross-project activity; `project.db` owns code/Git/delivery evidence and locators, never copied canonical messages. Cross-shard operations use journals/outboxes/sagas and vector watermarks.

Code graphs are immutable packed snapshot generations, published by verified manifest CAS. Branches use generation reuse/overlays, not database copies. Content-addressed blobs are scoped by privacy domain, encryption-key epoch, and retention class; deduplication never crosses that compound domain. Optional protected raw quarantine is separately encrypted, access controlled, short-lived, and excluded from ordinary indexes and stores.

SQLite uses host-local files, WAL, synchronous FULL, one fenced writer authority per mutable shard, daemon-owned construction, snapshot fences, and deterministic shutdown/checkpoint receipts. Remote sharing exchanges authenticated semantic snapshots/tails through application/API contracts; network-mounted SQLite, implicit multi-primary writes, and remote database URLs are forbidden.

## Rejected alternatives
- One giant database: couples privacy, backup, and failure domains.
- Per-project session copies or per-branch/per-board databases: create competing authorities.
- Network-mounted SQLite/libSQL as V2 remote semantics: page transport is not application consistency.
- Mutable graph tables as historical snapshots: weak publication and expensive copying.

## Compatibility, rollback, and removal gates
V1 stores open read-only through import adapters. Every family receives a retained/skipped/quarantined/redacted/deleted disposition, backup and integrity receipt, shadow watermark parity, and route cutover receipt. Non-disposable sources remain through rollback; V1 writers/readers retire only when no route opens them and restore/failover drills pass.