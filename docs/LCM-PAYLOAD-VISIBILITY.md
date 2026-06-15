# LCM external payload health — visibility spec

Status: **spec** (normative for the implementation/tests follow-on). Companion to the
lifetime & retention *contract* in [`LCM-PAYLOAD-LIFECYCLE.md`](./LCM-PAYLOAD-LIFECYCLE.md)
(parent task `t_f14bb734`) and the storage/deletion audit (`t_c2443a7f`). This document
defines **what an operator must be able to see** about externalized payload health, and
**where and how** they see it across the tracedecay surfaces.

The contract (`LCM-PAYLOAD-LIFECYCLE.md` §16) hands this spec two jobs:

1. Surface `live` / `unreferenced` / `missing` / `orphan` / `tombstoned` counts **and
   bytes**, plus `last_gc_at`, last GC error, and a **mandatory dry-run view before
   destructive cleanup**.
2. Define **healthy / warning / error** thresholds keyed off `missing_count` /
   `missing_payload_refs` (error) vs `orphan` / `unreferenced` counts (warning).

Everything here is additive: no existing `lcm_status` / `lcm_doctor` field is removed or
renamed (contract §11 — no breaking change to response shapes). All file:line anchors are
against the current tree and are anchors, not invariants.

## 1. Goal & non-goals

**Goal.** An operator looking at any tracedecay surface (standalone dashboard, Hermes
dashboard wrapper, MCP tool, or CLI doctor) can answer, in ≤ one glance for the headline
and ≤ one drill-down for detail:

- Are all referenced payload bodies present and intact? (`missing`, corrupted)
- How much disk is reclaimable right now vs after the grace window? (`orphan`, `unreferenced`
  bytes; grace remaining)
- Did the last GC run succeed, when, and what did it reap? (`last_gc_at`, last error, last
  reap totals)
- Before I delete anything, what exactly would be removed? (dry-run report; refs + bytes,
  never bodies)

