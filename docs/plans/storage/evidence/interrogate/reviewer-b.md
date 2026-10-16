# Reviewer B: storage redesign (docs/plans/storage/README.md + sketch.rs)

Spot checks ran read-only against the worktree (branch refactor/storage-read-boundary, master e5a8ede6ea plus
uncommitted step-1 edits). `project_server_lifecycle.rs` was not opened. No builds.

## Findings

### 1. [critical] Catalog-membership GC can delete every repository's durable memory
**Location**: README §6 growth source 1 ("Any `repos/*` directory missing from the catalog is deleted at open and every tick"); sketch `Storage::open` doc and TODO l.80-86; `retention::plan` step 1 (`UnregisteredDirectory`, "always"); `StorageClass::Catalog` policy `SalvageAndSwap` / `Migrate`.
**Finding**: The only key that protects `repos/<id>/memory.db` from deletion is a row in `catalog.db`. If the catalog loses that row, the sweep `rm -r`s the whole directory, `memory.db` included. That can happen on a lossy row-by-row salvage of a corrupt catalog, on a failed migration, on a user deleting `catalog.db`, or on an importer that crashes after writing `repos/<id>/` but before it commits the catalog row. Memory is the one class the fixed inputs say survives forever, and here a fault in a different, smaller file deletes it.
**Evidence**: The sketch runs the sweep in `Storage::open` before any request, with no quiet period and no confirmation. `plan()` lists unregistered dirs first and "always". `SalvageAndSwap` is the catalog's corruption policy, and a row-by-row salvage loses rows by definition. AGENTS.md "No secondary copies": "Never delete the sole active durable copy under the guise of this cleanup."
**Suggestion**: Don't make catalog absence a reason to delete. Each `repos/<id>/` should carry its own identity record, and the sweep should re-register any directory the catalog lacks. Only `forget` should delete memory. At minimum, the sweep must never touch a directory that contains `memory.db`.

### 2. [critical] LCM summaries are kept in the class whose schema and corruption policy is "unlink"
**Location**: README §2a "Two merges" (summaries join evidence because they "are not rebuildable"); §5 evidence row; §7 "evidence: unlink, re-ingest"; §6 "evidence evicts oldest first (summarized rows, then all rows)"; sketch `OnSchemaMismatch::Reingest` for Evidence.
**Finding**: Summaries go in `evidence.db` because they cannot be rebuilt. But every evidence schema change, and every confirmed evidence corruption, unlinks that file. The budget kernel also deletes the raw rows a summary covers first. So after any evidence schema bump, both the summary and its raw rows are gone. Anything older than the host's own JSONL retention is then lost for good, and LLM summaries cannot be regenerated.
**Evidence**: The grounding lists LCM 15, temporal 8 and projector v7 schema generations, so evidence-class schema changes are routine, not rare. The design's own rationale for the merge ("raw eviction depends on them") makes the loss compound: eviction is safe only while the summary exists.
**Suggestion**: Put summaries in a durable class with migrations (`memory.db`, or a small `summaries.db` with the Migrate policy), or make the evidence class Migrate. Then summary-first raw eviction is safe.

