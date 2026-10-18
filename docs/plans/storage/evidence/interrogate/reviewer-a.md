# Reviewer A: storage redesign (docs/plans/storage/README.md + sketch.rs)

Scope: the design package at worktree agent-a9a062786c04623b1, checked against the ground and why
documents and read-only source spot checks. `project_server_lifecycle.rs` was not opened. No builds.

## Findings

### 1. [critical] The unregistered-directory sweep can delete memory.db, so durable memory depends on catalog integrity
**Location**: README §6 item 1 ("Any `repos/*` directory missing from the catalog is deleted at open and every tick"); sketch `Storage::open` TODO "sweep repos/ dirs absent from catalog"; `retention::plan` step 1 ("unregistered dirs -> UnregisteredDirectory (always)"); catalog `on_corrupt_file: SalvageAndSwap`.
**Finding**: Whether `repos/<id>/memory.db` survives depends on a row in `catalog.db`. Several paths lose that row: a salvage that drops rows from damaged pages, a catalog that is recreated or deleted by an operator, or a forward migration that goes wrong. On the next open or tick, the sweep then deletes the whole repository directory, memory included. The planner's statement "Memory never appears" holds only for `RepositoryButMemory`. `UnregisteredDirectory` has no such guard.
**Evidence**: The catalog is the highest-churn FULL file in the design: per-hook `hook_events`, per-tick usage, and a checkout upsert on every `repository()` call. That makes it the file most likely to need salvage. Today's code has a comparable rule in the other direction. Explanation §10 says "A `memory_*` row blocks collection forever" (`memory_v2_rows_block_orphan_store_collection` exists in the schema). The redesign drops that guard without replacing it. This violates the fixed input "durable memory kept forever".
**Suggestion**: Make each `repos/<id>/` self-describing with an identity file written at creation. Have the sweep re-register a directory from that file instead of deleting it. Never let a sweep unlink a directory that contains `memory.db`. Only `forget` should do that.

### 2. [critical] The evidence schema policy (unlink and re-ingest) destroys LCM summaries, which the design placed in evidence because they cannot be rebuilt
**Location**: README §2a "Two merges" ("LCM summaries join evidence because summaries can be LLM-generated and are not rebuildable"); §7 ("evidence: unlink, re-ingest whatever host files still hold, write a loss receipt"); sketch `StorageClass::Evidence { on_schema_mismatch: Reingest, on_corrupt_file: ReingestAfterConfirm }`.
**Finding**: Any evidence schema change, and any confirmed evidence corruption, unlinks the file. That deletes every LLM-generated summary, and host files cannot re-derive them. It also deletes every observation older than the host's transcript retention. Claude Code's default `cleanupPeriodDays` is 30, so roughly 150 of the 180 days in the evidence window are gone. Evidence is the class with the most destructive schema rule, and the design moves the one non-rebuildable session artifact into it.
**Evidence**: The tradeoff section accepts "losing evidence older than the host's own transcript retention on an evidence reset". It does not count summaries, and it treats a schema bump as a reset. This project bumps session-side schemas often (LCM 15, temporal 8, projector v7; explanation §9). The design also says raw rows become evictable once a summary covers them. After a reset, the summaries are gone and so are the raw rows they replaced.
**Suggestion**: Give evidence (or at least summaries) the `Migrate` rule, the same as memory. Alternatively, put summaries in their own durable class with additive migrations. Keep `Reingest` only for confirmed file corruption, and only for the raw observation tables.

### 3. [critical] The journal reset policy deletes durable user state on every schema bump
**Location**: sketch `StorageClass::Journal { on_schema_mismatch: Reset, on_corrupt_file: ResetWithFinding }`; README §7 ("journal: reset, with typed `JournalReset`"); layout lists WorkflowDefinition, NativeIntegration and HandoffGrant in `journal.db`; `remote/<node>/journal.db` is the journal class.
**Finding**: `journal.db` mixes ephemeral execution state with things the user authored or approved:
- registered workflow definitions (`workflow_definition_source_journal`, `workflow_definition_disposition`; the `tracedecay_workflow_register_definition` tool);
- native-integration approvals (`approve_native_integration`);
- open handoff grants;
- for remote nodes, the unreplayed Remote Brain spool.

