# Arena cross-judge: storage redesign

Inputs: task.md, rubric.md, design-red-flags.md, ground/explanation.md, why/synthesis.md, growth-sources-4099.md,
candidates 1 to 4 (design.md + sketch.rs). Spot checks ran read-only against the worktree at e5a8ede6ea.
Fixed facts applied: one engine (SQLite) unless a measured reason; no backups; memory durable forever, never
size-evicted; identity = repository; drain needs a dead-letter path; synchronous per storage class; threads must
not scale with file count.

## Scores (1 to 5)

| Criterion | C1 profile classes | C2 repo classes | C3 event log | C4 same files |
|---|---|---|---|---|
| 1 Incident closure | 5 | 5 | 5 | 4 |
| 2 Interface depth | 4 | 5 | 3 | 2 |
| 3 Partitioning and containment | 4 | 5 | 4 | 2 |
| 4 Cutover realism | 3 | 4 | 2 | 4 |
| 5 Evidence fit | 4 | 4 | 4 | 4 |
| 6 Simplicity | 4 | 4 | 2 | 3 |
| Total | 24 | 27 | 20 | 19 |

## Candidate 1: one store set per profile, partitioned by class

Evidence per criterion.

1. Closure 5. Fixed ~20 paths sampled every 30 s into a size-history table (§6); evidence in weekly segments so
   retention is unlink; no per-project directories, so orphan `proj_*` and the `git_common_dir` liveness rule
   cannot exist (§6 item 1); TRUNCATE barrier with 2 s read deadlines (§3); inode watch → `ProfileLost` in ≤30 s
   (§9); dead letter per stream with redrive per incarnation (§5 table).
2. Depth 4. `Storage` 10 methods + 6 concrete handles, no single-impl traits, no serde/SQL/WAL on the surface
   (§8). Deduction: the six class accessors put the on-disk partition on the surface. Capture is
   `evidence().capture` and projection is `views().drain` (Usage, call site 2): the caller coordinates two
   handles for one pipeline. `Lane` is a per-call argument on Work verbs (sketch l.346-355), so scheduling policy
   leaks into every call site.
3. Containment 4. Table with sync column per class (§2a); evidence→views only via per-stream cursor, no cross-file
   txn; threads fixed at N. Deduction, admitted in §5: a corrupt `views.db` or evidence segment hits every repo.
   Mitigated by per-repo rebuild priority, but the rubric asks that a bad file block only its own partition.
4. Cutover 3. PR 1 is the Grafeo→SQLite pack swap with a parity/RSS gate and a named fallback; good ordering. But
   PR 6 deletes `sessions.db`, `user-sessions.db`, the queue, payloads and the ingest split in one wave, and PR 2
   rewrites the runtime under existing files while PRs 4 to 7 then move every file again. Two full passes over
   the same crates. Honors one engine (it reaches one engine). Importer one release only.
5. Evidence 4. Spot checks: "Grafeo confined to `tracedecay-graph-db`; no other Cargo.toml names it" holds
   (only `crates/tracedecay-graph-db/Cargo.toml` plus the workspace root). "`COMPACT_ROUND_TRIPS_BYTES` stays
   off because a 431-file generation fails its digest proof" holds (`docs/graph-at-rest/README.md:36`).
   `sealed_store.rs` dependency-bearing-generation claim holds (lines 20-22). Reuses FairQueue (it is
   `pub(crate)` at `admission/queue.rs:79`, so it moves rather than imports), dwell windows, ledger. Deletes the
   runtime-core async `Connection` with argument.
6. Simplicity 4. One crate, concrete handles. New concepts: class, segment, pack, tenant, lane. Call chain
   caller → handle → executor.

Red flags: information leakage (class accessors and `Lane`/`Tenant` as public per-call inputs); mild shallow
module in `Storage` (callers coordinate `evidence()` then `views()`). Root-commit identity merges forks (admitted).

Graft most worth taking: time-segmented evidence files (`evidence/<week>.db`), which turn retention into
`unlink`, cap rebuild windows, and remove VACUUM from the hottest class. It drops straight into C2's per-repo
`evidence.db` as `evidence/<week>.db`.

## Candidate 2: one store set per repository, partitioned by class, on one bounded executor