### 3. [critical] `retrieval_anchors` is a cross-file atomicity the split breaks (memory to evidence FK)
**Location**: README §2a "No transaction spans two files" and the layout ("anchors" in `evidence.db`); §7 importer ("copies `memory_v2_*` rows").
**Finding**: Memory facts hold foreign keys into `retrieval_anchors`, and one fact batch writes the fact, its anchors and its aliases in one transaction. Temporal (derived) tables also hold foreign keys into `retrieval_anchors`. The design moves anchors into `evidence.db`, which is evictable at 180 days and 8 GiB and is unlinked on schema change. Durable memory then points at rows in a different, disposable file, and the write that creates both can no longer be atomic. The importer copies only `memory_v2_*`, so with `foreign_keys=1` (today's connection PRAGMA) the import either violates the FK or needs the anchors too, and the plan has no home for them.
**Evidence**:
- `crates/tracedecay-runtime-core/src/db/memory_v2/schema/baseline.rs:114-127`: `memory_v2_evidence ... FOREIGN KEY(anchor_id, owner_json) REFERENCES retrieval_anchors`.
- `crates/tracedecay-rusqlite-runtime/src/repository/fact/writes.rs:1-4,87-127`: "The fact, anchor, and current-projection rows one fact batch writes", which inserts into `retrieval_anchors` and `retrieval_anchor_aliases`.
- `crates/tracedecay-global-db/src/session_temporal_schema.rs:56,445,516`: temporal FKs to `retrieval_anchors`.
- The README's no-cross-file claim cites only Work/Workflow/handoff greps.
**Suggestion**: Decide where anchors live. One option is anchors that memory cites stay in `memory.db` (memory owns them), with derived and evidence referencing them by value and no FK. Then redo the cross-file audit over every FK in the moved schemas, not only Work SQL.

### 4. [critical] `WorkJournal::transition` over one aggregate cannot express today's Work transactions, and the sketch replays after deciding
**Location**: sketch `WorkJournal::transition` l.444-466, `JournalAggregate` sealed set l.418-427; README §8 ("77 methods to 3").
**Finding**:
- (a) `decide: FnOnce(Option<&A>)` sees only the single keyed aggregate. Real Work transactions read and check several entities and an aggregate count in one `BEGIN IMMEDIATE`. `insert_retry_bounded_in_transaction` runs these steps:
  1. replays by `command_id`;
  2. checks retry evidence;
  3. checks that the attempt is absent;
  4. reads `work_run_controls_v1` (run reservation);
  5. checks first-run admission over other attempts;
  6. checks the placement root;
  7. inserts the receipt;
  8. checks capacity across sibling attempts (`require_capacity`);
  9. inserts the attempt.

  None of this fits one `Option<&WorkAttempt>`.
- (b) The seven sealed aggregates omit whole entities that have their own transactions today: `authorized_scope_sets_v1` (CAS), run controls, placements, effect holders, fences, blocked intervals, duplicate and leak adjudications, product events and graph versions, workflow handoffs, effect, census and disposition journals, and all of `remote_*`. `Storage` has no remote handle at all, although §4 deletes the remote-replay actor.
- (c) The sketch runs `decide` before `idempotent_replay`. A retried command whose first execution already moved the state gets a refusal from `decide` instead of its original outcome. Today's code replays first (`retry.rs:88-99`).

**Evidence**:
- `crates/tracedecay-rusqlite-runtime/src/work/retry.rs:81-114,292-330`.
- 25 files call `begin_immediate(` in rusqlite-runtime, including `remote/*` ×9, `repository/scope_set.rs`, `work/effect_holder.rs`, `work_run_control.rs` (blocked intervals + run controls + attempts) and `work_attempt.rs` (attempts + fences + run controls + placements).

**Suggestion**: Make the closure receive a read view of the journal (`FnOnce(&JournalView) -> Decision`), where the view can load any aggregate and run bounded counts inside the same write job. Keep the decision pure over that view. Check replay on an operation id passed outside the decision, before `decide` runs. Give remote and scope sets an explicit home in the surface.

### 5. [critical] Jobs are `FnOnce`, so the retry the containment design depends on is impossible
**Location**: sketch `executor::WriteJob` / `ReadJob` (`Box<dyn FnOnce ...>`, l.825-826), `LaneHandle::write/read`; README §3 ("in-flight reads ... are interrupted at their deadline and retried after the fence"); §5 #4094 row ("retry the job once on a fresh connection"); `fault::Recovery::ReconnectAndRetry`.
**Finding**: Three paths assume a job can run again after it has already run:
- the #4094 confirm ladder;
- the fence's interrupt-and-retry;
- batching, where several jobs share one `BEGIN IMMEDIATE`. If COMMIT fails with IOERR or CORRUPT, every batch-mate's closure has already been consumed.

A `FnOnce` cannot be re-run. "One bad connection costs one job" therefore becomes "one bad connection fails every job in the batch, with no retry", and an interrupted read becomes a caller-visible error.
**Evidence**: The type aliases in the sketch, set against §3 and §5 text that promises reruns. The confirm ladder is listed as a convergent "ships as written" item.
**Suggestion**: Make jobs `Fn` (or `FnMut`) over owned, clonable inputs, so the executor can replay them, and state that a retry re-runs the whole batch. Otherwise drop the retry claims and surface `Unavailable`.

### 6. [critical] The cutover order contradicts its own rationale: the fence is not total at step 2, and one read class is never removed
**Location**: README "Cutover plan" steps 1-3 and 6; "Rejected: the judge's cutover order" ("The executor's fence is only total once every read is a bounded job ... the pool rewrite is safest once its callers are job-shaped").
**Finding**:
- (a) Step 1 converts only the typed `dispatch_read` (10 call lines). Step 2 then claims a total fence and proves "continuous readers keep the WAL at or under soft plus one batch". At step 2 these readers still exist outside the executor:
  - sync exact-SQL reads (`query_with_priority`) until step 3;
  - `sqlite_read_snapshot` (online-backup readers used by in-daemon `storage_report.rs` and `orphan_stores/collection.rs`) until step 6;
  - CLI and doctor standalone registries until step 6.
- (b) The runtime-core async engine `Connection` (`db/engine/connection.rs:325` `cx.spawn_blocking`, class (b): LCM and temporal) is never named in any step or in §4's "Deleted" list. So both "nothing parks a blocking-pool thread on SQLite" and fence totality stay false at the end of the plan.
- (c) From step 2 to step 3, sync `ExactSqlHandle::begin_immediate` interactive transactions, which wait for up to 120 s on round trips, would run on the shared 6-thread pool. A few concurrent Work requests can park the whole pool, and if a caller's round trip needs a read job on the same pool, it deadlocks.

The rejection paragraph argues that the pool should come after its callers are job-shaped, which means step 3 before step 2.
**Evidence**: The grep counts above; grounding §4 table classes (a) to (d); `exact_sql/command.rs` (interactive transactions on the writer thread, 30 s idle / 120 s total, per grounding §3).
**Suggestion**: Order the steps as read boundary, then all readers (async engine `Connection`, exact-SQL reads, snapshot), then the async journal (step 3), then the pool and fence. Or land the pool in step 2 without the fence and add fenced TRUNCATE only after the last foreign reader is deleted. Add the async engine `Connection` to an explicit step's delete list.

### 7. [warning] The journal's "reset" policy destroys user-authored durable state
**Location**: README §5 journal row, §7 "journal: reset"; sketch `Journal` policy (`ResetWithFinding`, `OnSchemaMismatch::Reset`); cutover step 5 ("move the journal tables out") and step 6 (the layout move).
**Finding**: `journal.db` holds more than operational state:
- registered `WorkflowDefinition`s, which users author through `tracedecay_workflow_register_definition` / `activate_definition`;
- native-integration approvals;
- handoff grants;
- authorized scope sets.

The design calls the class "authoritative", yet every journal schema change, and the moves in steps 5 and 6 for beta installs, wipes these with only a `JournalReset` refusal. Zack's durable class includes "configurations etc other user state" (synthesis Q6).
**Suggestion**: Split durable journal rows (definitions, approvals, scope sets) into a Migrate class: `catalog.db` for profile-scoped rows, or a small per-repository durable file. Alternatively, give the journal additive migrations. Either way, apply reset only to in-flight aggregates.

### 8. [warning] Root-commit identity is not stable, and re-keying strands durable memory
**Location**: sketch `identity::IdentityAnchor`, `Marker` ("a cache ... not an authority"); README §2.
**Finding**:
- (a) "Smallest parentless commit reachable from HEAD" depends on HEAD. On an orphan branch (`gh-pages`, `checkout --orphan`), or after a `--allow-unrelated-histories` merge that adds a smaller root, a fresh clone or a deleted marker resolves to a different `repo_id` than existing clones do. History rewrites (`filter-repo`, root rebase) do the same.
- (b) Re-keying from `Path` to `RootCommit` ("re-keys at first commit") and from `Remote` to `RootCommit` ("when unshallowed") moves nothing. Memory written before the first commit, or in a shallow agent clone, stays in a store set that idle collection reduces to an unreachable `memory.db`. `git init`, then work with an agent, then commit, is a common journey.
- (c) The explicit `--as` / `--split` override lives only in the marker that the design calls a disposable cache, and that marker sits in `<git_common_dir>`, shared by every profile on the host. Deleting it, or re-cloning a split fork, silently merges the fork back.
- (d) Today a project root can be a subdirectory of a worktree (`ProfileRoot::discover_project_root`, `runtime-core/src/config.rs:246-265` walks up and returns the first initialized directory). `Checkout::discover` "walks up to the worktree root", so monorepo subproject stores collapse and code indexes widen to the whole monorepo. Neither is mentioned.
- (e) A non-git directory gets a `Path` anchor and a full four-file store set, not `_profile`, so scratch directories mint store sets, which is the "30+ stores" shape again.
- Submodules are not addressed.

**Suggestion**: Use the anchor of a stable ref set (all roots reachable from any ref, recorded once and kept), and store overrides and alias history in the catalog, keyed by anchor. Re-key by moving or merging the store set, at least `memory.db`. Say what happens to subdirectory projects and non-git directories.

### 9. [warning] The profile budget does not bound code generations, so 153 GB can recur
**Location**: README §6 ("hard profile budget (default 64 GiB, memory excluded)"; "evidence evicts ... derived trims ... journal prunes ... memory only raises a finding"); sketch `retention::plan` steps 5-7; layout `worktrees/<id>/` with "active plus 2, 7 d".
**Finding**: The budget planner evicts only evidence and derived. Code generations ("routinely ~1 GiB each", grounding §7) plus sealed Grafeo generations are kept per live worktree at active + 2. Their total scales with the number of live worktrees (32 linked worktrees on this host today) and is outside every budget. The design also replaces today's per-scope 8 GiB cap with a count rule. So growth from live worktrees is bounded only by hope.
**Suggestion**: Put `worktrees/` and `code/segments/` under the planner, for example by dropping non-active generations and then whole least-recently-used worktree scopes, which re-index on demand.

### 10. [warning] The fixed pool turns per-file stalls into profile-wide head-of-line blocking
**Location**: README §4 (N=6, one thread reserved for foreground); sketch `Storage::repository` ("catalog upsert ... one small write"), `identity::resolve` "on an executor thread (gix walk ...)".
**Finding**: These all occupy pool threads, and none can be interrupted:
- a FULL-sync commit stuck in fsync (this host has 5-31 s fsync stalls);
- `quick_check(1)` on an 8 GiB evidence file (O(file size));
- WAL recovery at lane open;
- a full gix history walk to find roots.

`sqlite3_interrupt` and the 30 s statement deadline stop none of them. One reserved thread serves every repository's journal, memory and catalog, so one stalled fsync freezes Work across all repositories. Today's thread-per-file layout isolated this. Separately, `Storage::repository` runs on "any request with a cwd" and does a FULL-sync catalog upsert each time, which puts an fsync on the hot path of every MCP call and hook.
**Suggestion**: Run opens, quick_check, identity walks and salvage on a separate bounded maintenance pool, not the serving pool. Debounce checkout `last_seen` writes (in memory, flushed by maintenance). Measure p99 with injected fsync stalls before step 2 lands.

### 11. [warning] Fence totality depends on convention, and the WAL bound is misstated
**Location**: README §3 ("the executor holds every connection's interrupt handle"; "The WAL peaks at the soft limit plus one batch"); sketch `checkpoint_decision` comment.
**Finding**:
- (a) `sqlite3_interrupt` has no effect while no statement is running. A read closure that holds `ReadTx` and computes in Rust (ranking, holographic similarity, hydrating JSON) keeps its snapshot open past the deadline, and the fence waits. Nothing in the types prevents this.
- (b) Nothing pauses writes while the fence drains. The WAL therefore peaks at the soft limit plus the read deadline (5 s for evidence and derived) times the write rate, not plus one batch. During a derived rebuild that can exceed the 229 MB incident WAL.

**Suggestion**: Bound the closure, not just the statements. Hand read closures only rows (a fetch phase under the snapshot, compute after the transaction ends), or make the executor end the transaction at the deadline. Either pause write batches once the fence is raised, or state the real bound.

### 12. [warning] Evidence eviction cannot see unprojected rows
**Location**: README §6 ("evidence evicts oldest first ... the derived cascade deletes rows of the evicted sequences"); sketch `Eviction::Evidence` (no projection guard).
**Finding**: The per-stream `projected_through` cursors live in `derived.db`, and nothing is ATTACHed. The evidence kernel therefore cannot tell which rows have not been projected, for example a stream parked for up to an hour or a derived file under rebuild. Rows evicted before projection vanish with no dead letter and no loss receipt, which breaks "the checkpoint never advances over a row without either projecting it or recording a dead letter".
**Suggestion**: Have the planner read each repository's minimum projection cursor (a derived read job) and cap eviction below it. Or write a loss receipt for every unprojected sequence it evicts.

### 13. [warning] Transient faults dead-letter healthy rows en masse
**Location**: sketch `sessions::projection::dispose` and `MAX_PARKED` (1 h); README §5 projection row.
**Finding**: `Transient` is documented as "says nothing about the row", yet after an hour parked the head is dead-lettered anyway. If derived is `Recovering` or `Rebuilding`, or evidence refuses writes during a re-ingest, for more than an hour, every stream in that repository records a gap. Those rows re-queue only once per daemon incarnation or on a projector version change, so a lane outage becomes data gaps until the next restart.
**Suggestion**: Park transient failures per lane, not per row, and never dead-letter on a transient fault. Dead-letter only on a deterministic fault that repeats.

### 14. [warning] Step 1's proof measures a proxy, and the in-progress diff only moves the park
**Location**: cutover step 1 ("Proof: the native reader-wait counter reads 0"); uncommitted diff in `crates/tracedecay-rusqlite-runtime/src/repository/attachment.rs` (`dispatch_read` now wraps the same reader acquire and snapshot in `native_runtime::spawn_blocking`).
**Finding**: The park moves from a native worker to a blocking-pool thread, which is class (b). The grounding says those waits "are not counted", so the counter reads 0 by construction while threads still park on reader admission. §4 says nothing parks a blocking-pool thread. A step-1 proof should therefore measure parked threads of any kind, or the step should be described honestly as a relocation that step 2 supersedes.

### 15. [warning] The beta importer is underspecified for the data it must move
**Location**: README §7 upgrade path; §9 `init`; cutover step 6.
**Finding**:
- (a) It runs only from `init`. A beta install that upgrades and lets the daemon auto-start never imports, and its memory is invisible.
- (b) Up to 14 old `proj_*` stores (one per clone) collapse into one `repo_id`. The importer must merge their fact lineage and supersession chains and rewrite `project_id`, which is part of `memory_v2_*` primary keys, owner JSON and receipts. Those tables are guarded by `*_no_update` / `*_no_delete` triggers. No merge rule is stated.
- (c) Anchors (finding 3).
- (d) "Deleted two releases later" loses memory for anyone who skips releases.
- (e) It reads old SQLite files from the CLI, which bypasses "the daemon is the only mutable authority".

**Suggestion**: Run the import inside `Storage::open` in the daemon. Define the multi-store merge. Gate deletion of the old directory on a read-back.

### 16. [warning] Turning the read deadline into an error breaks ranked search on large repositories
**Location**: README §4 (reads interrupted at class deadline); `SessionStore::search` with a 5 s deadline.
**Finding**: FTS5 `bm25` ranking (`crates/tracedecay-sessions/src/runtime/store_access/sessions.rs:694,723`) must score the whole match set before the first row comes back, so keyset paging does not bound it. On an 8 GiB evidence or derived pair, common-term searches will return `Unavailable{TimedOut}` deterministically, with no partial result. The tradeoffs section says "bulk work pages", but search cannot page.
**Suggestion**: Bound search by candidate count (top-k over a capped match set) and return partial results flagged as partial, rather than timing out.

### 17. [nit] Inconsistencies and AGENTS.md drift in the package
- The sketch `OnCorruptFile::ResetWithFinding` still says "Rename aside (debris, collected after 7 d)". That is a retained copy, against AGENTS.md "No secondary copies", and it disagrees with README §5 "recreate empty". The judge asked for this fix.
- `ProjectionBudget::page(256)` is still on the public surface (README Usage, sketch l.562-571). The judge asked to replace it with an internal default.
- The README links [`decisions.tsv`](decisions.tsv), but `docs/plans/storage/` contains only README.md and sketch.rs.
- The plan supersedes plans 02/38/39/40 but is not registered in `00-plan-set-index.md`, which AGENTS.md makes the "sole roadmap precedence".
- Contentless FTS5 needs `contentless_delete=1` for the eviction cascade's deletes, because the original text is already gone from evidence.
- `SystemIoFailure` is classed as suspect corruption, so a transient EIO on a flaky FS that `quick_check` then passes is treated as "poison input" and dead-lettered.
- The lane LRU cap of 64 has no effect: lanes already close after 60 s idle, and the cap "evicts only idle lanes", so with 30 repositories × 4 files active it can't hold.
