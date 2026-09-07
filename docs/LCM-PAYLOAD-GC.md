# LCM external payload garbage collection — design

Status: **design / actionable algorithm**. This is the implementation-and-test
spec for payload GC. It is normative for `delete_external_payload`, the reaper,
the GC marker store, the GC config knobs, and the safety invariants.

**Doctor boundary:** `lcm_doctor` is strictly read-only diagnosis and evidence.
It never invokes this reaper and never repairs, applies, cleans, garbage-collects,
retains, relinks, or migrates. The daemon maintenance owner, or an explicitly
authorized owner operation with an active write scope, owns any GC apply run.
Doctor requests require `provider`; `session_id` is optional and scopes the
read-only evidence when supplied.

It is a child of the **lifetime & retention contract**
([`docs/LCM-PAYLOAD-LIFECYCLE.md`](LCM-PAYLOAD-LIFECYCLE.md)) and the
storage/deletion audit (Kanban `t_c2443a7f`, comment 34). The contract defines
*what* a payload's lifetime is and *why*; this document defines *how* GC
reaps safely. Where the two disagree, **the contract wins** — this doc fills in
the algorithm the contract deliberately left open (§10) and must not contradict
its decisions (OM-1/2, D-1..D-4, SD-1/2, GP-1..4, MF-1).

Out of scope here (owned by siblings):
- **Dashboard/doctor visibility fields** (`t_0ab1c041`) — this doc specifies the
  values the reaper *produces*; that task specifies where they render.
- **Test cases** (`t_f0e07c5c`) — this doc specifies the invariants and
  deterministic hooks tests assert against; that task enumerates the cases.

All `file:line` references are anchors against the tree at design time, not
invariants.

---

## 1. Goals & non-goals

**Goals**

1. Introduce the **only** payload-file deletion primitive
   (`delete_external_payload`) and a **daemon-owned reaper** (`run_payload_gc`)
   that reconciles filesystem and DB state toward the lifecycle contract.
2. Reap exactly the contract's collectable states — orphan files,
   unreferenced metadata, missing metadata (after the long window), and
   dangling placeholders — and **never** live or corrupted payloads.
3. Be crash-safe and idempotent: any re-run after any crash converges to a
   clean state with no data loss and no duplicate work.
4. Default owner runs to **dry-run / report**; destruction is opt-in
   (`apply = true`) and requires explicit owner authorization.
5. Emit no payload body bytes anywhere (logs, reports, metrics, errors).

**Non-goals** (inherit the contract §14): age/content retention policy,
cross-store/cross-profile GC, soft-delete/undo/body recovery, cross-message
dedup, reaping summary nodes / lifecycle state / maintenance debt.

---

## 2. Primitives the implementation MUST reuse (do not reinvent)

GC adds a *destructive* capability to a module that today has none. Every
non-destructive building block already exists and is battle-tested by the read
path and the doctor diagnostics. Reusing them is a hard requirement, not a
convenience — they encode the path/ref/symlink safety the contract §13 demands.