1. Closure 5. Persisted `catalog.storage_usage` per tick with growth rate (§6); worktree liveness is `stat(root)`
   and the `live_git_common_dir_keeps_a_linked_worktree_store_live` rule is deleted; unregistered `repos/*` dirs
   collected at open and every tick; fenced TRUNCATE so WAL peaks at soft + one batch and no hard-drain state
   exists (§3); all-async lanes delete `dispatch_read`, sync `ExactSqlHandle`, the actor threads (§4);
   `st_nlink == 0` probe → close lane or `ProfileLost` (§5 last row); scratch container dies with the seal, so no
   Grafeo WAL outlives a seal.
2. Depth 5. 45 methods on 8 handles plus one sealed trait (§8 table). `SessionStore` hides the evidence/derived
   split; `Repository` hides files. Work: 77 methods → `get/list/transition` over pure decision functions.
   Errors are `Busy/Unavailable/Refused/NotFound`, no engine text. `StorageClass::policy()` is one table every
   policy derives from (sketch l.309-365), which is exactly the maintainer-extensibility property the task asks
   for. Minor leak: `SealScratch::path()` hands a filesystem path to the code-index runtime.
3. Containment 5. One class = one file per repository; `forget` = rmdir; derived unlink-and-rebuild; evidence
   refuse-confirm-reingest; journal reset with typed `JournalReset`; memory salvage-and-swap. No cross-file txn:
   consumer keeps its cursor (§2a "No cross-file transaction exists"). Threads N, lanes LRU-capped at 64.
