# LCM external payload retention & GC — triage decision

Status: **triage complete**. This is the decision record and single entry point for the
LCM external-payload retention/GC effort. It does not re-specify anything — the four
normative specs below do — it records the **triage verdict**, **reconciles the open
items the specs deferred to this card**, and **fixes the implementation decomposition**.

The five discovery/planning children (`t_c2443a7f` audit, `t_f14bb734` contract,
`t_bbd369f2` GC design, `t_0ab1c041` visibility spec, `t_f0e07c5c` test plan) all
landed. This card validated their work against source and decided the build order.

## 1. Verdict

**The plan is complete, internally consistent, and grounded in current source.** Four
artifacts form a closed spec set with explicit cross-references and a single normative
hierarchy ("the contract wins; the others fill in what it deliberately left open"):

| Spec | Role | Owns |
|---|---|---|
| [`LCM-PAYLOAD-LIFECYCLE.md`](LCM-PAYLOAD-LIFECYCLE.md) | **contract (what/why)** | ownership model OM-1/2, lifecycle state machine, deletion contract D-1..D-4 + SD-1/2, grace GP-1..4, idempotency, missing/dangling/corrupted handling, security §13 |
| [`LCM-PAYLOAD-GC.md`](LCM-PAYLOAD-GC.md) | **design (how)** | `delete_external_payload`, 4-phase reaper, schema-v5 marker store, config knobs, dry-run report, consolidated safety checklist |
| [`LCM-PAYLOAD-VISIBILITY.md`](LCM-PAYLOAD-VISIBILITY.md) | **spec (what operators see)** | canonical `payload_health` data model, healthy/warning/error classifier, dashboard/doctor/MCP/CLI surfaces, dry-run gate |
| [`LCM-PAYLOAD-GC-TEST-PLAN.md`](LCM-PAYLOAD-GC-TEST-PLAN.md) | **test plan (prove it)** | 64 named cases (DEL/FS/PHA/PHB/PHC/PHD/TS/GC/CFG/DRY/VIS), fixtures, testability hooks, coverage matrix |

Plus the storage/deletion audit in Kanban `t_c2443a7f` (comment 34) which is the
ground-truth map the contract's §2 was built from.

## 2. Ground-truth anchors re-verified for this triage

The contract's §2 claims were re-checked against the current tree; all hold:

- `LCM_SCHEMA_VERSION = 4` (`schema.rs:7`) → bump to 5 is the next value. ✓
- `lcm_external_payloads` (`schema.rs:133-147`): `payload_ref` PK, `UNIQUE(provider,message_id,payload_ref)`,
  `FK(provider,session_id) REFERENCES sessions ON DELETE CASCADE`, **no FK to/from `lcm_raw_messages`**. ✓ (OM-1/2)
- Migration guard (`schema.rs:91-96`) skips any DB at `version >= LCM_SCHEMA_VERSION` →
  additive `CREATE TABLE IF NOT EXISTS` is monotonic-safe; no existing column is altered. ✓
- `is_external_payload_placeholder` (`payload.rs:104`) **already accepts both `[gc'd …]`
  prefixes** — the tombstone marker is reserved but unwritten; MF-1 needs only the *write* + the
  new error branch, not a parser change. ✓
- **No deletion primitive exists today:** zero `remove_file`/`fs::remove` call sites in
  `payload.rs`; deletes today bypass payload-aware code entirely. ✓
- Reused primitives all exist at cited anchors: `validate_payload_ref` (`payload.rs:56`),
  `existing_payload_dir` (`payload.rs:357`), `canonical_storage_root` (`payload.rs:366`),
  `ensure_contained` (`payload.rs:396`), `private_file_options` (`payload.rs:457/468`, Linux
  `O_NOFOLLOW` + non-unix variant), `all_payload_metadata_refs` (`doctor.rs:461`),
  `referenced_payload_refs` (`doctor.rs:498`, private — to be extracted). ✓
