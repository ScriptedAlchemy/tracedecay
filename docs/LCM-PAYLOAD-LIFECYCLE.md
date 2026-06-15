# LCM external payload lifetime & retention contract

Status: **contract** (normative for follow-on work). Companion to the storage/deletion
audit in Kanban task `t_c2443a7f` and the compression-behavior map in
`docs/LCM-COMPRESSION-BEHAVIOR.md`.

This document defines the *contract* an externalized LCM payload obeys from ingest to
removal: who owns it, when it becomes garbage, how it is removed safely, and what is and
is not in scope. The actionable GC algorithm, dashboard visibility, and tests live in the
child tasks `t_bbd369f2`, `t_0ab1c041`, and `t_f0e07c5c`; they must conform to the rules
here, not re-derive them.

All file:line references are against the current tree and are anchors, not invariants.

## 1. Purpose & scope

Externalized LCM payloads are large message bodies written to the filesystem instead of
inline in `lcm_raw_messages.content`. Today the database and the filesystem are mutated by
different code on different timelines, and several paths can drop DB rows without removing
the corresponding file. This contract pins down the intended semantics so that deletion,
GC, diagnostics, and any future delete APIs converge on one model.

**In scope**

- The relationship between `lcm_external_payloads` rows, `lcm_raw_messages` references, and
  payload files under `<storage_root>/lcm-payloads`.
- When a payload becomes collectable, the safe order of removal, and the grace window
  before an orphan/unreferenced payload is reaped.
- Required behavior of any delete path (existing or future), idempotency, missing-file and
  dangling-reference handling, and compatibility with payloads created before this
  contract.

**Out of scope (non-goals, §13)**

- Age/content-based retention policy ("delete payloads older than N").
- Cross-storage-root or cross-profile GC.
- Soft-delete, undo, archive, or body recovery after GC.
- Cross-message/cross-owner deduplication.

## 2. Verified current state (ground truth)

These claims were re-verified against source for this contract; the audit (`t_c2443a7f`)
has the full map.

- **File naming & location.** `write_external_payload`
  (`src/sessions/lcm/payload.rs:117`) names each payload
  `payload_<sha256(provider\0session_id\0message_id\0content_hash)>.payload` and writes it
  under `payload_dir(storage_root)` = `<storage_root>/lcm-payloads`
  (`payload.rs:52`). Project-local stores resolve the root to
  `<project>/.tracedecay`; profile-scoped stores to `<hermes_home>/.tracedecay` (legacy
  `.tokensave` fallback).
- **Schema.** `lcm_external_payloads` is keyed by `payload_ref` (PK) with
  `UNIQUE(provider, message_id, payload_ref)` and `FOREIGN KEY(provider, session_id)
  REFERENCES sessions ON DELETE CASCADE` (`src/sessions/lcm/schema.rs:133-149`). There is
  **no FK from `lcm_external_payloads` to `lcm_raw_messages`**, and no FK from raw rows to
  payload metadata. `lcm_raw_messages` carries a nullable `payload_ref` and a
  `storage_kind IN ('inline','external')` (`schema.rs:104-132`).
- **Write order is not atomic with the DB transaction.** `upsert_session_message`
  (`src/global_db.rs:957`) opens `BEGIN IMMEDIATE`, then calls
  `upsert_raw_message_with_payload` (`src/sessions/lcm/raw.rs:226`), which writes the file
  (`raw.rs:258` → `payload.rs:117`) *before* the metadata row (`raw.rs:267`) and raw row
  (`raw.rs:275`). The file write happens inside the open transaction, but **SQLite cannot
  roll back a filesystem write.** On commit failure, rollback, or crash after the file is
  written, the DB rows are undone but the orphan file remains.
- **Read path validates references before reading bytes.** `expand_payload`
  (`payload.rs:211`) validates the ref, loads metadata, enforces provider+session ownership,
  then `ensure_current_raw_payload_ref` (`payload.rs:253`) requires the ref to be either the
  raw row's current `payload_ref` (whole-message external) or present as a placeholder in
  `content`/`snippet_text`/`index_text`/`metadata_json`. Only then is the file read
  (`read_payload_file`, `payload.rs:439`) and its SHA-256 verified. A missing file yields
  `LcmError::PayloadMissing`; a hash mismatch yields `PayloadIntegrityMismatch`.