4. Cutover 4. PR 1 (executor under the existing attachment, with the 64-store thread test and the rewritten
   #4097 test) is the right riskiest-first unknown and lands on today's files and schema. PRs 2 to 5 stay on the
   existing layout; the layout move is PR 6. Deletes 6 crates. Grafeo kept only as an immutable sealed format:
   not a second mutable engine, but still a second format; argued, not measured (open question 3). Beta path:
   `wipe --stale` + `init` plus a one-shot memory/config importer deleted two releases later.
5. Evidence 4. "`graph-db/location.rs:42-45`: Grafeo does not surface every WAL append failure" holds (lines
   44-46). `LcmSummarizerMode::HermesAuxiliary` exists (session_suite). `live_git_common_dir...` at
   `orphan_stores/tests.rs:271` holds. Unverified: "12 standalone registry sites" (I found `standalone_session`
   referenced from 5 non-test files; the 12 may be the explorer's count). Reuses FairQueue, ledger, sealed
   generations, connection policy. The host-admission spool is not named in the module map (kept or deleted?).
6. Simplicity 4. One new crate; concepts: Repository, class, lane, aggregate. The generic `transition<A>` with a
   sealed trait is the one piece a maintainer must learn. Call chain caller → handle → lane.

Red flags: none structural. Watch: `ProjectionBudget::page(256)` exposes batch size to the refresh worker;
journal "rename aside, debris collected after 7 d" is a kept copy for 7 days, which brushes the no-backups rule
(delete on reset instead; the finding is the record).

Graft most worth taking into another base: `StorageClass::policy()` as the single policy table (sync, soft WAL,
deadline, on-corrupt, on-mismatch, evictable), and `transition<A>` with pure decision functions in contracts.

## Candidate 3: framed event log as the only authority for evidence and journals

1. Closure 5. No WAL for 90% of bytes; retention is segment unlink; governor measures by `statx` without opening
   a database (§6); per-stream parking with 1 h ceiling then dead letter (§5 Projection); `confirm` verdict ladder
   (ProcessLocal/FileCorrupt/PoisonInput) closes #4094 by experiment, not string; `ProfileAnchor` checks
   `st_nlink` on every group commit (§9); 7 threads total.
2. Depth 3. About 96 methods, 2 public traits. Deductions: `Freshness` on every query and `Coverage` in every
   answer expose the projection stage to callers (red flag: public options expose internal stages);
   `Repo::log::<Evidence>()` makes the caller pick the storage class; `Expect::At(n)` exposes OCC; the design
   admits two-step reads (Tradeoffs). It hides SQL, files, engine, and lanes well.
3. Containment 4. Record-level CRC containment is finer than any SQLite candidate; views sharded by month;
   memory alone in a FULL file. Deduction: containment of the hot class rests on a new 700-line format plus fuzz
   suite that does not exist yet; "SQLite has no page checksums" is true but the fix is also available inside
   SQLite (cksumvfs) if one wanted it.
4. Cutover 2. Departs from the one-engine decision (§1 "the argument against the one-engine decision, made
   directly") with an argument, not a measurement; the judge's fixed facts make that disqualifying on this
   criterion regardless of the argument's quality. It also event-sources Work/Workflow (24 ports → aggregates),
   swaps Grafeo for SQLite behind a gate, and introduces a format, in one program. PR 1 is correctly the
   riskiest unknown (do Work invariants fit single streams) with a named fallback (`ledger.db`). Nine PRs.
5. Evidence 4. Claims checked and holding: hook admission ledger is exactly 1,746 lines
   (`tracedecay-hooks/src/admission_ledger.rs`); host-admission `spool/frames.rs:5` imports
   `tracedecay_domain::framed_log`; `tracedecay-private-fs/src/framed_log.rs` exists (1,152 lines) and is used
   by the hooks ledger, observability `delivery_spool.rs:16`, and automation `effect_runtime/journal.rs` and
   `run_ledger.rs`; rusqlite `hooks` feature is on (`rusqlite-runtime/Cargo.toml:20`); `pub trait Remote*`
   count in store+contracts is 24 (claim "about 20"). Replaces spool, sealed generations, ledger and async
   Connection, each with argument.
6. Simplicity 2. Two new crates and the most new concepts: segments, footers, StreamDirectory, upcasters,
   aggregates with snapshot-at-roll, Freshness, Coverage, month shards, DropPending on Windows.

Red flags: information leakage (Freshness/Coverage/Expect/Log<C>); "limits are symptoms, not knobs" removes
tuning that the hardware will need (fixed lane counts, 250 ms window).

Graft most worth taking: the `confirm` verdict ladder (drop connections, rerun once, then `quick_check(1)`;
pass → poison input, fail → file) which distinguishes a poison row from file corruption. C2 today stops at
"second failure on a fresh connection classifies the file", which would quarantine a healthy file for one bad
row. Also worth lifting: `statx`-based measurement that never opens a database.

## Candidate 4: same files, one governor

1. Closure 4. Executor with turn budgets and `WrongThread` sync door (§4); `next_wal_phase` ladder with a
   `Stuck` state that opens the gate and raises a finding (§3); per-stream parking with `redrive_after` (§5);
   `StorageLedger` samples every 60 s and persists 30 d (§6); Grafeo WAL timer checkpoint gated on PR 1;
   identity convergence by root commit. Deductions: a corrupt `sessions.db` still takes Work/Workflow/grants with
   it (admitted §2a); the #4100 adjacency (grants behind the same writer as ingest) is reduced by turn budgets,
   not removed; Grafeo heap residency is "fixable" by hibernate, unproven. The PASSIVE-first rung remains in the
   ladder, so the ratchet is bounded rather than impossible.
2. Depth 2. Admitted flat: 220 → 223 port methods; 19 traits and 24 Work ports kept as pass-through async
   flips (red flag: pass-through method); `WriteKind` chosen at every write site puts durability policy on the
   surface (information leakage); typed wire enums remain internally. The new 14-method executor surface is
   deep, but the design does not use it to remove shallow modules.
3. Containment 2. No split; evidence, projections, journals and queue share a file and a WAL. §2a's incident
   table argues each incident is closed elsewhere, but the rubric asks that a bad file block only its own
   partition, and the fixed inputs say ADR-003 stands unless refuted with evidence; "has not fired on disk" is
   absence of evidence, and the design itself concedes the journal split costs zero atomicity and zero threads.
   Per-request `synchronous` toggling inside one file also departs from "synchronous per storage class".
4. Cutover 4. Ten incremental PRs on existing files, mechanical port flips, PR 1 is the real unknown (live Grafeo
   checkpoint) with a kill criterion, no importer needed, deletes ~6.5 kLOC against +2 kLOC. Keeps two engines
   with an argument ("the ruling was about Turso"), not a measurement.
5. Evidence 4. "Work/Workflow SQL references no observation, LCM or queue table" holds (only `pending_*_observations`
   domain names match). `collapse_replayed_wal` calls `database.wal_checkpoint()` on `&GrafeoDB`
   (`recovery.rs:202-234`) holds. "Progress handler already installed at `connection/mod.rs:1131`" is
   `#[cfg(test)]` (`with_progress_cancellation`); the production precedent is `exact_sql/guard.rs:222`. Claim holds
   in substance, citation wrong. `synchronous=NORMAL` at `connection/mod.rs:812` holds. Reuses the most existing
   authorities of any candidate.
6. Simplicity 3. Zero new crates, but the end state keeps every existing concept and adds executor, ledger,
   governor, WAL ladder, fault supervisor on top. Simple change, not a simple result.

Red flags: pass-through methods (24 ports, 19 traits forwarding to closures); information leakage (`WriteKind`
per call); `StoreExecutor::block_on` is a sync door the task asked to remove.

Graft most worth taking: the pure, table-tested `next_wal_phase` state machine with an explicit `Stuck` phase
for foreign readers (gate opens, writes never blocked, finding, retry every 60 s). C2's fence assumes the daemon
is the only process; a developer's `sqlite3` shell would stall its fence forever. Also worth lifting: PR 1's
live Grafeo checkpoint stress test as a pre-gate for C2's PR 4 scratch container.

## Convergence (signal)

All four independently arrive at: one fixed-size executor with closure-shaped read and write jobs; reads bounded
by a deadline enforced through the progress handler or `sqlite3_interrupt`; a reader fence or barrier around a
TRUNCATE (C4 keeps PASSIVE as a first rung); an idle-WAL timer; no interactive transactions; the whole public
surface async (C4 keeps one `block_on` door); fault classification by result code plus reconnect-once before
blaming the file; TEMP tables banned; per-stream ordering with bounded attempts and a dead letter; worktree
liveness by root existence, never `git_common_dir`; root-commit repository identity with the marker as override;
payload files inlined as blobs (C1-C3); idempotency ledger pruned by age, not incarnation; response handles swept
per tick; persisted size samples; memory at FULL, never evicted; a one-shot memory importer deleted later;
`fstat`/inode probe that exits the daemon when the profile vanishes. Treat these as settled.

## Divergence

Unit of storage: profile-wide class files (C1) vs per-repository class files (C2, C3) vs status quo (C4).
Grafeo: dropped (C1, C3), narrowed to an immutable format (C2), kept whole (C4). Evidence substrate: SQLite
(C1, C2, C4) vs framed log (C3). Work ports: concrete async methods (C1), generic `transition` over pure
decisions (C2), event-sourced aggregates (C3), mechanical async flip (C4). Splitting `sessions.db`: yes (C1-C3),
not yet (C4). Containment grain: file per class (C1), file per class per repo (C2), record (C3), none new (C4).

## Recommended base: Candidate 2

A future maintainer extends C2 without breaking invariants because every policy hangs off one enum:
adding a class is one `StorageClass::policy()` row, and durability, fence threshold, deadline, corruption policy
and schema policy come with it. Containment is by directory, so `forget` and GC are `rmdir`, and no repo's fault
reaches another. The public surface (45 methods, 8 handles) hides the partition entirely; a caller cannot tell
evidence from derived. The cutover lands its riskiest piece (executor under today's attachment) first, on
today's files, and moves the layout last. It honors the SQLite ruling for every mutable byte and is honest that
sealed Grafeo is a second format kept for a measured property (mmap open), not a second mutable engine.

C1 ties on closure and nearly on depth; it loses on containment (whole-class blast radius across repos) and on
cutover (two passes over the same crates). Where tied, the smaller surface wins, and C2's is smaller (C1 keeps
77 Work verbs plus 39 fact verbs as methods).

Grafts into C2, in order: (a) C4's `next_wal_phase` `Stuck` state for foreign readers; (b) C3's three-way
`confirm` verdict with `quick_check(1)` so a poison row never quarantines a healthy file; (c) C1's weekly
evidence segments per repo so retention is unlink; (d) C3's `statx` measurement for directories so the usage
sample never opens a cold database; (e) C4's Grafeo live-checkpoint spike as a pre-gate before C2 PR 4.

Fix in C2 before implementation: delete the 7-day journal "debris" (no kept copies); name the host-admission
spool's fate in the module map; replace `ProjectionBudget::page(256)` with an internal default so the refresh
worker carries no batch size; verify the "12 standalone registry sites" count.