- `LcmError` (`types.rs:726`) has `InvalidPayloadRef`/`PayloadNotFound`/`PayloadMissing`/
  `PayloadIntegrityMismatch`; **`PayloadGc'd` and `StillReferenced` are absent** → both are
  new work (confirms MF-1 and the test-plan §8 open item). ✓
- Frontend source is real: `dashboard/lcm/src/{entry.tsx,styles.css}` → built to
  `dashboard/lcm/dist/{index.js,style.css}` by `dashboard/build.mjs`. The visibility UI card
  lands there. ✓

## 3. Open items reconciled (decisions for the implementation)

The specs deliberately deferred a handful of choices to the triage card. Decisions:

1. **GC run-state storage = GC.md's `lcm_gc_marks` + `lcm_gc_meta`.** VISIBILITY §4.2 floated
   three shapes (single-row `lcm_gc_state`, append-only `lcm_gc_runs`, key/value). The GC
   design (§5.2) owns storage and chose the minimal additive side tables; **that wins.**
   `lcm_gc_meta(key,value)` carries `last_gc_at` / `last_gc_status` / `last_gc_error` /
   `last_reaped_refs` / `last_reaped_bytes` / `last_gc_dry_run_at`; the VISIBILITY §3.2
   `LcmPayloadGcStatus` fields are **derived** from those keys. The append-only
   `lcm_gc_runs` history table is deferred to v6 (nice-to-have audit trail); v1 is
   latest-wins key/value, matching GC.md §5's "smaller blast radius, no ALTER of the hot
   table" rationale.

2. **Add both `LcmError::PayloadGc'd` (MF-1) and `LcmError::StillReferenced`.** The test plan
   (§8) flagged `StillReferenced` as absent; `DEL-005`/`PHB-003` assert it specifically, and
   GC.md §3.2 step 3a returns it on the reap-time referenced-ness re-check. Add both variants
   with `Display` impls; do **not** map `StillReferenced` onto an existing variant (the
   assertion would lose specificity).

3. **Injected clock, not mtime-backdating.** GC.md §16 commits to `run_payload_gc(..., now:
   i64)` and per-phase `now`; the test plan §2 makes injected `now` a precondition. **Honor
   it.** The implementation never calls `current_timestamp()` inside the reap path. This keeps
   the `filetime` dev-dependency out of v1 (only the *file's own* mtime is used for Phase A
   orphan grace, read via `symlink_metadata`, never set by tests unless a case explicitly
   asserts file-age semantics).

4. **Reclaimable-now vs reclaimable-after-grace is the one operator number that must be exact.**
   VISIBILITY §3.3 pins the math; GC.md §14 references it. Implementation computes
   `reclaimable_bytes` (now) from GC marks past grace + orphan files past grace, and
   `reclaimable_bytes_after_grace` from all unreferenced rows + all orphan files. Missing and
   corrupted are never reclaimable. This is the cross-cutting invariant VIS-002 asserts.

5. **Cross-surface numeric agreement is enforced structurally.** `lcm_status.payload`,
   `lcm_doctor … diagnostics.payloads`, and the dashboard `payload_health` block MUST be
   computed from one shared helper set (the extracted `referenced_payload_refs` + a shared
   payload-dir walk + a shared byte-sum query), never re-derived per surface (VIS §5.3,
   VIS-005). The implementation extracts those helpers; surfaces only serialize them.

6. **Schema v5 vs GP-3 column form.** Ship the side-table form (decision 1) as v5. The
   `unreferenced_since`/`gc_state` column form (GP-3) is a deferred, behavior-preserving v6
   refactor — not needed for v1 correctness, and tests assert behavior not table shape.

No spec conflict remains. The four docs agree; these decisions only resolve the choices they
left open.

## 4. Implementation decomposition

The implementation is large but, unlike most large efforts, **does not decompose into a
concurrent fleet** — the GC core is a strict build-order chain through shared files
(`types.rs`, `payload.rs`, `doctor.rs`, and a new shared `gc.rs`). Two agents writing those
files concurrently would collide. Per the user's "strict per-agent file ownership so writers
never collide" preference, the honest shape here is a **sequenced pipeline**, not a parallel
fleet. Parallelism returns only at the frontend layer (separate file tree).