| Need | Primitive | Location | Notes for GC |
|---|---|---|---|
| Canonical payload dir (validated, non-symlink, under root) | `existing_payload_dir` | `payload.rs:357` | Currently `fn` (private). Expose as `pub(crate)` or add a GC wrapper. Returns the canonical `<root>/lcm-payloads`; rejects symlink storage root, symlink dir, dir-not-under-root. **Enumerate candidates from this.** |
| Canonical, non-symlink storage root | `canonical_storage_root` | `payload.rs:366` | Reused transitively by `existing_payload_dir`. |
| Dir-under-root containment | `ensure_payload_dir_under_root` | `payload.rs:385` | Canonical `dir.parent() == root`. |
| Ref validation (single normal component, no `.`/`..`/`/`/`\`) | `validate_payload_ref` | `payload.rs:56` | Run on **every** enumerated filename and every reap input. |
| Path-in-dir containment | `ensure_contained` | `payload.rs:396` | `path.parent() == dir`. Re-run immediately before `remove_file`. |
| Symlink/dir rejection for an entry | `ensure_actual_private_dir` | `payload.rs:377` | Pattern to mirror for per-file lstat checks. |
| Private file open (Linux `O_NOFOLLOW`, mode 0600) | `private_file_options` | `payload.rs:456` | Used for hash-verify reads; basis for the safe-delete open (§7.3). |
| Ref extraction from placeholder text | `extract_payload_refs_from_text` | `payload.rs:73` | Used by tombstone rewrite + reference-set. |
| Tombstone-prefix recognition (already accepts `[gc'd …]`) | `is_external_payload_placeholder` | `payload.rs:104` | **No new parser.** Tombstone writer only changes the prefix. |
| Load one metadata row | `load_payload_metadata` | `payload.rs:312` | `pub(crate)`. |
| **Reference-set computation** | `referenced_payload_refs` | `gc.rs` / doctor diagnostics | The canonical "is this ref cited by a live raw row" query (whole-message `payload_ref` **and** placeholders in the 4 text columns). GC and read-only Doctor diagnostics must agree exactly (OM-2). |
| All metadata refs | `all_payload_metadata_refs` | `maintenance.rs` | Set of `payload_ref` PKs. |
| Unreferenced count | `count_unreferenced_payload_metadata` | status/maintenance queries | For status; GC's scan reuses the same sets. |
| Backup before mutate | `checkpoint_wal_for_backup`, `backup_database` | `maintenance.rs` | Reuse for an authorized owner "backup before reap" (§13); Doctor never calls it. |
| Now | `current_timestamp` (`crate::tracedecay`) / SQL `unixepoch()` | — | Marker/`last_gc_at` timestamps. |

**New symbols this design introduces** (implementation task): `delete_external_payload`,
`run_payload_gc` + phase functions, `tombstone_placeholder_in_text`, a shared
`referenced_payload_refs` (extracted), `LcmGcConfig`, `LcmGcReport`, the marker
tables, and `LcmError::PayloadGc'd`.

---

## 3. The deletion primitive — `delete_external_payload`

Synchronous, payload-aware, the **only** code permitted to `remove_file` a
payload. Implements contract §6.1 (D-1..D-4) and is reused by the reaper, by any
future session/message delete API, and by an explicitly authorized owner delete
operation. Doctor diagnostics never invoke it.

### 3.1 Signature

```rust
// crates/tracedecay-sessions/src/runtime/lcm/payload.rs (or a new gc.rs)
pub(crate) async fn delete_external_payload(
    conn: &Connection,
    storage_root: &Path,
    payload_ref: &str,
    opts: &DeleteOpts,
) -> Result<DeleteOutcome, LcmError>;

pub(crate) struct DeleteOpts {
    pub rewrite_placeholders: bool, // tombstone residual raw refs (default true)
    pub remove_file: bool,          // false => metadata+ref cleanup only (preview/tests)
    pub verify_hash: bool,          // D-4 hash gate (default true)
}

pub(crate) struct DeleteOutcome {
    pub metadata_row_existed: bool,
    pub file_existed: bool,
    pub file_removed: bool,
    pub placeholders_rewritten: usize,
    pub bytes_freed: u64,
}
```

`delete_external_payload` is idempotent: an already-absent row/file/tombstone is
a no-op success (D-3). `DeleteOutcome` lets the caller distinguish "nothing to
do" from "reaped N bytes" without an error.

### 3.2 Algorithm (D-2 safe order — critical)

```
delete_external_payload(conn, root, ref, opts):
    validate_payload_ref(ref)                      # reject bad input up front
    dir = existing_payload_dir(root)               # canonical, validated
    path = dir.join(ref); ensure_contained(dir, path)   # re-contain

    # 1. Read pre-deletion metadata (for hash gate + bookkeeping). Do NOT hold
    #    it across the txn as truth — re-read under the txn.
    meta = load_payload_metadata(conn, ref).await  # Ok | Err(PayloadNotFound)

    # 2. Hash gate (D-4): if a file exists AND metadata exists, the on-disk
    #    SHA-256 MUST equal meta.content_hash. Mismatch => ABORT, report
    #    PayloadIntegrityMismatch anomaly, remove nothing.
    if meta.is_ok() && path.is_file() && opts.verify_hash:
        if sha256(read_for_verify(path)) != meta.content_hash:
            return Err(PayloadIntegrityMismatch)   # NEVER auto-delete corrupted (§9)

    bytes_freed = meta.byte_count if file will be removed else 0

    # 3. DB transaction: delete metadata row (+ optionally rewrite placeholders).
    #    BEGIN IMMEDIATE so the reap-time referenced-ness re-check is authoritative.
    BEGIN IMMEDIATE
      # 3a. Referenced-ness re-check UNDER the txn (contract §10.3 / D-4).
      if referenced_payload_refs(conn, provider=NULL, session=NULL).contains(ref):
          ROLLBACK
          return Err(StillReferenced)        # live again (recovery edge); do not reap
      #     (For the synchronous caller path this is informational; for GC it
      #      is the race guard against concurrent ingest/replay/carry-over.)

      if metadata row exists: DELETE FROM lcm_external_payloads WHERE payload_ref = ref
      if opts.rewrite_placeholders: tombstone_residual_placeholders(conn, ref)
      # clear any GC mark for this ref (see §5)
      DELETE FROM lcm_gc_marks WHERE payload_ref = ref
    COMMIT                                         # <-- crash here => orphan file (safe)

    # 4. File removal AFTER commit (D-2). Crash here => orphan file; next GC reaps.
    if opts.remove_file && path.is_file():
        safe_remove_payload_file(dir, ref)         # §7.3: lstat+contain+O_NOFOLLOW
        file_removed = true

    return DeleteOutcome { ... }
```

**Why this order (D-2).** Deleting the file *before* the DB commit would destroy
bytes a rollback might still need → unrecoverable data loss. Deleting the row
first means a crash in the commit→remove window leaves an **orphan file** (safe;
GC reaps) instead of a missing file behind a live reference (unsafe; `expand`
would fail). This is the inverse of the ingest order (file before row,
`raw.rs:258`→`:267`) and is chosen for the same crash-safety reason: always err
toward leaving an orphan file, never toward losing referenced data.

### 3.3 `tombstone_residual_placeholders`

For every `lcm_raw_messages` row whose `content`/`snippet_text`/`index_text`/
`metadata_json` contains a placeholder citing `ref` (found via
`extract_payload_refs_from_text`), rewrite that placeholder's prefix from
`[externalized …` to `[gc'd externalized …` (and the `tool output` variant) using
`tombstone_placeholder_in_text` (§8). Idempotent: an already-`[gc'd …]` bracket
is left untouched. Per-row `UPDATE` inside the same txn. Whole-message external
refs (`storage_kind='external' AND payload_ref=ref`) are handled by the caller
clearing/nulling the column as appropriate — for GC the raw row is *not* deleted
(messages persist), so the column is set to the tombstone text form consistent
with the placeholder grammar.

---

## 4. The reaper — `run_payload_gc`

The daemon-owned reconciliation pass. Default `apply = false` (dry-run/report,
contract §10.6). An apply run requires an explicitly authorized owner write
scope; `lcm_doctor` can only report the same candidate evidence. It runs four
phases in a fixed order; each phase is independently idempotent and may be
skipped via config.

```
run_payload_gc(conn, root, provider, session_id, cfg) -> LcmGcReport:
    report = LcmGcReport::default()
    if cfg.backup_before_reap && cfg.apply:
        checkpoint_wal_for_backup(conn); backup_database(db_path, root)   # owner §13

    dir = existing_payload_dir(root)            # validated canonical dir
    metadata_refs = all_payload_metadata_refs(conn)        # PK set
    referenced   = referenced_payload_refs(conn, provider, session_id)  # OM-2
    now = current_timestamp()

    # PHASE A — orphan files (no metadata row). mtime-based grace (GP-2).
    report.orphans = reap_orphan_files(dir, metadata_refs, now, cfg, cfg.apply)

    # PHASE B — unreferenced metadata (row, no live ref). two-scan grace (GP-2).
    report.unreferenced = reap_unreferenced_metadata(
        conn, root, metadata_refs, referenced, now, cfg, cfg.apply)

    # PHASE C — missing metadata (row+ref, file gone). long window (GP-4).
    report.missing = reap_missing_metadata(conn, root, now, cfg, cfg.apply)

    # PHASE D — dangling placeholders (placeholder, no row, no file). tombstone.
    report.dangling = rewrite_dangling_placeholders(conn, provider, session_id, cfg.apply)

    if cfg.apply:
        set_gc_meta(conn, "last_gc_at", now)
        set_gc_meta(conn, "last_gc_run_bytes_reaped", report.total_bytes())
        clear_gc_meta(conn, "last_error")            # success clears prior error
    else:
        set_gc_meta(conn, "last_gc_dry_run_at", now)
    return report
```

Phases are ordered so the cheapest, safest reconciliations run first and so that
a phase's outputs feed the next: e.g. Phase A removes orphan files before Phase B
might tombstone a placeholder that *was* dangling only because its file was
orphan-class. Each phase catches its own per-ref errors (§10) and never aborts
the whole run on a single bad ref.

---

## 5. The two-scan rule and the marker store

### 5.1 Why a marker store is required

The contract GP-2 specifies the two-scan rule for **unreferenced metadata**:
mark on pass N, reap on pass N+1 only if *still* unreferenced, ≥ grace apart,
with **no new column** on `lcm_external_payloads`. "No new column" does not mean
"no persistent state": across process restarts (the normal case for a scheduled
GC) the "marked on pass N" set must survive durably, in the **same** DB as the
rows it tracks. A sidecar JSON file would reintroduce the exact file/DB-desync
problem GC exists to solve. Therefore the marker lives in **additive tables
inside the LCM DB**.

### 5.2 Marker tables (additive, monotonic-safe)

The current LCM schema creates these additive tables inside `ensure_lcm_schema`
via `CREATE TABLE IF NOT EXISTS` (so partial runs and DBs mid-migration
converge):

```sql
-- Per-ref two-scan marker (unreferenced + missing tracking).
CREATE TABLE IF NOT EXISTS lcm_gc_marks (
    payload_ref   TEXT    PRIMARY KEY,
    state         TEXT    NOT NULL CHECK(state IN ('unreferenced','missing')),
    first_seen_at INTEGER NOT NULL,          -- unixepoch when first observed in this state
    updated_at    INTEGER NOT NULL DEFAULT (unixepoch())
);

-- Run-level GC metadata (key/value), mirroring src/db/metadata.rs semantics
-- but scoped to the LCM DB.
CREATE TABLE IF NOT EXISTS lcm_gc_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

Monotonic safety (contract §11, §13): the schema guard skips DBs written by a
newer release, and the `IF NOT EXISTS` DDL is idempotent. **No existing column or
row is altered** — payloads without a mark are `live` until the first observed-
unreferenced scan. This honors GP-2 ("no new column" on the payload table) while
giving owner GC a crash-safe marker store.

> **Future schema option (contract GP-3).** A later additive schema revision may
> fold the marker into `lcm_external_payloads` as `unreferenced_since` and
> `gc_state` columns. The contract is identical either way; tests assert behavior,
> not table shape. Any migration remains an owner-authorized schema operation,
> never a Doctor action.

### 5.3 Mark lifecycle (the two-scan rule, concretely)

State machine for one ref, evaluated each run under `BEGIN IMMEDIATE`:

```
on each GC run, for unreferenced metadata (Phase B):
  U = metadata_refs - referenced               # currently unreferenced
  for ref in U:
      mark = SELECT state, first_seen_at FROM lcm_gc_marks WHERE payload_ref = ref
      if mark is None:
          INSERT lcm_gc_marks(ref, 'unreferenced', now)        # pass N: mark
          continue                                             # do NOT reap this run
      if mark.state == 'unreferenced' and now - mark.first_seen_at >= grace:
          # pass N+1: eligible — but RE-CHECK referenced-ness at reap time (§10.3)
          reap_unreferenced_ref(ref)                           # via delete_external_payload
          # delete_external_payload clears the mark inside its txn
      else:
          continue                                             # still within grace
  # recovery edge (live <- unreferenced): clear stale marks
  for ref in (metadata_refs ∩ referenced) with a mark:
      DELETE FROM lcm_gc_marks WHERE payload_ref = ref
```

**Crash safety.** Marks and the reap row-delete live in the same transaction.
Crash after marking, before reap → mark persists, next run continues the clock.
Crash mid-reap (row committed, file not removed) → mark deleted with the row,
orphan file remains → Phase A reaps it next pass by mtime. Either way, a re-run
converges (contract §8).

**Re-referenced recovery.** If a ref gains a live reference again before reaping,
its mark is cleared (last `for` above), so a *new* unreferenced period restarts
the grace clock — matching the lifecycle state machine's `live ← unreferenced`
edge (contract §5). This is why GC re-checks referenced-ness at reap time, not
at mark time.

---

## 6. Phase algorithms

### 6.1 Phase A — `reap_orphan_files` (mtime grace, no row)

```
reap_orphan_files(dir, metadata_refs, now, cfg, apply) -> OrphanReport:
    candidates = []
    for entry in fs::read_dir(dir):
        name = entry.file_name()                       # NEVER use a caller path
        if validate_payload_ref(name).is_err(): skip   # name shape (§7.1)
        m = fs::symlink_metadata(entry.path())         # lstat — do NOT follow
        if m.is_symlink() or not m.is_file(): skip     # reject symlink/odd (§7.2)
        if name in metadata_refs: skip                 # has an owner row -> not orphan
        age = now - m.mtime()
        if age < cfg.grace_seconds: skip               # GP-2 mtime grace
        candidates.push((name, m.len(), age))
    # reap (or report) — containment re-checked per file in safe_remove_payload_file
    for (name, bytes, _) in candidates:
        if apply: safe_remove_payload_file(dir, name)
    return OrphanReport { refs: names, bytes_total, count }
```

No mark table is used for orphans: `mtime` is the grace clock (contract GP-2).
No hash gate is possible (no owner row ⇒ no expected `content_hash`); orphan
reap trusts name-shape + containment + lstat only. Threat model: an attacker who
can *write* under `lcm-payloads/` already owns that directory; the invariant we
uphold is that reap **cannot escape** the canonical dir or follow a symlink
(§7, §13).

### 6.2 Phase B — `reap_unreferenced_metadata` (two-scan, §5.3)

For each eligible ref (marked ≥ grace, still unreferenced at reap time), call
`delete_external_payload(conn, root, ref, {rewrite_placeholders:true,
remove_file:true, verify_hash:true})`. On `StillReferenced`: clear mark, skip
(race lost to concurrent ingest — safe). On `PayloadIntegrityMismatch`: report
anomaly, **do not** reap, leave row+file (§9). The hash gate (D-4) runs inside
`delete_external_payload`.

### 6.3 Phase C — `reap_missing_metadata` (long window, GP-4)

A *missing* payload = metadata row + live reference, file absent. Per contract
§9 these are **reported as errors first**; automatic reap is gated on the longer
`reap_missing_metadata_after` window and tracked via the same `lcm_gc_marks`
table with `state='missing'`:

```
for ref in metadata_refs where not path.is_file():
    mark = lcm_gc_marks[ref] or INSERT(ref,'missing',now)   # first observation
    if now - mark.first_seen_at < cfg.reap_missing_after: continue   # report-only
    if cfg.reap_missing_enabled == false: continue                   # opt-in gate
    # eligible: tombstone the reference (row delete + placeholder -> [gc'd …]).
    # No file to remove; delete_external_payload(opts.remove_file=false) is a no-op
    # on the absent file and still clears the row + tombstones placeholders.
    delete_external_payload(conn, root, ref,
        {rewrite_placeholders:true, remove_file:false, verify_hash:false})
```

Because there is no file, D-4's hash gate is skipped (`verify_hash:false`) and
`remove_file:false`. The row is deleted and the live placeholder is tombstoned
so `expand` returns `PayloadGc'd` (MF-1) rather than `PayloadMissing`. If the
file reappears (FS remount/recovery) the mark is cleared by the recovery edge in
§5.3 (a present file with a reference is `live`).

**Default caution:** `reap_missing_metadata_enabled` defaults to `false` so
missing payloads are reported indefinitely unless an operator opts in — matching
the contract's "may indicate an FS problem worth investigating." Operators who
want automatic convergence set it `true` with a long `reap_missing_after`.

### 6.4 Phase D — `rewrite_dangling_placeholders`

A *dangling placeholder* = a raw row's text cites `ref`, but there is no metadata
row and no file (contract §3). GC rewrites the placeholder prefix to `[gc'd …]`
via `tombstone_placeholder_in_text` (§8) so the message still reads correctly.
Pure DB operation (per-row `UPDATE`), idempotent. Uses
`referenced_payload_refs` minus `metadata_refs` to find candidate refs, then
scans the carrying rows. No file, no mark.

---

## 7. Filesystem enumeration & deletion safety (§10.1–2, §13)

This is the highest-risk surface. Three independent gates, **all** required:

### 7.1 Name gate

Only filenames matching `^payload_[0-9a-f]{64}\.payload$` directly under the
canonical payload dir are even considered. `validate_payload_ref`
(`payload.rs:56`) already enforces "single `Normal` component, no `.`/`..`/`/`/`\`";
GC additionally rejects names that don't match the owner-hash shape so stray
files (e.g. editor swaps, `.tmp`) are ignored, never reaped.

### 7.2 Symlink / type gate (lstat, never stat)

Use `fs::symlink_metadata` (`payload.rs:345` pattern) — the lstat equivalent —
and **reject** any entry whose metadata is a symlink or not a regular file.
`entry.metadata()`/`is_file()` follow symlinks on some platforms and MUST NOT be
used for the safety decision. This mirrors `canonical_storage_root` (rejects a
symlink *root*) and `ensure_actual_private_dir` (rejects a symlink *dir*) at the
file level.

### 7.3 `safe_remove_payload_file` (containment + O_NOFOLLOW + lstat recheck)

`std::fs::remove_file` does **not** use `O_NOFOLLOW`, so a symlink could be
swapped in between validation and unlink. The safe sequence:

```
safe_remove_payload_file(dir, name):
    validate_payload_ref(name)
    path = dir.join(name)
    ensure_contained(dir, path)                     # parent == canonical dir
    # open the file itself with O_NOFOLLOW (Linux) / reject symlink elsewhere
    f = private_file_options().read(true).open(path)   # O_NOFOLLOW fails on symlink
    meta = f.metadata()                                # fstat on the opened fd
    assert meta.is_file()                              # not a dir/device
    # TOCTOU shrink: re-lstat the path; inode must match the opened fd
    lm = fs::symlink_metadata(path)
    assert not lm.is_symlink()
    assert same file identity (lm) == (fstat)
    drop(f)
    fs::remove_file(path)
```

On Linux the `O_NOFOLLOW` open (`private_file_options`, `payload.rs:462`) is the
primary guard: it fails `ELOOP` on a symlink rather than opening the target. On
non-Unix targets the `symlink_metadata` lstat check is the guard. A mismatch at
any gate aborts removal for that file and records a `skipped` anomaly; the run
continues with the next candidate. Tests (§11, sibling `t_f0e07c5c`) MUST cover:
symlink-in-dir, symlink swap, `..` traversal name, absolute-path injection, file
outside root reached via a symlinked dir, and a dir masquerading as `*.payload`.

**Containment is re-checked immediately before `remove_file`** (contract §10.2:
"never delete by caller-supplied path; refs only; resolve under canonical dir
and re-validate containment immediately before remove"). GC never accepts a path
from a caller — only a validated `name` joined to the canonical dir.

---

## 8. `tombstone_placeholder_in_text`

```
tombstone_placeholder_in_text(text, ref) -> text:
    # locate each "[externalized ... ref=<ref> ...]" / "[externalized tool output ...]"
    # bracket that cites `ref` (via extract_payload_refs_from_text offsets) and
    # rewrite its prefix to the "[gc'd externalized ...]" / "[gc'd externalized
    # tool output ...]" form recognized by is_external_payload_placeholder.
    # Already-"[gc'd …]" brackets are returned unchanged (idempotent).
```

`is_external_payload_placeholder` (`payload.rs:104`) already accepts all five
prefixes including the two `[gc'd …]` forms, so the parser needs no change —
only the writer. The rewriter must: (a) handle all live prefixes
(`[externalized payload:`, `[externalized lcm ingest payload:`,
`[externalized tool output:`); (b) be idempotent on already-tombstoned text;
(c) preserve everything after the prefix (the `ref=…;…` body); (d) handle a ref
appearing in more than one of the four columns or more than once in one column.
`expand` on a tombstoned ref returns the new `LcmError::PayloadGc'd` (MF-1), not
`PayloadMissing`.

---

## 9. Configuration knobs

`LcmGcConfig` is daemon-owned maintenance policy. Read-only Doctor diagnostics
may classify noise through `LcmNoiseClassificationConfig`, but neither Doctor
nor MCP accepts GC policy or invokes maintenance.

| Knob | Type | Default | Bounds | Effect |
|---|---|---|---|---|
| `lcm_payload_gc_grace_seconds` | u64 | `86400` (24h) | floor **300** (GP-1) | Min continuous collectable time before an orphan file / unreferenced-ref is reaped. |
| `lcm_payload_reap_missing_metadata_after_seconds` | u64 | `604800` (7d) | `0` = never | Window after which a *missing* payload (row+ref, no file) becomes eligible for tombstoning. |
| `lcm_payload_reap_missing_metadata_enabled` | bool | `false` | — | Master opt-in for Phase C auto-tombstone. `false` ⇒ missing payloads reported forever (contract §9 caution). |
| `lcm_payload_gc_max_batch_size` | usize | `500` (== `SQLITE_IN_BATCH_SIZE`) | ≥1 | Caps refs reaped per run; bounds txn/lock time. Excess candidates wait for the next run. |
| `lcm_payload_gc_backup_before_reap` | bool | `true` | — | Run `checkpoint_wal_for_backup`+`backup_database` before an authorized owner maintenance run (contract §13). |
| `lcm_payload_gc_interval_seconds` | u64 | `21600` (6h) | — | Daemon maintenance cadence; the store records `last_gc_at` and the owner skips if too recent. |
| `lcm_payload_gc_enabled` | bool | `true` | — | Master switch for daemon-scheduled GC. An explicitly authorized owner run may be invoked independently of the scheduler. |

**No zero-grace escape hatch.** The 300 s floor is enforced in the config parser
(contract GP-1) so a misconfiguration cannot create a near-zero grace that races
concurrent ingest. `grace_seconds` clamps up to 300.

---

## 10. Transaction, locking, batching & error handling

- **Per-ref `BEGIN IMMEDIATE`.** GC opens the writer transaction only across one
  ref's decision+delete, never across filesystem I/O or across the whole run
  (contract §10.7). `checkpoint_wal_for_backup` runs once before the run.
- **Referenced-ness re-check under the txn** (§3.2 step 3a, §5.3) is the guard
  against racing ingest/replay/compression carry-over: a ref marked unreferenced
  on pass N is reaped on pass N+1 only if *still* unreferenced when the reap txn
  opens. A ref that went `live` again is skipped and its mark cleared.
- **Batching.** Reaps are processed in chunks of `gc_max_batch_size`; each ref is
  its own txn. A run that hits the cap stops reaping and leaves the remainder for
  the next scheduled run (reported as `deferred`). This bounds lock hold time and
  matches the existing `SQLITE_IN_BATCH_SIZE=500` chunking in `doctor.rs`.
- **Two processes GC-ing the same store** (contract §12): only one holds the reap
  `BEGIN IMMEDIATE` at a time; the loser observes the row/mark already gone and
  no-ops (idempotent). The store's existing writer-lock model applies; GC adds no
  new cross-process lock.
- **Per-ref error isolation.** A failure on one ref (integrity mismatch, I/O
  error, containment rejection) is recorded in `report.errors` and the run
  continues. Only classes that indicate systemic corruption (schema missing,
  storage root not a dir) abort the run; the abort is recorded in
  `lcm_gc_meta.last_error` + `last_error_at`.
- **Partial-failure convergence.** Re-running GC after any crash or partial run
  converges: marks persist the clock; committed row-deletes with pending
  file-removals become orphans reaped by Phase A; half-tombstoned messages are
  completed by the next Phase D (idempotent). No manual "resume" step exists or
  is needed (contract §8).

---

## 11. Dry-run / report shape

Dry-run (`apply = false`) is the **default** and is mandatory before any
destructive view in the dashboard (contract §10.6). The report enumerates
exactly what *would* be removed — refs, counts, byte totals, ages — and **never**
body bytes, snippet text, or message content (contract §13). Hashes shown are
the `content_hash` already stored in `lcm_external_payloads`, never re-derived
body digests in a way that implies we read bodies into reports.

```jsonc
{
  "status": "dry_run" | "applied" | "error",
  "provider": "cursor",
  "session_id": null,
  "apply": false,
  "started_at": 1781420000,
  "ended_at":   1781420012,
  "config": { "grace_seconds": 86400, "reap_missing_after": 604800, "max_batch_size": 500 },
  "orphans":      { "count": 3, "bytes": 9437184, "refs": ["payload_….payload", …] },
  "unreferenced": { "count": 1, "bytes": 2097152, "refs": ["payload_….payload"] },
  "missing":      { "count": 0, "refs": [] },
  "dangling":     { "count": 2, "refs": ["payload_….payload"] },
  "deferred":     { "count": 0, "reason": null },
  "errors": [
    { "ref": "payload_….payload", "kind": "integrity_mismatch", "detail": "sha256 mismatch" }
  ],
  "totals": { "files": 3, "bytes": 11534336, "rows_deleted": 1, "placeholders_rewritten": 2 },
  "last_gc_at": 1781410000,
  "last_error": null
}
```

Counts are exact; `refs` lists are capped (reuse `MAX_SAMPLES = 20`,
`doctor.rs:14`) to bound report size. `status = "applied"` only when `apply &&
!errors_that_aborted`. The same shape is returned for `apply = true` with the
post-reap counts.

---

## 12. Scheduling & manual invocation

**No background thread in the store (v1).** Mirroring the contract's SD-2
rationale (keep the hot path simple), GC does **not** piggyback on ingest and
does **not** spawn its own scheduler. It exposes `run_payload_gc` for any caller
and records `last_gc_at`; scheduling is host-driven.

**Manual / owner invocation:**

- The daemon maintenance coordinator may schedule `run_payload_gc` and perform
  an apply run under its write ownership. An explicit owner API may do the same
  after validating the active write scope and recording the backup/report.
- `lcm_doctor` remains a read-only diagnosis/evidence surface. It may expose GC
  candidate counts and the last run status, but it has no `gc` mode, no `apply`
  flag, and no destructive alias. A separate owner operation is required for
  any mutation.
- `lcm_status` gains the GC health fields below (read-only). A `gc_config`
  argument carries `LcmGcConfig` (§9) for the daemon/owner entry point.

**Recommended scheduling:** the daemon owner may run a frequent **dry-run**
report (e.g. every 1–6 h) for visibility and a less frequent **apply** (e.g.
daily) gated by `lcm_payload_gc_interval_seconds` and `last_gc_at`. The dry-run
is cheap (no I/O mutations, no locks beyond reads) and safe to run any time. An
apply run is skipped if `now - last_gc_at < gc_interval_seconds` unless the
authorized owner explicitly forces it.

**Per-store, not global.** Each resolved project store (user-level shard,
explicit local store, or legacy local store) is GC'd independently against its
own DB (contract §14). There is no cross-store coordination.

---

## 13. Safety invariants (consolidated checklist)

Mapped to the contract §13 and this doc. Implementation and tests assert each.

1. **Never log/preview/report/stream body bytes.** Reports carry refs, counts,
   byte totals, and the `content_hash` already in the DB — nothing more (§11).
2. **Enumerate only** `^payload_[0-9a-f]{64}\.payload$` directly under the
   canonical payload dir; reject every other name (§7.1).
3. **Never follow a symlink** during enumerate or delete (lstat gate §7.2;
   `O_NOFOLLOW` open §7.3). Reject a symlinked storage root / payload dir
   (`canonical_storage_root`, `existing_payload_dir`).
4. **Never delete outside the canonical payload dir;** re-run `ensure_contained`
   immediately before `remove_file`; accept only validated names, never caller
   paths (§3.2, §7.3).
5. **Safe removal order (D-2):** commit the DB delete (metadata row + mark
   cleared + placeholders tombstoned) **before** `remove_file` (§3.2). Crash ⇒
   orphan file, never missing-file-behind-live-ref.
6. **Hash gate (D-4):** never delete a file whose on-disk SHA-256 ≠ the metadata
   `content_hash`; abort + report `PayloadIntegrityMismatch`, remove nothing
   (§3.2, §9). Corrupted files are **never** auto-deleted.
7. **Re-check referenced-ness at reap time under `BEGIN IMMEDIATE`** (§3.2 3a,
   §5.3) — the race guard for concurrent ingest/replay/carry-over.
8. **Honor grace:** mtime for orphan files, two-scan (marker) for unreferenced
   metadata, `reap_missing_after` (opt-in) for missing metadata (§5, §6, §9).
   300 s floor enforced (§9).
9. **Idempotent + convergent:** any re-run after any crash reaches a clean state
   with no data loss and no double-work (§5.3, §10).
10. **Backup before mutate:** `checkpoint_wal_for_backup` + `backup_database`
    before an authorized owner `apply` run (§4, §12).
11. **Dry-run default; destruction opt-in** (`apply = true`) for owner operations
    (§11, §12). Doctor is always read-only.
12. **Per-ref `BEGIN IMMEDIATE`, no FS I/O under the lock;** bounded by
    `gc_max_batch_size` (§10).
13. **Distinct `LcmError::PayloadGc'd`** for tombstoned refs (MF-1) so ops can
    tell intentional reap from unexpected loss (§8).

---

## 14. Status / health fields the reaper produces (for `t_0ab1c041`)

The reaper writes `lcm_gc_meta` and the existing doctor/status queries already
compute the candidate sets. Additive fields only (contract §11: no rename/remove
of existing fields):

- `LcmPayloadStatus` (`types.rs:517`) gains: `last_gc_at: Option<i64>`,
  `last_gc_dry_run_at: Option<i64>`, `bytes_reclaimable: i64` (sum of orphan +
  unreferenced byte totals from the last scan), `last_gc_error: Option<String>`.
- A new `LcmGcStatus` block (or fields on `LcmPayloadStatus`) carrying
  `orphans`, `unreferenced`, `missing`, `dangling` counts + bytes and the
  effective `LcmGcConfig` (mirroring `LcmConfigStatus` at `types.rs:500`).
- Healthy ⇔ `missing == 0 && errors empty`; warning ⇔
  `orphans + unreferenced > 0`; error ⇔ `missing > 0 || last_gc_error present`.
  (Surfacing/UI thresholds are `t_0ab1c041`'s to finalize.)

---

## 15. Implementation outline (for the implementation task)

Concrete, ordered, minimal-blast-radius steps. Each is independently testable.

1. **Marker-table authority** (`schema.rs`): keep `lcm_gc_marks` and
   `lcm_gc_meta` additive and idempotent, with their get/set helpers.
2. **Shared reference authority:** keep `referenced_payload_refs` in the shared
   GC path and have read-only Doctor call the same helper. Behavior must agree.
3. **Payload errors and tombstones:** keep `LcmError::PayloadGc'd`,
   `StillReferenced`, and `tombstone_placeholder_in_text` covered for all
   prefixes, idempotency, multi-column, and nested cases.
4. **Owner reaper:** keep `safe_remove_payload_file` and
   `delete_external_payload` behind the authorized owner boundary.
5. **Phases A–D:** keep `run_payload_gc` and `LcmGcConfig` dry-run by default;
   only owner apply runs may mutate.
6. **Reports and visibility:** keep `LcmGcReport`/status fields and ensure
   Doctor/status serialize evidence without applying it.
7. **Config wiring:** keep effective defaults surfaced to the daemon/owner
   maintenance entry point.

Each step adds capability without widening Doctor: the doctor continues to
report payload evidence while the daemon/authorized owner path performs any
deferred retention or GC write and records the resulting report.

---

## 16. Test hooks (for `t_f0e07c5c`)

To make GC deterministic and assertable without sleeping for grace periods:

- **Clock injection.** `run_payload_gc` takes a `now: i64` (or a `Clock` impl)
   rather than calling `current_timestamp` internally, so tests advance time to
   cross the grace/missing windows instantly. (Phases already take `now` in the
   pseudocode above.)
- **Marker inspection.** Tests query `lcm_gc_marks` directly to assert the
   two-scan state (marked-but-not-yet-reaped, mark-cleared-on-rereference).
- **`DeleteOpts` flags.** `remove_file: false` lets tests exercise the DB-only
   path (and the crash-between-commit-and-remove window) without touching the FS
   assertion; `verify_hash: false` is reserved for the missing-file path.
- **`LcmGcReport`** is the assertion surface: tests check `orphans`,
   `unreferenced`, `missing`, `dangling`, `totals`, `errors`, and idempotency
   (a second `apply` run reaps nothing new).
- **Fixtures** in `tempdir`: plant orphan files, symlinks (in-dir, swap,
   symlinked dir), `..`/absolute-path names, corrupted (hash-mismatch) files,
   missing files, dangling placeholders, and concurrent-write files (open fd
   held during reap) — covering every invariant in §13.

See the contract §16 test handoff for the state-transition and crash-order cases
these hooks support.

---

## 17. Open items handed off

- **Owner reaper and retention:** keep the implementation and authorization
  boundary covered by the current runtime tests.
- **UI/CLI surfacing:** expose healthy/warning/error evidence and any dry-run
  owner control without adding a mutating Doctor route.
- **Tests and fixtures:** keep direct behavior coverage for state transitions,
  safe removal, idempotency, path rejection, and error distinction.
- **Future marker-column form (GP-3):** optional behavior-preserving schema work;
  never a Doctor migration.
