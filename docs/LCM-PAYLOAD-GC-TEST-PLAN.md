# LCM external payload GC & retention — test plan

Status: **test plan** (normative for the implementation/tests task). This is the third
sibling of the payload work; it enumerates the **named test cases, fixtures, and
pass/fail criteria** the implementation (`t_baa1d2cf`) must satisfy. The *what/why*,
the *how*, and the *visibility* live in the three specs it defers to:

- Lifetime & retention contract — [`docs/LCM-PAYLOAD-LIFECYCLE.md`](LCM-PAYLOAD-LIFECYCLE.md)
  (parent `t_0ab1c041`). Normative for OM-1/2, the lifecycle state machine (§5), the
  deletion contract D-1..D-4 + SD-1/2 (§6), grace GP-1..GP-4 (§6.3), idempotency (§8),
  missing/dangling/corrupted handling (§9), GC reap contract (§10), security §13.
- GC design — [`docs/LCM-PAYLOAD-GC.md`](LCM-PAYLOAD-GC.md) (`t_bbd369f2`). Normative for
  `delete_external_payload`, the four reaper phases A–D, the schema-v5 marker store, the
  config knobs, the dry-run report shape, and the consolidated safety checklist (its §13).
- Visibility spec — [`docs/LCM-PAYLOAD-VISIBILITY.md`](LCM-PAYLOAD-VISIBILITY.md)
  (`t_0ab1c041`). Normative for the canonical `payload_health` data model and the
  healthy/warning/error classification; its §12 defines the acceptance **fixtures** this
  plan turns into concrete cases.

Where this plan and a spec disagree, **the spec wins** — this plan only fixes *case
identifiers, fixtures, and observables*, never semantics. All `file:line` references are
anchors against the tree at design time, not invariants.

---

## 1. Purpose & scope

**In scope.** Unit and integration tests that prove the GC lifecycle and the visibility
surfaces behave per the three specs: the delete primitive's safe order, hash gate, and
idempotency; the four reap phases' enumeration/grace/recovery behavior; the filesystem
hardening (symlink, traversal, containment, O_NOFOLLOW); crash/convergence windows;
config bounds; dry-run vs apply; and the dashboard/doctor/MCP health reporting plus its
state classification and dry-run gate.

