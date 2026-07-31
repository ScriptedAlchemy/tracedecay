# Memory V2 Owner Archive Implementation Plan

> **Archived record — not implementation authority.** This document preserves
> historical intent, migration safety decisions, and receipt evidence. Current
> requirements come only from the `docs/plans/tracedecay-v2/` hierarchy. Exact
> tests and counts, source-string checks, branch/commit/worktree choreography,
> snapshots, receipts, attestations, PR packets, and gate matrices below are not
> rebuild instructions; validate current migration and runtime behavior directly.
> `MemoryV2OwnerArchiveV1` names the initial final archive wire shape; the
> suffix and branch history require no V2 sibling. Migration remains required
> only for live persisted owner stores that branch retirement can reclaim.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve one owner's complete Memory V2 authority across branch retirement without changing stable identities or publishing a deletion receipt before durable project-wide proof.

**Architecture:** `tracedecay-store` owns a versioned, canonically ordered owner archive and a pure merge planner. The SQLite store adapter exports and imports that contract transactionally; migration lifecycle code sees only typed ports, verifies public reads, establishes database and receipt durability, then permits deletion.

**Tech Stack:** Rust, Serde canonical encoding, TraceDecay domain/store contracts, SQLite writer transactions.

## Global Constraints

- V2 is authoritative whenever present; legacy mirrors never mint replacement FactIds.
- Same identity with incompatible scope, content, or history fails closed before mutation.
- FTS, compatibility banks, dirty markers, and backfill/repair cursors are derived or target-local and are not archived.
- Lifecycle code contains no Memory V2 schema SQL.
- Tests use production writers and public `MemoryApplication` reads after source deletion.

---

### Task 1: Typed owner archive and merge planner

**Files:**
- Create: `crates/tracedecay-store/src/memory/archive.rs`
- Modify: `crates/tracedecay-store/src/memory/mod.rs`
- Modify: `crates/tracedecay-store/src/lib.rs`

**Interfaces:**
- Produces `MemoryV2OwnerArchiveV1`, `MemoryV2ArchiveRecordV1`, `MemoryV2ArchiveFamilyV1`, `MemoryV2OwnerMergePlanV1`, and `plan_memory_v2_owner_merge`.

- [x] Add pure tests for version rejection, owner mismatch, complete authoritative family membership, deterministic canonical ordering/digest, identical replay, new rows, stale legacy map retention, and every same-key conflict.
- [x] Run the archive tests and confirm they fail because the types are absent.
- [x] Implement validated typed scalars, named identity fields, explicit references, canonical ordering/digest, and a non-mutating merge planner.
- [x] Run the archive tests until green.
- [x] Commit as `feat(memory): add typed V2 owner archive`.

### Task 2: SQLite exporter/importer ports

**Files:**
- Create: `src/store/memory/archive.rs`
- Modify: `src/store/memory/mod.rs`
- Modify: `src/migrate/consolidate/sqlite/memory_v2.rs`
- Test: `src/store/memory/archive_test.rs`

**Interfaces:**
- Consumes `MemoryV2OwnerArchiveV1` and `MemoryV2OwnerMergePlanV1`.
- Produces `DatabaseFactStore::export_owner_archive` and `DatabaseFactStore::import_owner_archive`.

- [x] Seed a comprehensive owner through production writers, including purge, feedback, relations, proposals, mappings, quarantine, retrieval provenance, and evidence assembly.
- [x] Write failing export/import contract tests that delete the source and query current facts and lineage through `MemoryApplication`.
- [x] Implement adapter-local row decoding, closure validation, FK-safe transactional import, exact conflict checks, and derived-state rebuild.
- [x] Run only archive store contract tests until green.
- [x] Commit as `feat(memory): add V2 archive store ports`.

### Task 3: Durable cutover protocol

**Files:**
- Modify: `src/migrate/memory_cutover.rs`
- Modify: `src/storage.rs`
- Test: `tests/storage_suite/profile_storage_migration_test.rs`

**Interfaces:**
- Consumes archive export/import ports.
- Produces a versioned receipt binding source path/generation, archive digest, and verified target digest.

- [x] Write failing target-barrier, receipt-sync, committed-merge retry, and durable-receipt resume tests.
- [x] Replace lifecycle raw merge with archive export, plan, import, public readback, database/WAL durability, and durable receipt publication.
- [x] Verify both injected failures retain the source and expose no usable receipt.
- [x] Commit as `fix(memory): make V2 cutover receipt durable`.

### Task 4: PR lifecycle proof

**Files:**
- Modify: `tests/daemon_suite/pr_autotrack_test.rs`

**Interfaces:**
- Consumes the durable project-memory cutover.

- [x] Replace raw branch-memory fixture writes with production archive writers.
- [x] Verify stable FactId and public current/lineage reads after actual source deletion.
- [x] Run the exact nine lifecycle tests and focused archive/cutover contract target.
- [x] Merge current integration, run default check, workspace all-feature/all-target check, and formatting.
- [x] Commit any integration-only repairs conventionally.