A routine schema change wipes all of them. That breaks Zack's durable class ("configurations etc other user state", synthesis Preserve 5) and the README's own label for the class, "authoritative". A journal reset also drops every `GraphHead`, which leaves sealed generations unreferenced in `worktrees/`. The design names no sweep for those files.
**Evidence**: Schema at `crates/tracedecay-rusqlite-runtime/src/workflow*` defines 22 Work, Workflow and handoff tables, and several of them hold definitions rather than runs.
**Suggestion**: Move definitions and approvals into a migrated class (catalog or memory-like), or give the journal additive migrations. Keep `Reset` for terminal and run state only. Add an unreferenced-generation sweep keyed off `GraphHead`.

### 4. [critical] `FnOnce` jobs cannot be retried, so the #4094 containment ladder and the fence retry cannot be built with the sketched types
**Location**: sketch `executor::WriteJob = Box<dyn FnOnce(&mut WriteTx)>`, `ReadJob = Box<dyn FnOnce(&ReadTx)>`; `LaneHandle::write/read<F: FnOnce ...>`; `WorkJournal::transition<F: FnOnce ...>`; `fault::Recovery::ReconnectAndRetry` ("retry the failed job once"); README §3 (in-flight reads "retried after the fence") and §5 row 3.
**Finding**: The closure has been consumed by the time the first run fails, so it cannot run again on a fresh connection. Batching makes this worse: "several jobs batch into one BEGIN IMMEDIATE". If job k hits SQLITE_CORRUPT, the whole transaction rolls back, and jobs 1..k-1, which did nothing wrong, cannot be replayed either. They can only fail.
**Evidence**: The design rests its incident closure for #4094 on "drop every connection of that file and retry the job once on a fresh connection; a pass means the fault was process-local". With `FnOnce`, the only possible outcome is to fail every job in the batch. The caller then sees `Unavailable`, which is exactly the user-visible outcome the ladder was meant to prevent.
**Suggestion**: Make jobs `Fn + Send + 'static` over owned or `Arc` inputs, and state the constraint as part of the contract: a job must be re-runnable and must have no effects outside its transaction. Alternatively, return a typed `RetrySafe` failure to the handle method and re-run there from its owned inputs. Specify what happens to batch-mates.