- **There is no payload deletion primitive.** `payload.rs` contains no `remove_file`,
  `delete`, or reap function. The only owner mutation is `reassign_session_payloads`
  (`payload.rs:155`), which moves DB owner rows during compression-boundary carry-over and
  never touches files (refs are stable; files don't move).
- **Deletes bypass payload-aware code today.** `lcm_doctor mode=clean apply`
  (`delete_clean_candidates_in_transaction`, `src/sessions/lcm/doctor.rs:1239`) issues
  `DELETE FROM lcm_external_payloads` / `lcm_raw_messages` / `lcm_summary_nodes` /
  `lcm_lifecycle_state` but **never removes payload files**, deliberately converting them
  into GC candidates. **There is no public session- or message-delete API** anywhere in
  `src`; the only `DELETE FROM lcm_raw_messages` calls are inside doctor clean
  (`doctor.rs:1281`, `doctor.rs:1333`). Any future `DELETE FROM sessions` would FK-cascade
  through `lcm_*` rows and orphan files.
- **Memory deletion is a separate subsystem.** Hard-deleting a memory fact
  (`src/memory/store.rs`, `src/dashboard/memory_api.rs`) never touches LCM payloads or
  session storage.
- **Re-externalization / inline-conversion orphans are not reconciled.** `upsert_inline_raw_message`
  (`raw.rs:139`) nulls `payload_ref` and sets `storage_kind='inline'` without removing the
  prior `lcm_external_payloads` row. Re-externalizing the same message with changed content
  produces a new deterministic ref; the old metadata row + file are left behind because
  `upsert_payload_metadata` conflicts only on `payload_ref` (`payload.rs:174`), not on the
  message owner.
- **Diagnostics already classify these states (read-only).** `lcm_doctor` reports
  `missing_payload_refs`, `orphan_payload_refs` (= `gc_candidate_payload_refs`),
  `unreferenced_metadata`, `missing_placeholder_metadata`, `missing_placeholder_files`
  (`doctor.rs:370-379`). `lcm_status` reports `missing_count`, `unreferenced_count`, and
  `gc_candidate_count` (== unreferenced) (`src/sessions/lcm/query.rs:495-500`).
- **A GC tombstone marker is already reserved but unwritten.** `is_external_payload_placeholder`
  (`payload.rs:104`) recognizes both `[externalized payload: …]` and `[gc'd externalized
  payload: …]` (and the `tool output` variants), but nothing in the tree writes the `gc'd`
  form today. This contract adopts it as the tombstone.

## 3. Definitions

A **payload** is the triple `(payload_ref, lcm_external_payloads row, payload file)`. The
**canonical reference set** for a ref `R` is "every `lcm_raw_messages` row whose current
state names `R`" — either `storage_kind='external' AND payload_ref=R`, or `R` appears as a
placeholder in any of `content`, `snippet_text`, `index_text`, `metadata_json` (parsed by
`extract_payload_refs_from_text`, `payload.rs:73`).

| State | Definition |
|---|---|
| **live / referenced** | `lcm_external_payloads` row exists **and** ≥1 raw message currently references `R`. |
| **unreferenced** (GC candidate) | `lcm_external_payloads` row exists but **no** raw message references `R`. |
| **missing** | `lcm_external_payloads` row exists (and `R` is referenced) but the file is absent. |
| **orphan file** | A file under `lcm-payloads` with no `lcm_external_payloads` row for its ref. |
| **dangling placeholder** | A raw message placeholder names `R`, but there is no metadata row (and usually no file). |
| **tombstoned** | Payload was reaped: metadata row deleted, file removed, and the raw placeholder rewritten to the `[gc'd externalized payload: …]` form. |

## 4. Ownership model (decision)

**Decision OM-1.** `lcm_external_payloads` is the single source of truth for "this payload
exists and who owns it." The payload file is the canonical body bytes. The `lcm_raw_messages`
references (whole-message `payload_ref` + the four placeholder-bearing text columns) are the
*live reference set*; they do not own the payload, they cite it.

**Decision OM-2.** Because there is no FK between raw messages and payload metadata, liveness
is derived, not enforced: a payload is **referenced** iff at least one raw message currently
cites it (per §3). GC and diagnostics must compute referenced-ness from the union of the raw
`payload_ref` column and placeholder extraction over the four text columns — exactly as
`ensure_current_raw_payload_ref` and the doctor diagnostics already do.

**Decision OM-3.** Owner is `(provider, session_id, message_id, content_hash)`, embedded in the
ref itself. A ref is therefore specific to one message's exact content; the same content in a
different message gets a different ref. Compression-boundary carry-over reassigns owner rows
(`reassign_session_payloads`) without moving files — refs are stable across session rotation.

Rationale: the current schema already encodes this and the read path already trusts it. Adding
a raw→payload FK now would reject legitimate transient states (orphan files from a crashed
ingest, unreferenced metadata from inline-conversion) that GC is responsible for reconciling.
The contract treats those transients as first-class states instead of constraint violations.

## 5. Lifecycle state machine

```
                 ingest (write file -> metadata row -> raw row)
                                      |
                                      v
                              +---------------+
                   .--------->|    live       |
                   |          +---------------+
                   |            |          |
        re-ingest  |            |          | raw reference dropped
        (same id+  |            |          | (inline-conv / re-externalize /
        content)   |            |          |  placeholder rewrite / doctor
                   |            v          v
                   |   +----------------+   +------------------+
                   +---| live (refresh) |   |   unreferenced   |
                       +----------------+   +------------------+
                                                 |       ^
                                  grace period   |       |  re-referenced
                                  (rescan)       v       |  (re-ingest / replay)
                                           +-------------+-----+
                          reap ---------->|  tombstoned        |
                                           |  (row del, file    |
                                           |   del, ph -> gc'd) |
                                           +--------------------+
                                                 ^
   orphan file ----reap--->  (file removed)     |
   (no row)                                      |
   dangling placeholder --rewrite--> tombstoned |
                                                 |
   missing file (row+ref present) --no auto-del---> reported only;
        reap of *metadata* only after `reap_missing_metadata_after`
```

Transitions are monotone toward removal except the **live ← unreferenced** recovery edge: a
payload that becomes referenced again before it is reaped returns to live and must not be
reaped. GC therefore re-checks referenced-ness at reap time, not at candidate-marking time.

## 6. Deletion contract

### 6.1 The payload-aware deleter

**Decision D-1.** Introduce a single payload-aware deletion primitive
(`delete_external_payload(conn, storage_root, payload_ref)`, to be implemented under
`t_bbd369f2`) that removes the `lcm_external_payloads` row, the file, and rewrites any raw
placeholders to tombstones, in the order below. Every explicit delete of a payload — and
every new session/message delete API — MUST route through it or through GC. The deleter is
the only code path permitted to call `remove_file` on a payload.

**Decision D-2 (safe removal order — critical).** Remove in this order, never another:

1. Under a `BEGIN IMMEDIATE` transaction: delete the `lcm_external_payloads` row and (if
   applicable) clear/rewrite raw references. Commit.
2. **After commit succeeds:** remove the payload file via `remove_file`.

Rationale: deleting the file *before* the DB transaction commits would destroy body bytes the
transaction might still roll back to need — unrecoverable data loss. Deleting the DB row first
means a crash in the commit→file-remove window leaves an **orphan file** (safe; GC reaps it
later) rather than a missing file behind a live reference (unsafe; expand would fail). This is
the inverse of the ingest order (file before row) and is chosen for the same crash-safety
reason: always err toward leaving an orphan file, never toward losing referenced data.

**Decision D-3.** The deleter is idempotent: deleting a payload whose metadata row is already
absent, whose file is already gone, or whose placeholders are already tombstoned is a no-op
success, never an error. This makes retries and GC re-runs safe.

**Decision D-4.** The deleter must never delete a file whose on-disk SHA-256 does not match
the (pre-deletion) metadata `content_hash`, and must never delete a file that is currently
referenced by a live raw message. On either condition it aborts and reports an integrity
anomaly instead of removing anything.

### 6.2 Synchronous vs deferred (decision)

**Decision SD-1.** There are two deletion flavors, by intent:

- **Explicit deletes** (session/message delete API, operator `doctor` reap of a specific
  payload, manual deleter call): **synchronous**. The deleter commits the DB change and removes
  the file before returning success to the caller. The caller observes the payload as gone.
- **Background reconciliation** (orphan files from crashed ingests, unreferenced metadata left
  by inline-conversion/re-externalization, files orphaned by past doctor-clean runs): **deferred
  to GC**, reaped only after the grace period in §6.3.

**Decision SD-2.** Hot-path ingest never deletes. `upsert_inline_raw_message` and re-externalization
MUST NOT synchronously delete the superseded payload. The superseded metadata row becomes
unreferenced and is reaped by GC after the grace period. Rationale: (a) synchronous deletion in
the ingest hot path reintroduces file-delete-in-tx ordering risk on every inline upsert;
(b) a lookup of the prior ref adds latency to a write-heavy path; (c) the unreferenced state is
already detected by diagnostics and is cheap to carry for a grace period. (Alternative
considered and rejected: delete-after-commit in the upsert path.)

### 6.3 Grace period (decision)

**Decision GP-1.** Default unreferenced/orphan grace period: **24 hours**, configurable via
`lcm_payload_gc_grace_seconds` (min floor 300 s / 5 min to prevent a dangerous zero/near-zero
config). A payload/file may be reaped only after it has been continuously collectable for at
least the grace period.

**Decision GP-2.** Grace is measured two ways depending on state, to avoid requiring a schema
change in v1:

- **Orphan files** (no metadata row): by file `mtime`/`ctime`. Reap only files older than the
  grace period. This bounds the ingest-crash and commit→remove windows automatically.
- **Unreferenced metadata** (row present, no references): by the **two-scan rule** — GC marks a
  ref as a candidate on pass N and reaps it on pass N+1 only if it is *still* unreferenced,
  with the two passes separated by ≥ the grace period. This needs no new column.

**Decision GP-3 (evolved form, optional schema bump).** A cleaner long-term shape is an
additive `unreferenced_since`/`gc_state` column on `lcm_external_payloads` (schema v5),
backfilled so every existing row starts as `live`, with the first GC pass beginning tracking.
This is monotonic-safe under the existing migration guard (`schema.rs:87-96` skips DBs written
by a newer release). v1 may ship with the two-scan rule and migrate later; the contract is the
same either way.

**Decision GP-4.** A separate, longer knob `reap_missing_metadata_after` (default 7 days)
governs reaping a **missing** payload's metadata row (file gone but row+reference present). A
missing payload is NOT reaped on the normal grace period — it is reported as an anomaly first,
because a missing file behind a live reference may indicate an FS problem the operator should
investigate before the reference is tombstoned.

## 7. Hard-delete behavior

| Trigger | Behavior |
|---|---|
| **Memory fact hard-delete** | No LCM effect. Memory facts live in `memory_facts` and never cite LCM payloads (`src/memory/store.rs`, `src/dashboard/memory_api.rs`). Independent subsystem. |
| **Session delete** (no public API today) | MUST route through the payload-aware deleter for every payload owned by `(provider, session_id)`, **or** explicitly delete the `sessions` row and leave the files for GC. Contract requires one of these two be *documented*; the recommended path is "delete `sessions` row, leave files, let GC reap after grace" because it is simplest and crash-safe (FK cascade drops the `lcm_*` rows; orphan files become GC candidates). The deleter is used only when immediate file removal is required. |
| **Message delete** (no public API today) | Same two options as session delete, scoped to one message: either call the deleter (removes that message's referenced payloads synchronously) or drop the raw row and let GC reap the now-unreferenced payloads after grace. **Caveat:** because refs can be shared via nested placeholders within the same message, a message-delete reap must verify the ref is referenced by *no* surviving row before removing it. |
| **`lcm_doctor clean apply`** | Unchanged in effect (deletes DB rows, leaves files) but now **classified**: it is a *deferred* delete that intentionally produces GC candidates. The doctor output must state that payload files will be reaped by GC after the grace period, so operators do not expect immediate disk reclamation. |
| **Direct SQL / FK cascade / operator `rm`** | Unsupported but **tolerated**. GC reconciles on the next pass (orphan files → reaped; dangling placeholders → tombstoned). New delete code MUST NOT rely on this; it must call the deleter. |
| **Compression-boundary carry-over** | No deletion. `reassign_session_payloads` moves owner rows; refs/files are stable and remain referenced via the carried-over raw messages. GC must not reap a payload whose owner moved within the grace window (covered automatically by referenced-ness re-check). |

## 8. Idempotency requirements

- **Write** (`write_external_payload`): already idempotent — `create_new` + same-bytes accepted,
  different bytes → `PayloadIntegrityMismatch` (`payload.rs:405-437`). Re-ingest of identical
  content is a no-op file write. **Keep this invariant.**
- **Upsert**: `upsert_payload_metadata` and `upsert_*_raw_message` are `ON CONFLICT … DO UPDATE`
  and safe to replay.
- **Delete** (`delete_external_payload`): idempotent no-op success on already-absent row/file/
  tombstone (D-3).
- **GC reap**: reaping an already-absent file is a no-op. A crash between metadata delete and
  file delete leaves an orphan file the next pass reaps; a crash between two tombstone rewrites
  leaves a partially-tombstoned message the next pass completes. Re-running GC after any crash
  must converge to a clean state.
- **Expand**: deterministic per state — live returns bytes; tombstoned returns `PayloadGc'd`;
  missing returns `PayloadMissing`; corrupted returns `PayloadIntegrityMismatch`. No expand
  call mutates state.

## 9. Missing-file & dangling-reference handling

| Condition | Detection | Action |
|---|---|---|
| **Missing file** (row + reference present, file absent) | `read_payload_file` → `PayloadMissing` (`payload.rs:439`); doctor `missing_payload_refs`; status `missing_count` | **Report only.** Do not auto-delete the metadata row. Surface in dashboard as an error state. After `reap_missing_metadata_after` (GP-4), GC may tombstone the reference. |
| **Orphan file** (file present, no row) | doctor `orphan_payload_refs`/`gc_candidate_payload_refs` | Reap by GC after `mtime`-based grace (GP-2). |
| **Unreferenced metadata** (row present, no live reference) | doctor `unreferenced_metadata`; status `unreferenced_count`/`gc_candidate_count` | Reap by GC after two-scan grace (GP-2): delete row, remove file, tombstone any residual placeholder. |
| **Dangling placeholder** (placeholder cites ref with no row, no file) | doctor `missing_placeholder_metadata` / `missing_placeholder_files` | GC rewrites the placeholder to the `[gc'd …]` tombstone form so the message still reads correctly; no file to remove. |
| **Corrupted file** (present, hash mismatch) | `expand_payload` → `PayloadIntegrityMismatch` (`payload.rs:231`) | **Never auto-delete.** Report as integrity anomaly; operator decides. |

**Decision MF-1.** A new distinct error `LcmError::PayloadGc'd` MUST be returned by expand when
the raw reference is a `[gc'd …]` tombstone, so operators can distinguish "file vanished
unexpectedly" (`PayloadMissing`) from "intentionally reaped" (`PayloadGc'd`). Today both fall
through to `PayloadNotFound`/`PayloadMissing`; the tombstone prefix is already parsed by
`is_external_payload_placeholder` (`payload.rs:104`), so only the error branch is new.

## 10. GC reap — contract for the algorithm task (`t_bbd369f2`)

The full algorithm is `t_bbd369f2`'s to design, but it MUST satisfy this contract:

1. **Enumerate safely.** Only files matching `^payload_[0-9a-f]{64}\.payload$` directly under
   the canonical payload dir. Reject any other name, any symlink, any path whose canonical
   parent is not the canonical payload dir. Reuse `validate_payload_ref`
   (`payload.rs:56`), `canonical_storage_root` (`payload.rs:366`), and `ensure_contained`
   (`payload.rs:396`) — do not invent new path logic.
2. **Never delete by caller-supplied path.** Refs only; resolve to a path under the canonical
   dir and re-validate containment immediately before `remove_file`.
3. **Re-check referenced-ness at reap time under `BEGIN IMMEDIATE`.** A candidate marked on a
   previous scan is reaped only if still unreferenced when the reap transaction opens. This is
   the guard against racing concurrent ingest/replay/compression carry-over.
4. **Honor the grace period** (GP-1..GP-4): `mtime`-based for orphan files, two-scan for
   unreferenced metadata, `reap_missing_metadata_after` for missing payloads.
5. **Removal order per §6.2 (D-2):** DB row first (commit), then file; then tombstone any
   residual placeholders. A crash anywhere is recoverable by re-running.
6. **Dry-run/report mode is mandatory and default.** Reap is opt-in (`apply=true`); the report
   enumerates exactly what would be removed (refs + byte totals), never bodies. No body bytes
   appear in logs, reports, or metrics — only refs, counts, sizes, and hashes already in the DB.
7. **Batching/locking:** reap inside bounded transactions; a single GC run holds `BEGIN
   IMMEDIATE` only across the per-ref decision+delete, not across filesystem I/O, to avoid
   long lock holds. See `t_bbd369f2` for batch sizing.
8. **Metrics:** emit counts and bytes reaped, orphans remaining, missing remaining, errors;
   record `last_gc_at` and last error. (Surface fields are specified by `t_0ab1c041`.)

## 11. Compatibility & migration

- **Existing payloads are valid as-is.** Payloads created before this contract already have a
  metadata row + file + reference; they are `live`. No data migration is required to honor the
  contract — it is enforced *forward* by routing deletes through the deleter (§6.1) and adding
  GC.
- **Existing orphans are the first GC candidates.** Files orphaned by past doctor-clean runs,
  pre-contract crashes, or inline-conversions are simply collected on the first GC pass after
  the grace period. No special one-off reclaim is needed.
- **Tombstone markers are forward-compatible.** `is_external_payload_placeholder` already
  accepts the `[gc'd …]` prefixes, and `ensure_current_raw_payload_ref` already treats them as
  valid references; only the *write* of the tombstone and the `PayloadGc'd` error branch are
  new.
- **Optional schema v5** (`unreferenced_since`/`gc_state`, GP-3) is additive and
  backfill-initialized to `live`; the migration guard (`schema.rs:87-96`) keeps it
  monotonic-safe against DBs written by newer releases. v1 MAY ship on the current schema using
  the two-scan rule.
- **No breaking change to `lcm_doctor`/`lcm_status` response shapes** is required; the GC task
  may *add* fields (e.g., `last_gc_at`, `bytes_reclaimable`) but must not remove or rename
  existing ones.

## 12. Edge cases

- **Concurrent ingest during GC scan** — guarded by the grace period plus the reap-time
  referenced-ness re-check under `BEGIN IMMEDIATE` (§10.3). A payload written between scan and
  reap is `live` at reap time and is skipped.
- **Crash mid-reap** (commit done, file not yet removed) — leaves an orphan file; next GC pass
  reaps. (§8.)
- **Crash mid-tombstone** (some placeholders rewritten, others not) — re-run completes it;
  tombstoning is idempotent. (§8.)
- **Ref shared within one message via nested placeholders** — a message-delete reap must confirm
  the ref is cited by no surviving raw row before removing it; otherwise leave it referenced.
- **Symlink/path-traversal in reap input** — rejected by `validate_payload_ref` + canonical-root
  + `ensure_contained`; reap never follows links and never deletes outside the canonical dir.
- **File present but corrupted** (hash mismatch) — never auto-deleted; reported as integrity
  anomaly (§9, §6.1 D-4).
- **Operator `rm` of a live file** — produces a `missing` payload (row + reference present, file
  gone); reported, not auto-tombstoned until `reap_missing_metadata_after` (GP-4).
- **Two processes GC-ing the same store** — only one may hold the reap `BEGIN IMMEDIATE` at a
  time; the loser observes the row already gone and no-ops (idempotent). The store's existing
  writer lock model applies.

## 13. Security invariants (non-negotiable)

- **Never log, preview, report, or stream payload body bytes.** Reports carry refs, counts,
  byte totals, and the hashes already stored in `lcm_external_payloads` — nothing more.
- **Keep all existing path/ref safety:** `validate_payload_ref` (single normal component, no
  `.`/`..`/slashes), non-symlink storage root (`canonical_storage_root`), 0700 payload dir /
  0600 files, Linux `O_NOFOLLOW` (`payload.rs:456-483`). Reap must use the same primitives.
- **Never delete outside the canonical payload dir; never follow symlinks during reap.**
- **Keep DB backups before destructive GC** (mirroring doctor `clean apply`, which calls
  `checkpoint_wal_for_backup` then `backup_database`/`copy_sqlite_file_set`,
  `doctor.rs:160-161` & `1070-1090`): a reap run that mutates state should checkpoint WAL first
  so the store is recoverable.

## 14. Non-goals

- Age- or content-based retention policy (delete payloads older/larger than N). This contract
  defines the *mechanical* lifetime only; age-based retention can layer on top later and would
  convert `live` payloads into `unreferenced` via a policy pass.
- Cross-storage-root or cross-profile GC. Each store (project-local or profile-scoped) is GC'd
  independently against its own DB.
- Soft-delete, undo, archive, or restoration of reaped bodies. Reap is hard and permanent,
  consistent with memory fact deletion being hard-delete. The tombstone is informational only.
- Cross-message / cross-owner payload deduplication. The owner-hash includes `message_id`; no
  dedup exists and this contract does not add it.
- Reaping summary nodes, lifecycle state, or maintenance debt — those have their own cleanup in
  doctor `clean`. This contract is strictly about payload files + `lcm_external_payloads`.

## 15. Decision rationale summary

| # | Decision | Primary rationale | Rejected alternative |
|---|---|---|---|
| OM-1/2 | Metadata row is source of truth; referenced-ness derived from raw rows + placeholders | Matches schema & read path; avoids rejecting legitimate transient states | Add raw→payload FK (would reject orphan/unreferenced transients GC must handle) |
| D-2 | Remove DB row first (commit), then file | Crash leaves orphan file (safe), not missing-file-behind-live-ref (data loss) | Delete file first then row (unrecoverable on rollback) |
| SD-1 | Explicit deletes synchronous; hot-path orphans deferred to GC | Callers of explicit delete observe completion; hot path stays simple & crash-safe | Always-synchronous (hot-path risk) or always-deferred (explicit delete feels broken) |
| SD-2 | Ingest never deletes superseded payloads | Keeps write path crash-safe & fast; diagnostics already detect unreferenced | Delete-after-commit in upsert path (latency + ordering risk on every inline upsert) |
| GP-1 | 24h default grace (min 5 min) | Bounds ingest-crash & commit→remove windows; cheap to keep short-term, unrecoverable if wrongly reaped | Zero grace (races concurrent ingest); very long grace (wastes disk) |
| GP-2 | `mtime`-based for orphans, two-scan for unreferenced | Needs no schema change in v1 | Require `unreferenced_since` column now (deferred to optional v5, GP-3) |
| GP-4 | Missing payloads reported, not auto-reaped on normal grace | Missing-file-behind-live-ref may indicate FS problem worth investigating | Reap missing metadata on the 24h grace (could hide a real outage) |
| MF-1 | Distinct `PayloadGc'd` error for tombstones | Distinguishes intentional reap from unexpected loss in ops/telemetry | Reuse `PayloadMissing` (ambiguous) |
| §7 | Session/message delete: recommend drop row + leave files for GC | Simplest, crash-safe; FK cascade does the DB work | Force every delete through the synchronous deleter (over-engineered for the common case) |

## 16. Handoff to child tasks

- **`t_bbd369f2` (GC workflow):** implement `delete_external_payload` + the background reaper per
  §6 and §10. This contract is its spec; cite §6.1 (D-1..D-4), §6.2, §6.3, §9, §10, §13.
- **`t_0ab1c041` (dashboard/doctor visibility):** surface `live`/`unreferenced`/`missing`/`orphan`/
  `tombstoned` counts and bytes, `last_gc_at`, last error, and a mandatory dry-run view before
  destructive cleanup. Reuse existing fields (§2) and add per §10.8. Healthy/warning/error
  thresholds key off `missing_count` and `missing_payload_refs` (error) vs `orphan`/`unreferenced`
  counts (warning).
- **`t_f0e07c5c` (tests):** cover the state transitions in §5, the safe removal order (§6.2 incl.
  crash-between-commit-and-remove), idempotency (§8), symlink/path-traversal reap rejection
  (§10.1-2, §13), missing vs tombstoned error distinction (§9 MF-1), and inline-conversion/
  re-externalization orphaning (§2). The reserved `[gc'd …]` tombstone prefix
  (`payload.rs:104`) is the marker to assert against.