**Non-goals.** Designing the GC algorithm itself (that is `t_bbd369f2`, bounded by contract
§6 and §10). Age/content retention policy UI. Cross-storage-root or cross-profile rollups
(each store is GC'd and reported independently). Body-byte recovery / soft-delete UI.

## 2. Existing surfaces (what exists today)

Every recommendation below reuses or augments one of these. Nothing is invented from
scratch.

| Surface | Anchor | What it exposes today |
|---|---|---|
| Storage status type | `LcmPayloadStatus` `src/sessions/lcm/types.rs:517-527` | `externalized_count`, `missing_count`, `unreferenced_count`, `placeholder_ref_count`, `missing_placeholder_metadata_count`, `missing_placeholder_file_count`, `gc_candidate_count` (== unreferenced), `root_contained` |
| Storage status builder | `status()` `src/sessions/lcm/query.rs:450-518` | Populates `LcmStatus.payload` (no bytes, no `last_gc`, no orphan-file count, no tombstoned) |
| Doctor diagnostics | `payload_diagnostics()` `src/sessions/lcm/doctor.rs:310-381` | Already classifies `missing_files`, `missing_payload_refs`, `orphan_files`, `orphan_payload_refs`, `unreferenced_metadata`, `placeholder_refs_total`, `missing_placeholder_metadata`, `missing_placeholder_files`, `gc_candidate_files`, `gc_candidate_payload_refs` (read-only; up to `MAX_SAMPLES=20` ref samples) |
| Doctor doctor() wrapper | `src/sessions/lcm/doctor.rs:44-93` | Returns `{status, dry_run, apply, diagnostics, repairs}`; `status ∈ {ok, issues_found, repaired}` |
| MCP `lcm_status` | `def_lcm_status` `src/mcp/tools/definitions.rs:1882-1904`; handler `handle_lcm_status` `src/mcp/tools/handlers/session.rs` | Args `{provider, session_id?, storage_scope?, hermes_home?}` → `LcmStatus` |
| MCP `lcm_doctor` | `def_lcm_doctor` `src/mcp/tools/definitions.rs:1906-1944`; mode parsing `lcm_doctor_mode` `handlers/session.rs:667-671` | Modes `diagnose | repair | retention | clean`; `apply` bool; `doctor_clean_apply_enabled` safety gate for `clean`+`apply` (`handlers/session.rs:677`) |
| Dashboard LCM routes | `router()` `src/dashboard/mod.rs:289-301` | `GET /api/plugins/hermes-lcm/{overview,search,session/{id},node/{id},timeline,compression}` |
| Dashboard LCM overview | `overview()` `src/dashboard/lcm_api.rs:213-344` | `messages_total`, `sessions_total`, role/source counts, summary-node stats, compression ratio, `latest_sessions` — **no payload-health block at all** |
| Dashboard capabilities | `capabilities()` `src/dashboard/mod.rs:323-348` | `features.lcm`, `lcm_scope`, `dashboards:["…","hermes-lcm",…]` |
| Dashboard frontend | bundled `dashboard/lcm/dist/{index.js,style.css}` (`assets.rs:33-34`); plugin label "LCM" (`mod.rs:365-373`) | LCM tab UI (messages/sessions/summaries); no payload-health card |
| Hermes plugin wrapper | `src/agents/hermes/templates.rs:81-106, 396-399, 556-562` | Exposes `lcm_status` / `lcm_doctor` as tracedecay tools; `tracedecay doctor` subcommand (`templates.rs:3449-3481`) is **installation/agent-integration** health, not LCM payload health |
| CLI | `src/cli.rs:14-263` | Top-level `Doctor` subcommand = tracedecay install/agent health. **No dedicated `tracedecay lcm` CLI subcommand**; LCM is reachable only via MCP tool dispatch (`tool_command.rs:43-47` allowlist) |

**Gap summary.** Counts exist (mostly) but (a) no **byte totals** anywhere, (b) no
**orphan-file count/bytes** in `lcm_status` (only in doctor), (c) no **`last_gc_at` / last
GC error / last reap** (no GC run state exists yet — depends on `t_bbd369f2`), (d) no
**tombstoned count**, (e) the **dashboard has no payload-health block or card**, (f) no
**dry-run/apply** endpoint pair for payload cleanup in the dashboard.

## 3. Canonical payload-health data model

All surfaces MUST derive from one canonical block. It is defined once in the storage layer
(`LcmPayloadStatus` extension + a new `LcmPayloadGcStatus`) and serialized verbatim by the
MCP tools and dashboard. Field names are `snake_case` and additive.

### 3.1 `LcmPayloadStatus` (extend existing — `types.rs:517`)

Keep all current fields; **add** the ones marked **(new)**. Existing consumers see new keys
they can ignore (no rename, no removal — contract §11).

| Field | Type | Source / semantics | State |
|---|---|---|---|
| `externalized_count` | i64 | rows in `lcm_external_payloads` in scope | existing |
| `missing_count` | i64 | rows whose file is absent **or** ref invalid (`payload_diagnostics` `missing_files`) | existing |
| `unreferenced_count` | i64 | metadata rows with no live raw reference (== `gc_candidate_count`) | existing |
| `placeholder_ref_count` | i64 | distinct refs extracted from the 4 text columns | existing |
| `missing_placeholder_metadata_count` | i64 | placeholder refs with no metadata row | existing |
| `missing_placeholder_file_count` | i64 | placeholder refs whose file is absent | existing |
| `gc_candidate_count` | i64 | alias of `unreferenced_count` (kept for back-compat) | existing |
| `root_contained` | bool | canonical payload dir is contained under storage root | existing |
| `orphan_file_count` **(new)** | i64 | files under payload dir with no metadata row (`payload_diagnostics` `orphan_files`) — promoted from doctor-only to first-class | new |
| `tombstoned_count` **(new)** | i64 | raw rows currently carrying a `[gc'd externalized payload: …]` placeholder (count of `is_external_payload_placeholder` gc'd-form hits) | new |
| `referenced_count` **(new)** | i64 | `externalized_count − unreferenced_count` (the `live` count; convenience, always derivable) | new |
| `total_bytes` **(new)** | u64 | Σ `byte_count` over all metadata rows in scope (referenced + unreferenced + missing) | new |
| `referenced_bytes` **(new)** | u64 | Σ `byte_count` over referenced rows | new |
| `orphan_file_bytes` **(new)** | u64 | Σ on-disk size of orphan files (no row → must come from `fs::metadata`, not the DB) | new |
| `reclaimable_bytes` **(new)** | u64 | bytes reclaimable **right now** by an `apply`: unreferenced rows past grace + orphan files past grace (see §3.3) | new |
| `reclaimable_bytes_after_grace` **(new)** | u64 | bytes that **will be** reclaimable once the grace window (contract §6.3 GP-1) elapses = Σ bytes of all unreferenced rows + all orphan files | new |
| `integrity_mismatch_count` **(new)** | i64 | rows whose on-disk SHA-256 ≠ stored `content_hash` (contract §9 — never auto-deleted) | new |

### 3.2 `LcmPayloadGcStatus` **(new struct)** — GC run + config state

Populated from the GC run-state record persisted by `t_bbd369f2` (see §4.2). All fields
absent/zero when GC has never run.

| Field | Type | Semantics |
|---|---|---|
| `last_gc_at` | Option\<i64\> | Unix ts of last completed GC pass (None until first run) |
| `last_gc_duration_ms` | Option\<u64\> | Wall time of last pass |
| `last_gc_status` | Option\<String\> | `ok` \| `partial` \| `failed` |
| `last_gc_error` | Option\<String\> | Last error string (truncated, **no body bytes** — contract §13) |
| `last_reaped_refs` | Option\<i64\> | Refs reaped on the last pass (metadata rows + orphan files) |
| `last_reaped_bytes` | Option\<u64\> | Bytes reclaimed on the last pass |
| `grace_seconds` | i64 | Effective `lcm_payload_gc_grace_seconds` (default 86400; floor 300 — contract GP-1) |
| `reap_missing_metadata_after_seconds` | i64 | Effective `reap_missing_metadata_after` (default 604800 — GP-4) |
| `next_run_eligible_at` | Option\<i64\> | Earliest ts a currently-unreferenced/orphan item becomes reaped-eligible (min mtime-based/scan-based eligibility in scope); informational |

`LcmStatus` gains `payload_gc: LcmPayloadGcStatus`.

### 3.3 Grace-aware reclaimable math

`reclaimable_bytes` (now) vs `reclaimable_bytes_after_grace` (eventually) is the single most
operator-relevant number, so the semantics are pinned:

- **Orphan files** are eligible at `now − file_mtime ≥ grace_seconds` (contract GP-2, mtime
  rule). Files not yet eligible contribute to `reclaimable_bytes_after_grace` only.
- **Unreferenced metadata** is eligible only under the two-scan rule (GP-2): marked on pass
  N, reaped on pass N+1 ≥ grace later. Until GC has recorded a first-scan mark for a ref it
  is **not** in `reclaimable_bytes`; once marked and ≥ grace has elapsed it moves into
  `reclaimable_bytes`. (With the optional v5 `unreferenced_since` column, GP-3, the rule
  collapses to `now − unreferenced_since ≥ grace`.)
- **Missing** payloads are **never** in either reclaimable bucket — reported-only until
  `reap_missing_metadata_after` (GP-4). **Corrupted** (`integrity_mismatch_count`) payloads
  are never reclaimable (contract §9 / D-4).

When GC run-state is unavailable (binary built before GC landed), `reclaimable_bytes` falls
back to **0** and `reclaimable_bytes_after_grace` = unreferenced bytes + orphan bytes, with
`last_gc_status = None`. The UI MUST render this as "GC not yet run" rather than implying
nothing is reclaimable.

## 4. Backend (storage layer)

### 4.1 Extend `status()` (`query.rs:450`)

Compute the §3.1 additions during the existing pass:

- `orphan_file_count` + `orphan_file_bytes`: extend the payload-dir walk that
  `payload_diagnostics` (`doctor.rs:340-362`) already does; share one `fs::read_dir` helper.
- `total_bytes` / `referenced_bytes`: `SELECT SUM(byte_count)` over metadata rows, with the
  referenced subset using the same `referenced_payload_refs` set the unreferenced count
  already derives (`query.rs` referenced-set helper; mirrored in `doctor.rs:498`).
- `integrity_mismatch_count`: optional/deferred — a SHA-256 sweep is I/O-heavy; gate it
  behind a `deep=false` default (see §5.1) so the default `lcm_status` stays cheap. When
  `deep=false`, emit `integrity_mismatch_count: null` (distinguish "not checked" from "0").
- `tombstoned_count`: `SELECT COUNT(*) … WHERE content LIKE '%[gc''d externalized payload:%'`
  (union of the 4 text columns), reusing the prefix already parsed by
  `is_external_payload_placeholder` (`payload.rs:104`). Cheap (LIKE over externalized rows
  only) but still opt-in for the default path — include in default since the set is small.

Keep the default `status()` pass O(metadata rows + files) with **no file hashing** unless
`deep=true`.

### 4.2 GC run-state persistence (recommendation to `t_bbd369f2`)

`last_gc_*` fields require persisting the outcome of a GC pass. Recommended minimal shape
(additive, monotone-safe under the migration guard `schema.rs:87-96`):

- A single-row table `lcm_gc_state(provider, session_id_scope, last_gc_at, last_gc_status,
  last_gc_error, last_reaped_refs, last_reaped_bytes, grace_seconds, …)` updated at the end
  of each GC pass **after** the reap transaction commits, OR
- An append-only `lcm_gc_runs(run_id, started_at, ended_at, status, reaped_refs,
  reaped_bytes, error)` with `lcm_gc_state` as a `MAX`-derived view.

Append-only is preferable (auditable history, trivially feeds a dashboard activity list).
This spec does not prescribe the table; it requires only that the §3.2 fields be derivable
from it. **`t_bbd369f2` owns the schema; this spec owns the surfaced shape.**

### 4.3 Extend `payload_diagnostics()` (`doctor.rs:310`)

Doctor already computes most of §3.1. **Add byte totals** to the `payloads` diagnostics
object (currently count-only) so `lcm_doctor` and `lcm_status` can share the same numbers:
`total_bytes`, `referenced_bytes`, `orphan_file_bytes`, plus sample buckets already capped at
`MAX_SAMPLES=20` (`doctor.rs:14`). Keep all sample lists capped — refs only, never bodies
(contract §13).

## 5. MCP tools

### 5.1 `tracedecay_lcm_status` — augment

Add optional input `deep: bool` (default `false`). When `false`, omit the SHA-256 sweep
(`integrity_mismatch_count` → null); when `true`, run it. Output gains the §3.1 new fields on
`payload` and the new top-level `payload_gc` block (§3.2). All additions are additive; the
existing description line (`definitions.rs:1886`) is updated to mention payload bytes + GC
state. No required-input change.

### 5.2 `tracedecay_lcm_doctor` — augment + add GC mode

- Output `diagnostics.payloads` gains the byte totals from §4.3 (additive).
- **Add mode `gc`** to the `mode` enum (`definitions.rs:1924`, `lcm_doctor_mode`
  `handlers/session.rs:669`): `diagnose | repair | retention | clean | gc`.
  - `mode=gc, apply=false` (default) → **dry-run reap report**: enumerates refs that would be
    reaped now (past grace, referenced-ness re-checked), their bytes, and the grace-remaining
    for not-yet-eligible items. No mutation. This is the mandatory dry-run view (contract
    §10.6).
  - `mode=gc, apply=true` → reap (deferred to `t_bbd369f2`); the doctor wrapper checkpoints
    WAL + backs up before applying (mirror `clean` path, `doctor.rs:160-161`).
- Update the `mode` description (`definitions.rs:1925`) to document `gc`.
- Reuse the existing `doctor_clean_apply_enabled`-style gate pattern for `gc`+`apply`
  (recommended `lcm_gc_apply_enabled`, default off unless env), so a destructive reap can't
  be triggered by accident.

### 5.3 Output shape note

`lcm_status` and `lcm_doctor` SHOULD agree numerically: `lcm_status.payload.orphan_file_count`
=== `lcm_doctor … diagnostics.payloads.orphan_files`, and byte totals identical. The
implementation MUST compute both from the same helpers (§4.1/§4.3) so they cannot drift.

## 6. Dashboard HTTP (standalone + Hermes wrapper)

The dashboard currently has **zero** payload-health exposure (§2). Add it in two layers: a
cheap headline on the existing overview, and a dedicated drill-down.

### 6.1 Augment `GET /api/plugins/hermes-lcm/overview` (`lcm_api.rs:213`)

Add a `payload_health` object to the `overview` payload (alongside the existing `overview`
insert at `lcm_api.rs:344`):

```jsonc
"payload_health": {
  "status": "healthy",           // healthy | warning | error  (§8)
  "externalized_count": 1284,
  "referenced_count": 1279,
  "unreferenced_count": 5,
  "orphan_file_count": 2,
  "missing_count": 0,
  "tombstoned_count": 17,
  "integrity_mismatch_count": null,  // null = not checked (deep=false)
  "total_bytes": 48210311,
  "referenced_bytes": 47900000,
  "orphan_file_bytes": 102431,
  "reclaimable_bytes": 0,            // reclaimable NOW
  "reclaimable_bytes_after_grace": 310311,
  "last_gc_at": 1781460000,
  "last_gc_status": "ok",
  "last_gc_error": null,
  "last_reaped_refs": 4,
  "last_reaped_bytes": 98000,
  "grace_seconds": 86400
}
```

This is computed from the same `status()` pass as the MCP tool (share the builder; do not
re-derive in `lcm_api.rs`). Cost: one metadata/files scan on overview load — acceptable for
a dashboard landing call. If profiling later shows it's too heavy, add `?deep=true` for the
integrity sweep only (headline stays cheap).

### 6.2 New `GET /api/plugins/hermes-lcm/payloads/health` — drill-down

Returns the full §3 block plus capped sample lists (refs only) for each non-zero anomaly
bucket, mirroring doctor's `missing_payload_refs` / `orphan_payload_refs` sample shape
(`doctor.rs:334-360`, cap `MAX_SAMPLES=20`). Supports `?deep=true` for the integrity sweep
and `?session_id=`/`?provider=` scope filters (same scoping rules as the rest of the LCM
API). Query params: `deep`, `provider`, `session_id`, `limit` (sample cap, default 20, max
100).

### 6.3 New dry-run/apply pair for operator-triggered cleanup

```
GET  /api/plugins/hermes-lcm/payloads/gc?apply=false   → dry-run reap report (default)
POST /api/plugins/hermes-lcm/payloads/gc               → apply reap (gate via capabilities)
```

- The `GET` (dry-run) is the **mandatory preview before any destructive cleanup**
  (contract §10.6, §7 §13). It returns the exact refs + byte totals that `apply` would
  remove — never bodies.
- The `POST` performs the reap (depends on `t_bbd369f2`). It MUST 400 if the caller did not
  first retrieve a dry-run within a short window (recommended: require a `dry_run_token`
  echoed from the GET, or at minimum require an explicit `confirm=true` body field), so a UI
  cannot fire a destructive POST without having shown the preview. This operationalizes the
  "safe dry-run view before destructive cleanup" acceptance criterion.
- Wire `features.lcm_gc` into `capabilities()` (`mod.rs:332`) so the UI can hide the action
  when the binary predates GC.

### 6.4 `capabilities()` additions (`mod.rs:323`)

Add to `features`: `"lcm_gc": <bool>` (true once `t_bbd369f2` lands), and surface
`"lcm_payload_health": true` so wrappers/UI can feature-detect the new block without parsing
version strings.

## 7. Dashboard UI (LCM tab)

The LCM frontend bundle (`dashboard/lcm/dist`, plugin "hermes-lcm") gains a **Payload Health**
card, consistent with the existing card styling on the overview.

### 7.1 Headline card (always visible on the LCM tab)

- A colored status pill: **green** healthy / **amber** warning / **red** error (predicates in
  §8). The pill links the operator to the drill-down (§7.2).
- Four primary numbers: `referenced_count` / `externalized_count`, `reclaimable_bytes` (now)
  with `reclaimable_bytes_after_grace` as secondary, `orphan_file_count`, `missing_count`.
- A GC line: "Last GC: `<last_gc_at>` (`<last_gc_status>`)" or "GC not yet run" when
  `last_gc_at` is null. On `last_gc_status != ok`, show `last_gc_error` (truncated) in red.
- Human-readable byte formatting (KiB/MiB/GiB) for every byte field; counts are integers.

### 7.2 Drill-down (the `payloads/health` data)

One section per anomaly bucket, each showing the count, the byte total where meaningful, and
the capped sample ref list:

- **Missing** (`missing_count`, error-red): refs + their owner message/session. Caption:
  "Referenced payload whose file is absent — investigate before any cleanup."
- **Unreferenced / GC candidates** (`unreferenced_count`, warning-amber): refs + bytes +
  grace-remaining per sample. Caption: "Reaped by GC after the grace window."
- **Orphan files** (`orphan_file_count`, warning-amber): file refs + on-disk bytes +
  age (`now − mtime`). Caption: "File with no DB row; reaped by GC after grace."
- **Integrity mismatch** (`integrity_mismatch_count`, error-red, only when `deep=true` and
  > 0): refs. Caption: "On-disk hash ≠ stored hash — never auto-deleted."
- **Tombstoned** (`tombstoned_count`, neutral): informational; "Payloads already reaped;
  placeholder retained so the message still reads."

### 7.3 Dry-run → apply flow (the safety UX)

A "Reclaim disk (dry run)" button calls `GET …/payloads/gc` and opens a **modal** showing the
exact refs + total bytes to be removed, the grace-remaining items that will *not* be touched,
and an explicit confirm. The confirm button (only enabled after the dry-run renders) issues
the `POST …/payloads/gc`. The modal copy MUST state that reap is permanent (no undo) and that
only refs/sizes are shown — no body preview, ever (contract §13). The button is hidden when
`features.lcm_gc` is false.

## 8. State classification — healthy / warning / error

Single source of truth, computed from the §3 block. Implemented once and reused by the
dashboard pill and any CLI/agent summary. Predicates (first match wins):

| State | Predicate | Rationale |
|---|---|---|
| **error** | `missing_count > 0` **OR** `missing_placeholder_file_count > 0` **OR** (`integrity_mismatch_count` is a number `> 0`) **OR** `last_gc_status == "failed"` **OR** `root_contained == false` | Missing/corrupted payload behind a live reference, or a failed GC, or a storage-root containment break, all indicate data the operator must investigate before any cleanup (contract §9, GP-4). |
| **warning** | not error, AND (`orphan_file_count > 0` **OR** `unreferenced_count > 0` **OR** `last_gc_at` is null) | Reclaimable garbage exists, or GC has never run. Not data loss, but disk/tidiness attention. |
| **healthy** | none of the above | All referenced bodies present and intact, nothing reclaimable, GC healthy. |

This matches the parent handoff (contract §16): error keys off `missing_count` /
`missing_payload_refs`; warning keys off orphan/unreferenced counts. `root_contained == false`
is folded into error because a non-contained payload dir is a security/config break
(contract §13) that silently bypasses reap safety.

**Thresholds are counts, not ratios.** A single missing payload is an error regardless of
how many healthy payloads exist — one missing body behind a live reference is already a
correctness problem (contract §9). Tunable numeric thresholds are explicitly **not** added
(non-goal: avoid false-comfort from "only 0.1% missing").

## 9. Dry-run-before-destructive-cleanup contract (cross-cutting)

Every destructive payload path MUST present a dry-run first. Concretely:

- `lcm_doctor mode=gc apply=false` is the default and is the preview; `apply=true` reaps
  (§5.2).
- `lcm_doctor mode=clean apply=true` already deletes DB rows and leaves files (contract §7).
  Its output MUST state that payload files will be reaped by GC after the grace period (so
  operators don't expect immediate disk reclamation). Add this line to the `clean_lcm_noise`
  action description (`doctor.rs:204-211`).
- Dashboard `POST /payloads/gc` requires a prior dry-run token / explicit confirm (§6.3).
- **No body bytes** in any dry-run or apply output — only refs, counts, sizes, and hashes
  already in the DB (contract §13).

## 10. Hermes plugin & CLI reachability

- The Hermes wrapper (`agents/hermes/templates.rs`) surfaces `lcm_status` / `lcm_doctor`
  unchanged; it inherits the §5 additions automatically because it proxies the storage-layer
  JSON. No template change required for the data; only the `lcm_doctor` tool description should
  be updated to mention the `gc` mode (one-line edit at `templates.rs:562`).
- The existing `tracedecay doctor` CLI (`templates.rs:3449-3481`, `cli.rs:260`) is
  **install/agent-integration** health and is out of scope for payload health. Payload health
  is reached via the MCP tools (`tracedecay_lcm_status`, `tracedecay_lcm_doctor`) through the
  existing tool dispatch allowlist (`tool_command.rs:43-47`). A future thin convenience
  `tracedecay lcm health` CLI is **optional and not required** by this spec; if added, it
  should just print the §3 block + §8 state pill as text.

## 11. Security invariants (non-negotiable, from contract §13)

- No payload body bytes in any status/doctor/GC/dry-run/dashboard response or log — only refs,
  counts, byte totals, and hashes already stored in `lcm_external_payloads`.
- Reuse `validate_payload_ref`, `canonical_storage_root`, `ensure_contained` for all path
  logic (contract §10.1). The `root_contained` field surfaces a containment break as an
  **error** state (§8).
- Destructive dashboard/doctor actions require the dry-run-first gate (§6.3, §9).

## 12. Acceptance checks

Each check is testable against a store fixture built into the known state. Fixtures are
constructed by the tests task (`t_f0e07c5c`); this spec defines the expected observables.

### 12.1 Healthy state

Given a store with N referenced payloads, all files present and hash-correct, no orphans, GC
run once successfully:

- `lcm_status.payload`: `missing_count == 0`, `unreferenced_count == 0`,
  `orphan_file_count == 0`, `integrity_mismatch_count` is `null` (deep=false) or `0`
  (deep=true).
- `payload_gc.last_gc_status == "ok"`, `last_gc_at` is set.
- Dashboard `payload_health.status == "healthy"`, pill green.
- `reclaimable_bytes == 0`.

### 12.2 Warning state

Given a store with 2 orphan files (1 past grace, 1 within grace) and 3 unreferenced metadata
rows (all marked on a prior scan, 2 past grace), no missing, GC run ok:

- `orphan_file_count == 2`, `unreferenced_count == 3`.
- `reclaimable_bytes == bytes(2 unreferenced past grace) + bytes(1 orphan past grace)`.
- `reclaimable_bytes_after_grace == bytes(3 unreferenced) + bytes(2 orphans)`.
- `status == "warning"`, pill amber.
- `GET …/payloads/gc` (dry-run) lists exactly the now-eligible refs+bytes; the modal confirms
  the within-grace items are **not** in the removal set.

### 12.3 Error state

Given a store with 1 referenced payload whose file was `rm`'d (missing), 0 orphans, GC ok:

- `missing_count == 1`; `missing_payload_refs` sample contains the ref.
- `status == "error"`, pill red.
- The drill-down "Missing" section shows the ref + owner message/session.
- `POST …/payloads/gc` (apply) does **not** reap the missing payload (contract GP-4); it
  remains reported-only. The dry-run report likewise excludes it from the removal set.
- Separately, with a corrupted file (hash mismatch) and `deep=true`:
  `integrity_mismatch_count == 1`, `status == "error"`, and apply does **not** remove it
  (contract §9 / D-4).

### 12.4 Dry-run/apply gate

- `POST …/payloads/gc` without a prior dry-run token / `confirm` returns 400.
- After a successful apply, `last_reaped_refs` / `last_reaped_bytes` reflect what was removed,
  and `last_gc_status == "ok"`.

### 12.5 Cross-surface numeric agreement

For any fixture, `lcm_status.payload`, `lcm_doctor … diagnostics.payloads`, and the dashboard
`payload_health` block report identical counts and byte totals (computed from shared
helpers — §5.3).

## 13. Field source-of-truth summary

| Want | Existing source | Action |
|---|---|---|
| missing/unreferenced/placeholder counts | `LcmPayloadStatus` (`types.rs:517`), `payload_diagnostics` (`doctor.rs:310`) | reuse |
| orphan-file count | `payload_diagnostics` `orphan_files` (`doctor.rs:340`) | promote into `LcmPayloadStatus` |
| byte totals | `lcm_external_payloads.byte_count` (row) + `fs::metadata` (orphan) | **add** in shared helper |
| `last_gc_*` | GC run-state table (new, `t_bbd369f2`) | **add** struct `LcmPayloadGcStatus` |
| tombstoned count | `[gc'd …]` placeholder prefix (`payload.rs:104`) | **add** count query |
| reclaimable (now vs after grace) | grace math (contract GP-2) over run-state marks | **add** derived field |
| dashboard block | `overview()` (`lcm_api.rs:213`) + new `payloads/health` route | **add** |
| state pill | §8 predicates | **add** (shared classifier) |

## 14. Handoff

- **Implementation consumer** (child task, e.g. `t_baa1d2cf`): implement §3 type/struct
  additions, §4 storage helpers, §5 MCP augmentations + `gc` mode, §6 dashboard routes, §7 UI
  card/modal. Cite this doc section-by-section; conform to the contract (`LCM-PAYLOAD-LIFECYCLE.md`
  §6, §10, §13) for anything this spec defers (especially reap order and grace math).
- **GC dependency** (`t_bbd369f2`): owns the reap algorithm, the `gc` mode body, and the GC
  run-state persistence that §3.2 and §4.2 require. This spec and `t_bbd369f2` agree on the
  surfaced shape; `t_bbd369f2` owns the storage.
- **Tests** (`t_f0e07c5c`): implement the §12 fixtures and assertions; additionally assert
  cross-surface numeric agreement (§12.5) and the dry-run/apply gate (§12.4).
- **Sequencing note:** §3.2 / §5.2 `gc apply` / §6.3 POST / §7.3 confirm flow all hard-depend
  on `t_bbd369f2`. The read-only parts (§3.1 byte/orphan/tombstoned additions, §6.1/§6.2
  dashboard blocks, §7.1/§7.2 card, §8 classifier) can land first and will surface
  `last_gc_at = null` / `lcm_gc = false` until GC arrives.