### 5. [critical] `transition` checks idempotency after `decide`, so a retried operation that already committed gets a refusal
**Location**: sketch `WorkJournal::transition` TODO, in this order: `let decision = decide(current.as_ref())?; if let Some(prior) = tx.idempotent_replay(&decision.operation_id)? { return Ok(prior) }`.
**Finding**: The operation id exists only inside the `Decision` that `decide` returns. On a replay, `decide` sees the post-commit state. For a cancel, that state is already Cancelled, and `request_cancellation` returns an error. The `?` returns that error before the ledger is consulted. There is also no caller-supplied operation id in the signature. If `decide` mints a fresh id, a replay never matches at all.
**Evidence**: The design also promises "Admitted writes run to commit ... even if the caller drops the future" and "settlement outlives the request" (#3711). The standard client behaviour after a dropped future is to retry. That retry now gets `Refused` for an operation that succeeded.
**Suggestion**: Use `transition(key, operation_id, decide)`, look up the ledger by `operation_id` before loading or deciding, and return the stored Outcome on a hit.

### 6. [critical] Cutover order: step 2 assumes an invariant that only steps 3 and 6 establish
**Location**: README "Cutover plan" steps 1–3 and 6; "Rejected: The judge's cutover order" ("The executor's fence is only total once every read is a bounded job").
**Finding**: Step 1 makes only the typed `dispatch_read` async, with 7 production callers (verified: `remote_query.rs:137`, `external_source_store.rs` ×4, `fact_store/runtime.rs:257`, `observation_adapter.rs:1800`). After step 1, three things still sit outside the executor:
- Sync exact-SQL reads and interactive transactions, deleted only in step 3. Interactive transactions run in `tracedecay-rusqlite-runtime`: 49 `begin_immediate` sites across `work*`, `workflow*`, `handoff.rs`, `remote/*` ×16, `repository/graph_publication/support.rs` ×5 and `repository/scope_set.rs`. Explanation §3: "runs the whole interactive transaction on the writer thread ... 30 s idle and 120 s total".
- `sqlite_read_snapshot`, deleted only in step 6. `storage_report.rs:449-458` and `orphan_stores/collection.rs:788` open foreign connections and run an online backup with `SnapshotReadControl::unlimited()`.

On a shared pool, an interactive transaction pins a pool thread across caller round trips for up to 120 s, so six concurrent Work transactions starve the whole profile. The snapshot readers are invisible to the fence. TRUNCATE gets BUSY, the lane takes the "another process" path, and past 512 MiB of WAL it refuses writes. A storage report on a large store would therefore cause write `Busy`. That is a regression from today, where PASSIVE at least makes progress. Step 2's proof ("continuous readers keep the WAL at or under soft plus one batch") cannot pass while these readers exist. Its "#4097 closes as superseded" is premature for the same reason.
**Evidence**: The README's own rationale for putting reads first argues for this order: 1 → 3 → (delete or route `sqlite_read_snapshot`) → 2. Step 3 can run closure jobs on today's writer thread, which already executes closures. Once that lands, step 2 swaps the thread model under callers that are all job-shaped, and it needs no compatibility path for interactive transactions.
**Suggestion**: Reorder to 1, 3, then a read-snapshot removal step, then 2. Separately, measurement, worktree-root GC, the response-handle sweep and the `hook_analytics.jsonl` rotation depend on none of the executor work. Today they wait for step 7, which leaves the 153 GB incident class open for the whole cutover. Ship them first.

### 7. [critical] Steps 3–5 change schemas of files still under the create-only/ResetRequired regime, before the memory importer exists
**Location**: Cutover step 4 ("memory graph ... move to SQLite tables"), steps 3 and 5 (journal tables out of `sessions.db`), step 6 (importer); README §7 ("memory and catalog: forward-only, additive, in-crate migrations").
**Finding**: Before step 6 there is no `memory.db`. The memory graph tables that step 4 adds must land in today's `projects/<id>/tracedecay.db` or `user-memory.db`. Both are "create-only at `SCHEMA_VERSION = 39` ... Any other stamp or shape is `ResetRequired`" (explanation §9). As planned, every beta install loses its `memory_v2_*` facts at step 4, two steps before the importer that the README says exists because "durable memory survives forbids a reset of that class."
**Evidence**: The upgrade path ("from step 6 onward `init` also imports memory") covers only post-step-6 layouts. Steps 2–5 have no stated memory-preservation rule.
**Suggestion**: Land the memory class rule (additive migrations on the existing memory tables) as step 0. Alternatively, ship `memory.db` and the importer before any step that touches a file holding `memory_v2_*`.

### 8. [warning] Seven single-aggregate `transition`s cannot express today's multi-entity Work transactions
**Location**: sketch `JournalAggregate` sealed for 7 types; `transition` decide signature `FnOnce(Option<&A>)`; README §4 and §8 ("77 methods to 3").
**Finding**: Work spans 22 tables. One transaction often reads and writes several entities and checks invariants across rows. Two examples:
- `work_run_control.rs` reads `work_attempts_v1` and writes `work_run_controls_v1`, `work_blocked_intervals_v1` and `work_blocked_interval_observation_cursors_v1` in one transaction.
- `work_placement.rs:92-95` enforces "no other admitted/quarantined placement targets this `target_root` in the repository", a uniqueness check over a set of rows.

None of the 7 aggregates is a placement, run control, blocked interval, adjudication or product-graph version, and `decide` sees exactly one aggregate. The fallback ("decide outside, pass expected_version") versions only the written aggregate, not the rows read, so the placement check becomes a race. Separately, `decide` returns `StorageError`, so domain refusals such as "already terminal" collapse into `Refused(InvalidInput(String))`. That is a stringly typed failure, which AGENTS.md's typed-failure rule forbids.
**Suggestion**: Make the `decide` input a typed read set: `decide(&JournalView)`, where the view can load or list any aggregate inside the same write transaction. Add the missing aggregates, or name one `WorkRun` aggregate that owns attempts, controls, intervals and placements. Return `Result<Result<Outcome, A::Rejection>, StorageError>`. Step 3 should list each of the 24 ports with its target aggregate before it starts.

### 9. [warning] Root-commit identity: a sticky cache, re-keys that strand memory, cross-profile markers and subdirectory projects
**Location**: README §2; sketch `identity::IdentityAnchor`, `Marker` docs, `resolve` TODO; `Checkout::discover`.
**Findings**:
- **Sticky and divergent.** `resolve` uses a cached anchor before computing one, so the marker is the authority in practice. The sketch even says so: "if it caches an anchor -> use it". With several roots (subtree merges), the smallest root depends on what HEAD reached at first resolve. A clone that first resolved before a merge keeps the old root, and a fresh clone gets the new one. The sketch also contradicts itself on how the root is found: `RootCommit` is the "smallest parentless commit reachable from HEAD", which needs a walk through all parents, while the `Marker` doc says recomputation is "one gix first-parent walk". A first-parent walk finds a different root.
- **Re-keys strand memory.** Shallow → unshallowed, unborn → first commit, and history rewrites all mint a new `repo_id`. The old set's `memory.db` is kept forever by policy but never read again. That is semantic loss plus an uncollectable leak. Agents shallow-clone often.
- **Cross-profile markers.** `<git_common_dir>/tracedecay-project.json` is shared across profiles (tests, Bazel, `TRACEDECAY_DATA_DIR`). Today's `is_initialized_project_root` explicitly ignores a marker whose shard this profile does not hold (`config.rs:270-273`). The design writes explicit overrides into that marker, and `forget` clears its id, so one profile's `link --split` or `forget` changes another profile's identity.
- **Subdirectory projects.** `discover_project_root` (`config.rs:246-265`) stops at the first *initialized* directory below the worktree root, so monorepo subprojects are supported today. `Checkout::discover` walks to the worktree root. Subprojects merge into one identity and one code-index scope. "What this supersedes" does not list this change.
- **Non-git routing.** §2 sends non-repository transcripts to `_profile`, while `IdentityAnchor::Path` gives every non-git cwd its own repository. Every cwd under `~/Downloads/*` becomes a store set whose "live checkout" never dies.

**Suggestion**: Re-key by moving the old set into the new id, or record an alias (a catalog alias row) rather than minting a new set. Recompute and cross-check the anchor on a timer. Scope marker overrides by `profile_id`. Decide one rule for non-git cwds. Either keep initialized-subdirectory roots or list their removal as superseded behaviour.

### 10. [warning] The profile budget counts code generations that it cannot evict, so it deletes irrecoverable evidence to pay for rebuildable bytes
**Location**: README §6 (64 GiB profile budget, "memory excluded"; eviction order); sketch `retention::plan` steps 5–7; the code-generations row (active plus 2).
**Finding**: Live worktrees' sealed generations and code-index generations count toward the profile budget. The planner's only lever while those worktrees are live is evidence and derived eviction from the least recently used repositories. Code-index generation files are "routinely ~1 GiB each" (explanation §7). This host had 32 linked worktrees, so 32 × 3 × ~1 GiB plus sealed Grafeo copies is already more than 64 GiB. The planner would then evict every repository's transcript evidence, which cannot be re-ingested past host retention, while keeping bytes that can be regenerated from source.
**Suggestion**: Give code its own budget with its own lever: evict non-active generations, then idle worktrees' heads. Excluding rebuildable classes from the evidence planner would also work. Evidence eviction should respond only to evidence pressure.

### 11. [warning] Catalog and pool coupling: a FULL fsync on every request, two retention rules in one file, and cross-repository head-of-line blocking
**Location**: README §2a ("Every file has exactly one retention rule"; catalog row: config kept, events 30 d, usage 90 d); Usage (`storage.repository(...) // opens no file`); sketch `Storage::repository` ("Registers or refreshes the checkout row in the catalog (one small write)"); §4 (one reserved foreground thread, pool of 6).
**Finding**: Every request with a cwd performs a FULL-durability catalog write. The Usage comment "opens no file" is wrong, since the catalog lane opens. The catalog holds identity, configuration and per-hook telemetry at FULL, so it contradicts both "one retention rule per file" and "durability per class". It is also a profile-wide single point of failure: if the catalog lane is unavailable, every `repository()` call fails across all repositories. That is the same "one failure domain" argument the design uses to reject a profile `activity.db`.

The shared pool also creates a new cross-repository coupling. Today an fsync stall on one file parks only that file's writer thread. With 6 threads and one foreground reservation, a few stalled fsyncs park every repository's writes. This host records /fast fsync stalls of 5–31 s. The claim "Other repositories and other classes never see any of these faults" holds for correctness, not for latency.
**Suggestion**: Move hook events and usage history into a separate NORMAL telemetry file. Upsert checkouts only on a change or a coarse timer, cached in memory. Give a lane stuck in fsync a bounded hold on its thread, or run fsync-heavy commits outside the fair pool. Measure p99 under an injected fsync stall in step 2.

### 12. [warning] Dead-lettering transient faults after one hour turns outages into permanent gaps
**Location**: sketch `sessions::projection::MAX_PARKED` and `dispose` ("Transient if parked_for >= MAX_PARKED => DeadLetter"); README §5 row 1.
**Finding**: The `Transient` cases ("Lane busy/unavailable") include derived `Unavailable{Recovering|Rebuilding}` and SQLITE_FULL. A derived rebuild of a large repository took 6 hours on 2026-07-23. During any outage longer than an hour, every stream dead-letters one head per hour. The policy cannot even write those dead letters, because they live in the unavailable `derived.db`. The gaps re-queue only on a projector version change or a daemon restart. The 6-day freeze came from a misclassified fault, not from a real transient one. The connection-first ladder already fixes misclassification.
**Suggestion**: Transient faults park without limit and raise a finding. Only `Deterministic` faults dead-letter. Re-queue dead letters on lane recovery, not only per incarnation.

### 13. [warning] `quick_check(1)` scans the whole file, so the poison-versus-corrupt verdict costs O(file size) per suspect row
**Location**: README §5 rows 4–5; synthesis graft "the confirm ladder".
**Finding**: N in `PRAGMA quick_check(N)` caps the number of errors reported, not the work done. The check scans the entire database. On an 8 GiB evidence file, or today's 63 GB `sessions.db`, it runs for minutes. That is far past the 5 s read deadline that would interrupt it, and it holds one of 6 pool threads while it runs. A stream of SQLITE_CORRUPT-classified poison rows would run it repeatedly.
**Suggestion**: Run quick_check as a background maintenance job without a deadline, at most once per file per window. Hold the suspect job, or refuse it typed, until the verdict arrives. Cache the verdict by file generation.

### 14. [warning] The beta upgrade path orphans profile-level legacy files, and the importer's merge semantics are undefined
**Location**: README §7 "Upgrade path"; step 6; §6 item 1 (sweep covers `repos/*` only).
**Findings**:
- **Profile-level files.** The importer "deletes the old directory", meaning `projects/<id>/`. Nothing names the profile-level legacy files for deletion: `user-sessions.db` (45.7 GB live), `global.db` (2.58 GB), `user-memory.db`, `*.grafeo` and their WAL directories, `lcm-payloads/`, `hook_analytics.jsonl` and `maintenance/`. The usage sampler measures only known classes, so these bytes become unmeasured and uncollected. That is a fresh instance of #4099.
- **Merge semantics.** The 14 clones' old `projects/<proj_id>/tracedecay.db` stores map to one `repo_id`. The importer must merge 14 independent `memory_v2_*` histories (supersession chains, banks, receipts, `*_no_delete` triggers) into one `memory.db`, and the design gives no merge rule.
- **Missed imports.** The import runs in per-checkout `init`. Old directories whose checkouts are gone are never imported or deleted. A user who skips two releases loses memory silently.

**Suggestion**: Run the importer at daemon start over every `projects/*` and the profile-level files. Define the merge as a union by fact id with supersession preserved. Delete every legacy path by explicit list after a successful import. Report un-importable stores as findings, not silence.

### 15. [warning] The idempotency ledger's location is ambiguous, and the answer decides atomicity
**Location**: README §2a layout (journal holds "idempotency ledger"); §6 item 9 ("pruned at 7 d of age in the journal"); step 2 ("keeping FairQueue, batching and the ledger").
**Finding**: Today the writer writes a `td_runtime_writer_idempotency_v2` row in each batch of each file (explanation §3). If the ledger lives only in `journal.db`, memory, evidence, derived and catalog writes cannot record it in the same transaction, and replay deduplication for a `MemoryStore::commit` after a crash is lost. If it stays per file, the layout table and §6.9 are wrong, and retention must prune it in every class.
**Suggestion**: State that the ledger is per file, written in the same transaction, and pruned by age in every class.

### 16. [warning] The new `RepositoryId` and `WorktreeId` duplicate existing domain types and drop the privacy salt; the fate of `ProjectId` is unspecified
**Location**: sketch `pub struct RepositoryId(String)`, `pub struct WorktreeId(String)`; README "`repo_id` is `repo_` plus 16 hex of sha256 of the identity anchor".
**Finding**: `tracedecay_domain::RepositoryId` and `WorktreeId` already exist (re-exported at `tracedecay-domain/src/lib.rs:330,341`). Work rows, remote protocol and provenance use them. `tracedecay-sessions/src/repository_provenance.rs:134-146` derives them as salted opaque ids (`derive_project_privacy_domain_salt`) from the common directory. The sketch adds a third identity with the same names, which violates AGENTS.md ("do not hand-write duplicate DTOs", "no parallel authorities"). It is unsalted: anyone who knows a public repository can compute its `repo_id` from the root commit. `ProjectId` appears in 657 files, including the MCP `project_selector.project_id` wire surface. The README deletes `projects/<id>/` resolution but never says whether `ProjectId` is replaced, aliased or kept.
**Suggestion**: Redefine the existing domain types instead of adding new ones. Keep the privacy-domain salt (salt the anchor hash). Add a cutover line for `ProjectId` on the wire.

### 17. [warning] Cross-repository data has no home
**Location**: README §2 and §2a (every class is per repository, `_profile` refuses `work()`).
**Finding**: `authorized_scope_sets_v1` (`repository/scope_set.rs`, the multi-root scope-set CAS tools) and multi-root execution span repositories by definition. Neither appears in the layout, the 7 aggregates or the class table. Sessions that touch several repositories ("a session may touch zero, one, or many projects", synthesis Q1) get no routing rule beyond "cwd".
**Suggestion**: Add scope sets to the catalog, or to a profile journal, and state the session routing rule.

### 18. [warning] Fence claims do not hold as written, and long searches have no bounded form
**Location**: README §3 ("The WAL peaks at the soft limit plus one batch"; reads "interrupted at their deadline ... and retried after the fence"); step 2 proof; `SessionStore::search(SessionQuery::text(..).limit(20))`.
**Findings**:
- **Writes continue during the fence.** The fence queues only reads, so writes continue through the drain window (up to 5 s for derived). The peak is soft plus 5 s of bulk writes, not soft plus one batch, and the step-2 assertion fails as specified.
- **Retrying a timed-out read is pointless.** The fence interrupts only at the deadline, so it never shortens a read. A read that hit its deadline will hit it again on retry and re-arm the fence.
- **FTS ranking cannot be paged.** Ranked FTS (bm25 over a large corpus) cannot use keyset paging, so LCM search over a big derived file returns `TimedOut`. The usage example has no paging, and the tradeoffs do not name this regression.

**Suggestion**: Either pause write admission at the fence, or restate the bound and the test. Do not retry deadline-exceeded reads; return `TimedOut`. Specify a bounded search shape: a time-boxed top-k with a `partial` flag, as already exists for rebuilds.

### 19. [warning] Contentless FTS5 cascade deletes and cross-file hydration race eviction
**Location**: README §2a ("LCM index (contentless FTS5)"), §6 ("the derived cascade deletes rows of the evicted sequences"); sketch `SessionStore::search` ("bodies hydrate from evidence in the same job pair").
**Finding**: Deleting a row from a contentless FTS5 table requires either the original column values or `contentless_delete=1`, which needs SQLite 3.43 or later. The cascade runs after evidence has already evicted the text, so without that option the derived delete cannot run and FTS keeps dangling postings. Search also runs as two jobs on two snapshots. Hits whose evidence was evicted in between must be a typed absence, not an error.
**Suggestion**: Require `contentless_delete=1` in the derived schema. Define the hydration-miss behaviour: drop the hit and count it.

### 20. [nit] Leftovers from judge fixes and type details
**Location**: sketch `OnCorruptFile::ResetWithFinding` ("Rename aside (debris, collected after 7 d)"); README Usage `ProjectionBudget::page(256)`; sketch `IdentityAnchor::RootCommit([u8; 20])`.
**Finding**: The sketch still keeps a 7-day renamed copy of the journal. The README says "recreate empty", and AGENTS.md "No secondary copies" forbids the copy. The judge asked for the batch size to be internal, but it is still on the public surface. `[u8; 20]` assumes SHA-1, while git's sha256 object format produces 32-byte ids.
**Suggestion**: Make the sketch match the README, use an internal projection default, and store `gix::ObjectId` for the root commit.