**Out of scope** (owned elsewhere): the GC algorithm itself (`t_bbd369f2`'s design), the
visibility data model (`t_0ab1c041`'s spec), age/content retention policy, cross-store
or cross-profile GC, soft-delete/body recovery. Performance/load tests are a follow-up;
this plan targets correctness and safety.

The plan is written so each case is directly transcribable into a `#[tokio::test]` in a
new `src/sessions/lcm/gc.rs` test module (or the existing inline modules). Case IDs are
stable strings (`DEL-001`, `FS-002`, …) so the implementation task can reference them in
commit messages, code comments, and the PR checklist.

---

## 2. Testability hooks the implementation MUST expose

The GC design (its §16) commits to a set of deterministic hooks. These are
**preconditions for this plan** — tests cannot assert grace/crash behavior without them.
If any is missing, the implementation task is not done; this plan's cases will fail to
compile, which is the intended signal.

| Hook | Required shape | Why tests need it |
|---|---|---|
| **Injected clock** | `run_payload_gc(conn, root, provider, session, cfg, now: i64)` and each phase takes `now`. Never calls `current_timestamp()` internally. | Advance time past grace/two-scan/missing windows instantly, no sleeping. |
| **`DeleteOpts` flags** | `remove_file: bool` + `verify_hash: bool` on `delete_external_payload` (`GC.md` §3.1). | `remove_file=false` exercises the DB-only path and the crash-between-commit-and-remove window without removing the file; `verify_hash=false` covers the missing-file path. |
| **`DeleteOutcome`** | `{ metadata_row_existed, file_existed, file_removed, placeholders_rewritten, bytes_freed }` | Distinguishes "nothing to do" from "reaped N bytes"; lets tests assert the exact reap result. |
| **`LcmGcReport`** | The `GC.md` §11 report struct returned by `run_payload_gc` | Primary assertion surface: per-phase counts/bytes/refs + `errors` + `deferred`. |
| **Marker-table access** | Direct read of `lcm_gc_marks(ref, state, first_seen_at)` and `lcm_gc_meta(key, value)` from the test conn | Assert the two-scan state: marked-but-not-yet-reaped, mark-cleared-on-rereference, `last_gc_at` persisted. |
| **Shared reference-set helper** | `referenced_payload_refs` extracted to `pub(crate)` (`doctor.rs:498` today) | Tests assert GC and doctor agree (OM-2); also the seed-fixture's reference-set source of truth. |
| **Phase entry points** | `reap_orphan_files` / `reap_unreferenced_metadata` / `reap_missing_metadata` / `rewrite_dangling_placeholders` reachable individually (or via a per-phase config switch) | Unit-test each phase in isolation without driving the whole `run_payload_gc` ordering. |
| **Tombstone helper** | `tombstone_placeholder_in_text(text, ref) -> text` as a pure function (`GC.md` §8) | Unit-test the rewriter for all prefixes, idempotency, multi-column, nested without a DB. |
| **Safe-delete seam** | `safe_remove_payload_file(dir, name)` callable directly (`GC.md` §7.3) | Unit-test the symlink/traversal/containment gates without a full GC run. |

`LcmGcConfig` (`GC.md` §9) must be constructible with `Default` and `Builder`-style fields
so tests set grace/missing-after/enable/batch/backup knobs directly; serde defaults are
verified separately (`CFG-004`).

---

## 3. Fixtures & tempdir strategy

### 3.1 Test store harness (shared builder)

All integration cases build a store the same way, mirroring the existing pattern in
`src/sessions/lcm/doctor.rs` (`mod tests`, `clean_apply_backup_callback_runs_under_immediate_transaction`,
`doctor.rs:1487`):

1. `temp = tempfile::tempdir()`; `storage_root = temp.path()` (a `tempfile` dev-dep, `Cargo.toml:162`).
2. Open the store exactly as production does: `GlobalDb::open_at(&db_path)` then a
   `libsql::Builder::new_local(db_path)` connection; set `busy_timeout`. The payload dir
   `<root>/lcm-payloads` is created lazily by `prepare_payload_dir` (`payload.rs:342`) on
   first `write_external_payload`.
3. `ensure_lcm_schema(&conn)` runs the v5 DDL (`lcm_gc_marks`, `lcm_gc_meta`) — its guard
   (`schema.rs:91`) makes this safe to call at the top of every test.

Provide a `test_store()` helper returning `{ temp, storage_root, conn, provider }`, plus
**named seed/planthelpers** that build on the real primitives (never raw `fs::write` where
the production path matters):

| Helper | Built from | Produces |
|---|---|---|
| `seed_referenced_payload(content)` | `payload::write_external_payload` + `upsert_payload_metadata` + a `lcm_raw_messages` row (`storage_kind='external'`, `payload_ref`) | A **live** payload: file + row + raw reference. |
| `seed_placeholder_ref(ref)` | A raw row whose `content`/`snippet_text`/`index_text`/`metadata_json` carries an `[externalized payload: … ref=<ref>]` bracket | A placeholder-bearing message citing `ref`. |
| `plant_orphan_file(name, bytes, mtime)` | `write_private_file` under the canonical dir, then backdate mtime | A file with **no** `lcm_external_payloads` row. |
| `plant_corrupted_payload(ref)` | Seed a referenced payload, then overwrite the file bytes with a different length | On-disk SHA-256 ≠ stored `content_hash`. |
| `plant_missing_payload(ref)` | Seed a referenced payload, then `fs::remove_file` | Row + live ref present, file gone. |
| `plant_dangling_placeholder(ref)` | `seed_placeholder_ref(ref)` with **no** metadata row and no file | A `[externalized …]` bracket with nothing behind it. |
| `plant_tombstoned(ref)` | Seed a referenced payload, then rewrite the raw bracket to `[gc'd externalized payload: …]` | A reaped-but-cited ref (the post-reap state). |
| `plant_symlink(target, name)` | `std::os::unix::fs::symlink` under the payload dir | A symlink named `name` pointing at `target`. |
| `advance_clock(now, delta)` | returns `now + delta` | Crossing grace/two-scan windows. |

**mtime policy.** Orphan grace is `now − mtime` (`GC.md` §6.1). Prefer **advancing the
injected `now` clock** past grace (no FS mutation, deterministic) over backdating mtime.
Where mtime must be set explicitly (e.g. asserting the *file's own* age is used, not the
DB clock), use `filetime` (add to `[dev-dependencies]`) or `std::fs::File::set_modified`
(stable). Each such case names which knob it uses.

**Provider/session scoping.** Default scope is `provider='cursor'`, `session_id=None`
(whole-store), matching the doctor/status default. Scoped variants pass an explicit
`session_id`; the plan calls out scoping only where it changes behavior (`VIS-006`).

### 3.2 Cross-platform notes

The symlink / `O_NOFOLLOW` cases (`FS-*`) are gated `#[cfg(unix)]`. Linux-only assertions
on `O_NOFOLLOW` (`private_file_options`, `payload.rs:456`) use `#[cfg(target_os =
"linux")]`. Non-Unix targets fall back to the lstat gate; the plan marks these cases
`cfg(unix)` and notes the non-Unix expectation is "symlink rejected via lstat" rather
than "open fails ELOOP".

---

## 4. Test case catalog

Conventions: **Spec ref** cites the normative section (`LIFECYCLE §x`, `GC §x`,
`VIS §x`). **Pass** = the assertions that must hold; **Fail** = what a broken
implementation would produce (the red signal a reviewer watches for). Every integration
case starts from `test_store()` unless noted.

### 4.1 Delete primitive — `delete_external_payload` (LIFECYCLE §6.1, GC §3)

| ID | Case | Setup | Action | Pass | Fail / red signal | Spec ref |
|---|---|---|---|---|---|---|
| **DEL-001** | Live reaped in safe order (D-2) | `seed_referenced_payload(P)` | `delete_external_payload(conn, root, P.ref, {remove_file:true, verify_hash:true})` (after dropping the raw reference — see `DEL-005`) | Row deleted then file removed; `DeleteOutcome{metadata_row_existed:true, file_existed:true, file_removed:true, bytes_freed>0}`; no row, no file remain. | File removed before commit (crash-window data loss); or outcome lies about `file_removed`. | LIFECYCLE §6.1 D-2; GC §3.2 |
| **DEL-002** | DB-only path leaves file (test/doctor seam) | `seed_referenced_payload(P)` | `delete_external_payload(.., {remove_file:false, verify_hash:false})` after row is unreferenced | Metadata row deleted; **file remains** on disk; `file_removed == false`, `bytes_freed == 0`. | `remove_file=false` still deletes the file, or outcome reports a removal it did not perform. | GC §3.1, §16 |
| **DEL-003** | Idempotent no-op success (D-3) | Run `DEL-001` | Call `delete_external_payload(.., P.ref, ..)` a second and third time | Each call `Ok(DeleteOutcome{metadata_row_existed:false, file_existed:false, file_removed:false, bytes_freed:0, placeholders_rewritten:0})`; no error. | Second call returns `Err`; or reports phantom reap. | LIFECYCLE §6.1 D-3, §8; GC §3.1 |
| **DEL-004** | Hash gate aborts corrupted file (D-4) | `plant_corrupted_payload(P)`; keep its raw reference | `delete_external_payload(.., {remove_file:true, verify_hash:true})` | `Err(PayloadIntegrityMismatch)`; **row + file both untouched**; doctor still classifies it as integrity-mismatch; not reaped by a subsequent `run_payload_gc(apply=true)`. | Corrupted file silently deleted; or reap proceeds past the gate. | LIFECYCLE §6.1 D-4, §9; GC §3.2, §13 #6 |
| **DEL-005** | Still-referenced aborts reap | `seed_referenced_payload(P)` (raw reference intact) | `delete_external_payload(.., P.ref, ..)` opening `BEGIN IMMEDIATE` | `Err(StillReferenced)` (or equivalent); row + file untouched; placeholder unchanged. | A referenced payload is reaped (data loss). | LIFECYCLE §5 live←unref edge, §10.3; GC §3.2 3a |
| **DEL-006** | Tombstones residual placeholders | `seed_placeholder_ref(P.ref)` + `seed_referenced_payload(P)`; drop the whole-message ref leaving only the placeholder | `delete_external_payload(.., {rewrite_placeholders:true, remove_file:true, verify_hash:true})` | Bracket rewritten to `[gc'd externalized payload: … ref=P.ref …]`; `placeholders_rewritten >= 1`; body after the prefix preserved (`ref=…;…`). | Placeholder left live (expand would still try to read bytes); or prefix mangled. | LIFECYCLE §6.1, §9 MF-1; GC §3.3, §8 |
| **DEL-007** | Invalid ref rejected up front | — | `delete_external_payload(.., "../evil", ..)` and `("", "x.payload")` | `Err(InvalidPayloadRef)`; no FS/DB access attempted. | A bad ref reaches a path operation. | LIFECYCLE §13; GC §3.2 step 1 |

### 4.2 Filesystem enumeration & deletion safety — `safe_remove_payload_file` (GC §7, LIFECYCLE §10.1–2, §13)

All `FS-*` are `#[cfg(unix)]`; `O_NOFOLLOW`-specific checks are `#[cfg(target_os="linux")]`.

| ID | Case | Setup | Action | Pass | Fail / red signal | Spec ref |
|---|---|---|---|---|---|---|
| **FS-001** | Symlink in-dir ignored at enumerate | `plant_symlink("/etc/passwd", "payload_00…01.payload")`; valid-shape name | `safe_remove_payload_file(dir, name)` + enumerate via Phase A | Symlink **not** followed; reported as skipped anomaly; never opened; `read_dir` enumeration rejects it (lstat, not `entry.metadata()`). | Symlink target read/deleted (arbitrary file removal). | GC §7.2, §13 #3; LIFECYCLE §13 |
| **FS-002** | Symlink swap between validate and unlink | Plant a real file `name`; run enumerate to mark candidate; **replace it with a symlink**; then `safe_remove_payload_file` | Re-lstat inode ≠ opened fd (or `O_NOFOLLOW` `ELOOP`) | Removal aborted; original target untouched; anomaly recorded; run continues. | TOCTOU: swapped symlink followed to its target and deleted. | GC §7.3; LIFECYCLE §13 |
| **FS-003** | `..` traversal name rejected | File literally named `..` or `payload_../../etc/x.payload` is not creatable; assert via `validate_payload_ref` + enumerate skip | Enumerate candidate set | Name rejected by `validate_payload_ref` (`payload.rs:56`); never `join`ed to the dir. | A path with `..` escapes the canonical dir. | LIFECYCLE §10.1, §13; GC §7.1 |
| **FS-004** | Absolute-path injection rejected | Try `delete_external_payload(.., "/etc/passwd", ..)` and a candidate whose name is absolute | Primitive + enumerate | `Err(InvalidPayloadRef)`; enumerate skips. | Absolute path reaches `remove_file`. | LIFECYCLE §10.1, §13; GC §7.1 |
| **FS-005** | File outside root via symlinked dir | Make `lcm-payloads` reachable through a symlinked parent; plant a valid-name file under it | `existing_payload_dir` + enumerate | `canonical_storage_root` rejects the symlink root (`payload.rs:366`); dir-under-root check (`payload.rs:385`) fails; nothing enumerated. | Symlinked storage root accepted. | GC §7.2; LIFECYCLE §13; `VIS §8` `root_contained` |
| **FS-006** | Dir masquerading as `*.payload` | `fs::create_dir(dir.join("payload_00…02.payload"))` | Enumerate + `safe_remove_payload_file` | lstat says not a regular file → skipped; never treated as reaped. | A directory is `remove_file`d (error) or recursed. | GC §7.2; LIFECYCLE §10.1 |
| **FS-007** | Non-owner-hash stray name ignored | Plant `editor-swap.tmp`, `.DS_Store`, `payload_DEADBEEF.payload` (bad length) | Enumerate | None enumerated; name regex `^payload_[0-9a-f]{64}\.payload$` excludes them. | Stray/editor files reaped. | GC §7.1, §13 #2; LIFECYCLE §10.1 |
| **FS-008** | Containment re-checked immediately before remove | Plant a valid orphan; enumerate candidate; mutate dir between (best-effort) | `safe_remove_payload_file` | `ensure_contained(dir, path)` (`payload.rs:396`) re-asserts `parent == canonical dir` right before `remove_file`. | remove_file on a path whose parent is no longer the canonical dir. | LIFECYCLE §10.2; GC §7.3 |

### 4.3 Phase A — orphan files (GC §6.1, GP-2 mtime grace)

| ID | Case | Setup | Action | Pass | Fail | Spec ref |
|---|---|---|---|---|---|---|
| **PHA-001** | Orphan within grace not reaped | `plant_orphan_file(O1, now=now)` | `reap_orphan_files(.., now, grace=86400, apply=true)` | Orphan remains; report lists it as deferred/within-grace; `reclaimable_bytes_after_grace` includes it but `reclaimable_bytes` does not. | New orphan reaped immediately (races concurrent ingest). | LIFECYCLE §6.3 GP-1/GP-2; GC §6.1 |
| **PHA-002** | Orphan past grace reaped (mtime clock) | `plant_orphan_file(O1)` with `mtime = now − 90000` | same, `apply=true` | File removed; `orphans.count==1`, `orphans.bytes==len`; **no** row ever existed (assert `metadata_refs` unchanged). | Orphan left forever, or a row fabricated for it. | LIFECYCLE §6.3 GP-2; GC §6.1, §11 |
| **PHA-003** | Dry-run lists but does not remove | `plant_orphan_file(O1)` past grace | `reap_orphan_files(.., apply=false)` | Report lists O1 + bytes; **file still present**; no txn opened. | Dry-run mutates state. | LIFECYCLE §10.6; GC §4, §11 |
| **PHA-004** | Orphan with an owner row is skipped | `seed_referenced_payload(P)` | enumerate | P's file not enumerated as orphan (in `metadata_refs`). | A live payload's file classified orphan. | GC §6.1; LIFECYCLE OM-2 |

### 4.4 Phase B — unreferenced metadata (GC §5.3, §6.2, two-scan rule)

| ID | Case | Setup | Action | Pass | Fail | Spec ref |
|---|---|---|---|---|---|---|
| **PHB-001** | First scan marks, does not reap | `seed_referenced_payload(P)`, then drop the raw ref (→ unreferenced) | `run_payload_gc(.., now=T0, apply=true)` (first run) | `lcm_gc_marks(P.ref).state=='unreferenced'`, `first_seen_at==T0`; row + file **intact**; report shows unreferenced not-yet-eligible. | Reaped on first observation (zero effective grace). | LIFECYCLE §6.3 GP-2; GC §5.3 |
| **PHB-002** | Second scan past grace reaps | continue `PHB-001` | `run_payload_gc(.., now=T0+grace, apply=true)` | `delete_external_payload` reaps P; mark cleared; row + file gone; placeholder tombstoned (if any). | Never reaped, or reaped before grace elapsed. | GC §5.3, §6.2 |
| **PHB-003** | Re-referenced clears mark (live←unref) | continue `PHB-001` | Re-add a raw ref to P; `run_payload_gc(.., now=T0+grace)` | Mark **deleted**; P stays live; a *new* unreferenced period restarts the clock (`first_seen_at` resets on next scan). | A re-referenced payload reaped; or stale mark survives. | LIFECYCLE §5 edge, §10.3; GC §5.3 |
| **PHB-004** | Two-scan state survives restart | `PHB-001` | Simulate restart (reopen conn on same `db_path`); run again at `T0+grace` | Reap still eligible — mark persisted in DB (not a sidecar). | Mark lost across restart → grace resets indefinitely. | GC §5.1; LIFECYCLE §8 |
| **PHB-005** | Hash gate fires mid-reap | `plant_corrupted_payload(P)` then drop ref; mark it; advance grace | reap | `Err(PayloadIntegrityMismatch)` recorded in `report.errors`; P row+file untouched; run continues; `last_gc_status=='partial'` (not aborted). | Corrupted reaped, or one bad ref aborts the whole run. | GC §6.2, §10 per-ref isolation; LIFECYCLE §9 |

### 4.5 Phase C — missing metadata (GC §6.3, GP-4)

| ID | Case | Setup | Action | Pass | Fail | Spec ref |
|---|---|---|---|---|---|---|
| **PHC-001** | Missing reported, never auto-reaped by default | `plant_missing_payload(P)` (ref still live); `reap_missing_enabled=false` | `run_payload_gc(.., apply=true)` repeatedly across `reap_missing_after` | P **never** reaped; reported in `missing`; status `missing_count==1`, state **error**. | Missing auto-tombstoned on normal grace. | LIFECYCLE §6.3 GP-4, §9; GC §6.3 |
| **PHC-002** | Opt-in tombstone after window | `plant_missing_payload(P)`; `reap_missing_enabled=true`, `reap_missing_after=604800` | mark at `T0`; advance; reap at `T0+reap_missing_after` | `delete_external_payload(.., {remove_file:false, verify_hash:false})` deletes row + tombstones ref; `expand` → `PayloadGc'd` (MF-1). | Reaped before the long window, or with `verify_hash:true` (no file → spurious mismatch). | GC §6.3; LIFECYCLE §9 MF-1 |
| **PHC-003** | File reappears → back to live | `plant_missing_payload(P)` with mark; `fs::write` the file back; reap | mark cleared (present file + ref ⇒ live); not reaped. | A restored payload reaped because of a stale mark. | LIFECYCLE §5; GC §6.3 recovery edge |

### 4.6 Phase D — dangling placeholders (GC §6.4, §8)

| ID | Case | Setup | Action | Pass | Fail | Spec ref |
|---|---|---|---|---|---|---|
| **PHD-001** | Dangling placeholder tombstoned | `plant_dangling_placeholder(R)` | `rewrite_dangling_placeholders(.., apply=true)` | Bracket → `[gc'd externalized …]`; no row, no file; message still parses; `expand` on R → `PayloadGc'd`. | Placeholder left dangling; or a file/row fabricated. | LIFECYCLE §9; GC §6.4, §8 |
| **PHD-002** | Tombstone rewrite idempotent | `plant_tombstoned(R)` + run `PHD-001` twice | second run | `placeholders_rewritten==0` second time; text unchanged. | Double-prefix (`[gc'd [gc'd …]`), or non-idempotent. | LIFECYCLE §8; GC §8 |
| **TS-001** | Tombstone helper: all live prefixes | unit test `tombstone_placeholder_in_text` | feed `[externalized payload:`, `[externalized lcm ingest payload:`, `[externalized tool output:` citing R | each → its `[gc'd …]` counterpart; body after prefix preserved. | A prefix not recognized; body truncated. | GC §8; `payload.rs:104` |
| **TS-002** | Tombstone helper: multi-column + repeated ref | a row citing R in `content` and `index_text`, twice in one column | rewrite via the helper | every occurrence rewritten; `placeholders_rewritten` counts all. | Only the first occurrence rewritten. | GC §8 (d) |
| **TS-003** | `PayloadGc'd` vs `PayloadMissing` distinction (MF-1) | tombstoned R vs missing M | `expand(R)` / `expand(M)` | `Err(PayloadGc'd)` vs `Err(PayloadMissing)` respectively. | Both return `PayloadMissing`/`PayloadNotFound` (ambiguous). | LIFECYCLE §9 MF-1; GC §8 |

### 4.7 Orchestration: grace, batching, convergence, concurrency (GC §4, §5, §10; LIFECYCLE §8, §10, §12)

| ID | Case | Setup | Action | Pass | Fail | Spec ref |
|---|---|---|---|---|---|---|
| **GC-001** | Full run dry-run default | any non-empty fixture | `run_payload_gc(.., apply=false)` (default) | `report.status=='dry_run'`; **zero** mutations; `lcm_gc_meta.last_gc_dry_run_at` set, `last_gc_at` unchanged. | Dry-run writes `last_gc_at` or mutates. | LIFECYCLE §10.6; GC §4, §11 |
| **GC-002** | Idempotent repeated apply | a fixture with ≥1 reapable orphan past grace | `run_payload_gc(apply=true)` twice | First run reaps; second run reaps **nothing new** (`orphans/unreferenced/missing/dangling==0`); `last_gc_at` updated both times. | Second run re-reaps, or errors on the empty set. | LIFECYCLE §8; GC §10 |
| **GC-003** | Convergence after mid-reap crash | `seed_referenced_payload(P)`, drop ref, mark at T0; advance grace | `delete_external_payload(.., {remove_file:false})` (commit row, **leave file**) to simulate crash in the commit→remove window; then `run_payload_gc(apply=true)` | Phase A reaps the now-orphan file by mtime; final state == clean reaped state; no error, no double work. | Orphan file left forever, or a "missing file behind live ref" created. | LIFECYCLE §8, §12; GC §3.2 D-2, §10 |
| **GC-004** | Partial-tombstone completion | seed a message citing R in 3 columns; `delete_external_payload(rewrite_placeholders:true)` interrupted after 1 column (use a seam or two runs) | re-run Phase D | All 3 columns tombstoned; idempotent. | Half-tombstoned message stays half-done. | LIFECYCLE §8; GC §8, §10 |
| **GC-005** | Two processes GC the same store | two conns on the same `db_path` | both attempt reap of the same ref under `BEGIN IMMEDIATE` | Only one reaps; the loser no-ops (`Err` swallowed / row already gone); final state correct, no panic/deadlock. | Double-delete error; or lost-update. | LIFECYCLE §12; GC §10 |
| **GC-006** | Batch cap defers remainder | `cfg.gc_max_batch_size=2`; 5 reapable orphans past grace | `run_payload_gc(apply=true)` | Reaps exactly 2; `report.deferred.count==3`, `reason=='batch_cap'`; a second run reaps 2 more. | All 5 reaped ignoring the cap (long lock hold). | GC §9, §10, §11 |
| **GC-007** | Per-ref error isolation | 1 corrupted unreferenced + 2 healthy orphans past grace | `run_payload_gc(apply=true)` | The 2 orphans reaped; corrupted ref recorded in `report.errors`; `status` reflects partial success; run not aborted. | One bad ref stops the whole run. | GC §10 |
| **GC-008** | Concurrent ingest during scan | mark R unreferenced at T0; **between** scan and reap insert a new raw ref to R; reap at T0+grace | reap | Referenced-ness re-checked under `BEGIN IMMEDIATE` ⇒ R skipped as `live`; mark cleared. | R reaped despite a new reference (race lost to ingest). | LIFECYCLE §12, §10.3; GC §3.2 3a, §10 |
| **GC-009** | In-flight open fd during reap | `seed_referenced_payload(P)`, drop ref, mark, advance; **hold an open read fd** on P's file; reap | reap | File removed (unlink under open fd is fine on Unix); reader keeps its fd; outcome correct; no EBADF-style panic. | Reap refuses/panics on an open file, or corrupts the reader. | GC §6.1, §10; LIFECYCLE §12 |
| **GC-010** | Backup-before-mutate runs | `cfg.backup_before_reap=true`, `apply=true` | run | `checkpoint_wal_for_backup` + `backup_database` invoked once before reaping; a backup artifact exists under root (mirror `doctor.rs:159`). | No backup before a destructive apply. | LIFECYCLE §13; GC §4, §13 #10 |
| **GC-011** | No-FS-IO-under-lock invariant | instrument / assert | run with apply | each `BEGIN IMMEDIATE` opens only for the DB decision+delete; `remove_file` happens **after** commit. | `remove_file` inside the writer txn. | LIFECYCLE §10.7; GC §10, §13 #12 |
| **GC-012** | Phase ordering A→B→C→D | orphan file O (no row) whose ref is *also* cited by a dangling placeholder | `run_payload_gc(apply=true)` | A reaps O; D then tombstones the now-dangling placeholder; no cross-phase double-count in totals. | Phases run out of order causing a transient dangling reference to be misclassified. | GC §4 ordering rationale |

### 4.8 Config knobs (GC §9; LIFECYCLE §6.3 GP-1)

| ID | Case | Setup | Action | Pass | Fail | Spec ref |
|---|---|---|---|---|---|---|
| **CFG-001** | Grace floor 300s enforced | `cfg.grace_seconds=0` (and `=10`) | build config | clamps to **300**; an orphan 200s old is **not** reaped at `grace_seconds` would-be 0. | Near-zero grace races concurrent ingest. | LIFECYCLE §6.3 GP-1; GC §9 |
| **CFG-002** | `reap_missing_enabled=false` is default | `Default::default()` | inspect | `reap_missing_enabled==false`; missing never reaped (see `PHC-001`). | Missing auto-reaped out of the box. | GC §9; LIFECYCLE §6.3 GP-4 |
| **CFG-003** | `reap_missing_after=0` ⇒ never | `reap_missing_enabled=true`, `reap_missing_after=0` | advance any time | never reaps missing. | Reaps missing immediately. | GC §9 |
| **CFG-004** | serde defaults match §9 table | `LcmGcConfig::default()` | serde round-trip | grace 86400, reap_missing_after 604800, batch 500, backup true, gc_enabled true, interval 21600. | Defaults drift from the spec table. | GC §9 |
| **CFG-005** | `gc_enabled=false` blocks scheduled-only | `gc_enabled=false` | scheduled-style invocation | skipped; **but** manual `doctor mode=gc apply=true` still works. | Master switch also blocks the operator override. | GC §9 |

### 4.9 Dry-run vs apply (GC §11; LIFECYCLE §10.6; VIS §9)

| ID | Case | Setup | Action | Pass | Fail | Spec ref |
|---|---|---|---|---|---|---|
| **DRY-001** | Dry-run report shape | orphan(2)+unreferenced marked(1)+missing(0)+dangling(1) | `run_payload_gc(apply=false)` | JSON matches `GC.md` §11 shape: `status=='dry_run'`, per-phase `{count,bytes,refs}`, `refs` capped at 20, `deferred`, `errors`; **no body bytes / no snippet text** anywhere. | Report shape drifts; or bodies leak. | GC §11, §13 #1; LIFECYCLE §13 |
| **DRY-002** | Apply reaps exactly the dry-run set | same fixture | `run_payload_gc(apply=false)` then `(apply=true)` | The refs in the dry-run `orphans/unreferenced` removal sets == the refs actually removed by apply; byte totals match. | Apply reaps a different set than the dry-run advertised. | LIFECYCLE §10.6; GC §11 |
| **DRY-003** | Dry-run excludes within-grace & missing & corrupted | orphan within grace + unreferenced not-yet-eligible + missing + corrupted | `run_payload_gc(apply=false)` | Those refs are **absent** from the removal set (reported separately / in deferred / errors / missing). | Dry-run advertises a destructive set it would not actually touch. | LIFECYCLE §10.6; GC §11; VIS §12.2 |

### 4.10 Visibility: status / doctor / dashboard (VIS §3–§12)

These implement the VIS §12 acceptance fixtures as runnable cases. They assume the
implementation consumes the `t_0ab1c041` data model (`payload_health` block, classifier,
endpoints).

| ID | Case | Setup | Action | Pass | Fail | Spec ref |
|---|---|---|---|---|---|---|
| **VIS-001** | Healthy fixture (VIS §12.1) | N referenced, all present + hash-correct, no orphans, GC run ok | `lcm_status(deep=false)` & `(deep=true)`; dashboard `GET …/overview` + `GET …/payloads/health` | `missing==0, unreferenced==0, orphan_file==0, integrity==null(deep=false)/0(deep=true)`; `payload_health.status=='healthy'`; `reclaimable_bytes==0`; `last_gc_status=='ok'`. | Healthy misclassified warning/error; integrity null vs 0 conflated. | VIS §12.1, §8 |
| **VIS-002** | Warning fixture (VIS §12.2) | 2 orphans (1 past grace), 3 unreferenced marked (2 past grace), no missing | same | `orphan_file_count==2, unreferenced_count==3`; `reclaimable_bytes==bytes(2 past-grace unreferenced)+bytes(1 past-grace orphan)`; `reclaimable_bytes_after_grace==bytes(3)+bytes(2)`; `status=='warning'`; dry-run lists exactly the now-eligible refs. | Reclaimable-now vs after-grace math swapped; within-grace item in the removal set. | VIS §12.2, §3.3 |
| **VIS-003** | Error: missing (VIS §12.3) | 1 referenced payload `rm`'d | `lcm_status` + drill-down + `POST …/payloads/gc` (apply) | `missing_count==1`; `status=='error'`; drill-down "Missing" shows ref + owner; apply does **not** reap it (GP-4); dry-run excludes it. | Missing reaped on apply; or hidden behind a ratio. | VIS §12.3, §8; LIFECYCLE §9 |
| **VIS-004** | Error: integrity mismatch (deep) | `plant_corrupted_payload(P)`; `deep=true` | `lcm_status(deep=true)` + apply | `integrity_mismatch_count==1`; `status=='error'`; apply does **not** remove it (D-4). | Corrupted reaped; or not surfaced with `deep=true`. | VIS §12.3, §3.1; LIFECYCLE §9 |
| **VIS-005** | Cross-surface numeric agreement (VIS §12.5) | the warning fixture | compare `lcm_status.payload` vs `lcm_doctor diagnostics.payloads` vs dashboard `payload_health` | identical counts **and** byte totals (`orphan_file_count`, byte sums); computed from shared helpers (assert via the same `referenced_payload_refs`). | status ≠ doctor ≠ dashboard (drift). | VIS §5.3, §12.5; OM-2 |
| **VIS-006** | Scope filters are consistent | seed payloads across 2 sessions | `lcm_status(session_id=S1)` vs `(None)` | scoped counts ⊆ whole-store counts; orphan detection uses `all_payload_metadata_refs` for whole-store and the scoped set otherwise (mirror `doctor.rs:342`). | Scope miscounts orphans (uses scoped set for whole-store). | VIS §6.2; `doctor.rs:342` |
| **VIS-007** | Dry-run gate: POST without preview → 400 (VIS §12.4) | any fixture | `POST …/payloads/gc` with no prior dry-run token / `confirm` | HTTP **400**; no mutation. | Destructive POST allowed without a preview. | VIS §6.3, §12.4 |
| **VIS-008** | Dry-run gate: after preview, apply reflects reaped (VIS §12.4) | warning fixture | `GET …/payloads/gc` (dry-run token) → `POST` with token | apply succeeds; `last_reaped_refs/bytes` match; `last_gc_status=='ok'`. | Apply ignores the token or misreports reaped totals. | VIS §12.4 |
| **VIS-009** | `gc` doctor mode (VIS §5.2) | warning fixture | `lcm_doctor(mode='gc', apply=false)` then `(apply=true)` | dry-run == reaper report; apply reaps; `lcm_gc_apply_enabled` gate honored (default off). | `gc` mode missing or ungated. | VIS §5.2; GC §12 |
| **VIS-010** | GC-not-yet-run fallback (VIS §3.3) | fresh store, GC never run | `lcm_status` + overview | `last_gc_at==None`, `lcm_gc` capability false; UI renders "GC not yet run"; `reclaimable_bytes==0`, `reclaimable_bytes_after_grace==unreferenced+orphan bytes`. | Renders as "nothing reclaimable". | VIS §3.3, §6.4 |
| **VIS-011** | `root_contained==false` ⇒ error | make payload dir reachable outside root (symlink) | `lcm_status` | `root_contained==false`; `status=='error'`. | Containment break classified healthy. | VIS §8; LIFECYCLE §13 |
| **VIS-012** | No body bytes in any surface | any fixture incl. corrupted/missing | serialize every response (status/doctor/GC dry-run/apply/overview/payloads-health) + logs | grep for any payload body substring / snippet text: **none** present; only refs, counts, byte totals, stored `content_hash`. | Body bytes / snippets leak into a response or log. | LIFECYCLE §13; GC §13 #1; VIS §11 |

---

## 5. Coverage matrix — required areas × cases

Maps the ten coverage areas the task body mandates to the case IDs above (and the
GC §13 / LIFECYCLE §10 invariants they pin). "Primary" = the case that most directly
exercises the area; "also" = secondary coverage.

| Required area | Primary cases | Also | Invariants pinned |
|---|---|---|---|
| DB row deletion with file cleanup semantics | `DEL-001`, `DEL-002`, `DEL-006` | `GC-011`, `GC-012` | D-2 safe order; §10.7; §13 #5/#12 |
| Orphan file detection | `PHA-001`..`PHA-004`, `FS-007` | `VIS-002`, `VIS-005` | §6.1; GP-2 mtime; §7.1 name gate |
| Missing file handling | `PHC-001`..`PHC-003`, `TS-003` | `VIS-003`, `DEL-004` | §9; GP-4; MF-1 |
| Symlink rejection | `FS-001`, `FS-002`, `FS-005` | `VIS-011` | §7.2; §13 #3 |
| Path traversal attempts | `FS-003`, `FS-004`, `DEL-007` | `FS-008` | §10.1; §13; `validate_payload_ref` |
| Files outside the payload root | `FS-005`, `FS-008`, `VIS-011` | `DEL-007` | §10.2; `ensure_contained`; `canonical_storage_root` |
| Concurrent / in-flight writes | `GC-005`, `GC-008`, `GC-009` | `PHB-003` | §10.3; §12; §10 per-ref txn |
| Idempotent repeated GC | `DEL-003`, `GC-002`, `PHD-002` | `GC-003`, `GC-004` | §8; §13 #9 |
| Dry-run vs destructive mode | `DRY-001`..`DRY-003`, `PHA-003`, `GC-001` | `VIS-007`, `VIS-008`, `VIS-009` | §10.6; §11; §13 #11 |
| Dashboard / doctor health reporting | `VIS-001`..`VIS-012` | — | VIS §3/§8/§12; OM-2 |

Every consolidated GC §13 invariant has at least one case:

| GC §13 invariant | Cases |
|---|---|
| 1 no body bytes | `VIS-012`, `DRY-001` |
| 2 enumerate only `payload_*.payload` | `FS-007`, `PHA-004` |
| 3 never follow symlink | `FS-001`, `FS-002`, `FS-005` |
| 4 never delete outside canonical dir | `FS-008`, `DEL-007`, `FS-004` |
| 5 safe removal order (D-2) | `DEL-001`, `GC-003`, `GC-011` |
| 6 hash gate (D-4) | `DEL-004`, `PHB-005`, `VIS-004` |
| 7 re-check referenced-ness under txn | `DEL-005`, `PHB-003`, `GC-008` |
| 8 grace honored (mtime/two-scan/missing; 300s floor) | `PHA-001/2`, `PHB-001/2`, `PHC-001/2`, `CFG-001` |
| 9 idempotent + convergent | `DEL-003`, `GC-002/3/4` |
| 10 backup before mutate | `GC-010` |
| 11 dry-run default; destruction opt-in | `GC-001`, `DRY-001/3`, `VIS-007` |
| 12 per-ref `BEGIN IMMEDIATE`, no FS IO under lock, batched | `GC-006`, `GC-011` |
| 13 distinct `PayloadGc'd` error (MF-1) | `TS-003`, `PHC-002`, `PHD-001` |

---

## 6. Pass/fail criteria for the plan itself (acceptance)

This plan is "done" when:

1. **Named cases exist for all ten required areas** — §5 coverage matrix shows ≥1 primary
   case per area, with stable IDs. ✔ (see §5).
2. **Setup expectations are explicit** — §2 lists the testability hooks; §3.1 the store
   harness + named plant helpers; §3.2 the platform gating.
3. **Fixtures/tempdir strategy is concrete** — §3 reuses the existing `doctor.rs` test
   pattern (`tempfile::tempdir` → `GlobalDb::open_at` → `libsql::Builder::new_local` →
   `ensure_lcm_schema`), names the `test_store()` builder and every plant helper, and pins
   the mtime policy (prefer clock injection).
4. **Pass/fail criteria are tied to the specs** — every case cites LIFECYCLE/GC/VIS
   sections; §5 ties the GC §13 invariants to cases; VIS cases are the VIS §12 fixtures
   verbatim.
5. **The implementation task (`t_baa1d2cf`) can consume it directly** — case IDs are
   referenceable; hooks are specified as preconditions; sequencing (§7) tells it what to
   land first.

The **code** (turning these cases into `#[tokio::test]`s) is the implementation task's
deliverable, not this card's. This card's artifact is this document.

---

## 7. Sequencing & dependencies (for `t_baa1d2cf`)

Cases are gated on the implementation steps in `GC.md` §15. Land in this order to keep the
suite green at each step:

1. After **step 1 (schema v5)** + **step 2 (extract `referenced_payload_refs`)**: the
   `test_store()` harness and `FS-001`..`FS-008` (filesystem primitives already exist) can
   land and pass.
2. After **step 3 (`PayloadGc'd`)** + **step 4 (`tombstone_placeholder_in_text`)**:
   `TS-001`..`TS-003`, `PHD-001/2`.
3. After **step 5 (`safe_remove_payload_file` + `delete_external_payload`)**: `DEL-001`..`DEL-007`.
4. After **step 6 (phases + `run_payload_gc`)**: `PHA/PHB/PHC-*`, `GC-001`..`GC-012`, `CFG-*`, `DRY-*`.
5. After **step 7 (`LcmGcReport` + status fields)** + **step 8 (doctor `gc` mode)**:
   `VIS-001`..`VIS-012` (these also consume the `t_0ab1c041` dashboard endpoints/MCP additions).

**Hard dependency:** every `PHB-*`/`PHC-*`/`GC-003/004` case needs the injected-clock
contract (§2); if the implementation calls `current_timestamp()` internally those cases
must be written as backdated-mtime variants and the plan amended.

**Shared with the visibility task:** `VIS-*` cases assume the `payload_health` block,
classifier, and dry-run/apply endpoints from `t_0ab1c041`. They land last and double as
that spec's §12 acceptance evidence.

---

## 8. Open items handed off

- **Clock-injection vs mtime-backdating.** §2 requires injected `now`. If the
  implementation instead reads wall-clock mtime only, `PHB-001/2`, `PHC-002`, `PHA-*` must
  be rewritten to backdate mtime (add `filetime` to dev-deps) — flag to `t_baa1d2cf`.
- **`StillReferenced` vs reuse of an existing variant.** `DEL-005`/`PHB-003` assert a
  still-referenced abort; `LcmError` has no such variant today (`types.rs:726`). The
  implementation should add one (or document the mapped variant) so the assertion is
  specific — tracked as a sibling of MF-1.
- **Dashboard/UI cases are backend-asserted here.** `VIS-001/002/003/007` assert the JSON
  contract (status, pill color via `status` field, 400 gate). Pixel-level UI verification of
  the green/amber/red pill and the dry-run modal is out of scope for this plan; the
  `t_0ab1c041` implementation task should add a visual-QA follow-up for the card/modal.
- **Load/perf follow-up.** This plan is correctness-only; a separate card should add a
  store-scale reap benchmark (10k+ payloads) once v1 lands.