Pipeline (each card owns its files; a card does not dispatch until its predecessor completes,
so no two writers touch the same file concurrently):

```
A1 (foundation)  ──►  A2 (reaper engine)  ──►  B (visibility + invocation)  ──►  C (frontend, deferred)
```

| Card | Scope (files owned) | Tests that land with it | Model |
|---|---|---|---|
| **A1 — foundation** | `schema.rs` (v5: `lcm_gc_marks`+`lcm_gc_meta`+get/set), `types.rs` (`PayloadGc'd`+`StillReferenced`+`LcmGcConfig`), `gc.rs` NEW (extracted `referenced_payload_refs` + `tombstone_placeholder_in_text`), `doctor.rs` (call shared fn) | `TS-001..003`, `CFG-001..005`, `FS-001..008` | bounded; clear spec |
| **A2 — reaper engine** | `payload.rs` (`delete_external_payload`+`DeleteOpts/Outcome`, `safe_remove_payload_file`, wire `expand`→`PayloadGc'd`), `gc.rs` (`LcmGcReport`, phases A–D, `run_payload_gc` w/ injected clock, reap-time ref re-check) | `DEL-001..007`, `PHA/PHB/PHC/PHD-*`, `GC-001..012`, `DRY-001..003` | hardest; safety-critical (path/symlink/crash/txn) |
| **B — visibility + invocation** | `query.rs` (`status()` byte/orphan/tombstoned additions), `doctor.rs` (`payload_diagnostics` bytes + `mode=gc`), `types.rs` (`LcmPayloadGcStatus` + status fields + `LcmGcReport` surfacing), `mcp/tools/definitions.rs` + `handlers/session.rs` (`lcm_status deep`, `lcm_doctor gc` mode), `dashboard/lcm_api.rs` + `mod.rs` (`payload_health` block, `/payloads/health`, `/payloads/gc`, capabilities), `agents/hermes/templates.rs` (gc mode desc + env wiring) | `VIS-001..012` | multi-file integration; cross-surface agreement |
| **C — frontend** *(deferred, spawned after B)* | `dashboard/lcm/src/{entry.tsx,styles.css}` (Payload Health card + dry-run→apply modal) → rebuild `dashboard/lcm/dist/{index.js,style.css}` via `build.mjs` | visual QA of green/amber/red pill + modal | UI; operator review of design |

**Why A1/A2/B are serial, not parallel:** A2 needs A1's schema + extracted helper + error
variants; B needs A2's `LcmGcReport`/status fields; all three write `types.rs` and `gc.rs`/
`doctor.rs`. The dependency links guarantee only one is active at a time. **C is parallel-safe**
(separate `dashboard/lcm/src` tree) but depends on B's JSON contract, so it is gated on B and
spawned after the contract is reviewable.

**Each implementation card cites the specs section-by-section** (GC.md §15 is the ordered
9-step outline; the test plan §7 is the case-landing order) and treats the four docs as
normative. No card re-derives semantics.

## 5. Non-goals reaffirmed (from contract §14)

Age/content retention policy; cross-store/cross-profile GC; soft-delete/undo/body recovery;
cross-message dedup; reaping summary nodes / lifecycle state / maintenance debt. The plan
addresses only payload files + `lcm_external_payloads` reconciliation. A separate load/perf
benchmark card (10k+ payloads) is a follow-up after v1 lands.

## 6. Sequencing note for tests

The test plan §7 ties each case to an implementation step. Cards land cases in lockstep with
their step (foundation → ts/cfg/fs; reaper → del/pha/phb/phc/phd/gc/dry; visibility → vis),
keeping the suite green at each card's completion. `#[cfg(unix)]` gates the symlink/`O_NOFOLLOW`
cases; `#[cfg(target_os="linux")]` the `O_NOFOLLOW`-specific ones.
