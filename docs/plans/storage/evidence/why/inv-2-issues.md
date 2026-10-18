# Investigator 2: GitHub Issues (ScriptedAlchemy/tracedecay)

## Source

Issue / ticket tracker: GitHub Issues on ScriptedAlchemy/tracedecay, read through `gh issue view --json ... comments` (falling back to `gh pr view` for numbers that are PRs). The Linear playbook was adapted: issue bodies stand in for ticket descriptions, comments stand in for decision threads, and "Closes/Fixed by" comments stand in for linked PRs. I used 60 gh calls. Raw dumps are in `scratchpad/storage-arch/why/inv2/i<N>.txt`. The full search hit list is in `inv2/uniq.txt`, with 366 unique issues.

Authorship caveat: almost every issue and comment authored as `ScriptedAlchemy` on this repository is written in agent voice ("Fleet lane ...", "claimed by fleet slug ...", "Written by Devin"). Some are explicitly agents (devin-ai-integration). Treat `ScriptedAlchemy` text as an operator account that agents post through, not as Zack's verbatim words, unless the text says so. External reporters (HashemKhalifa, BleedingDev, m4tty-d, spa5k, JonWolfeDrata) are real users.

## What I Searched

Seed items opened in full, with all comments: #3402, #3409 (PR), #4085, #4099, #4100 (PR).

`gh issue list --state all --limit 60 --search "<q>"` for each of these queries:
`storage in:title`, `sqlite`, `WAL`, `checkpoint`, `wal_checkpoint`, `store growth`, `disk`, `GB in:body`, `projection drain`, `corrupt`, `worktree store`, `per-worktree`, `registered root`, `grafeo`, `staging container`, `writer actor`, `retention`, `turso`, `shard`, `common-dir worktree`, `asupersync`, `host admission`.

Opened in full from the hits: #477, #470, #480, #2402, #912, #3560, #730, #2398 (worktree and store identity); #4046, #3711 (PR), #3736, #883, #4007, #3578 (ports, writer, runtime); #3984, #4094, #4096, #2401, #798, #2521 (drain, growth, WAL); #3491 (Turso); #763 (Grafeo); #1074 (store-runtime ownership); #2138, #1218 (retention and backups).

These were seen in the hit lists by title only and not opened: #2097, #2363, #2400, #2106, #2050, #2611/#2647/#2641, #1979, #1982, #2877, #3760, #3838, #4095, #3586, #3571, #2469, #1079, #1090/#1094, #874, #1247, #2012, #2962, #799, #836, #762, #764, #765.

## Direct Evidence Found

### Q5: Why the Turso/libSQL cutover was abandoned

- **What it says**: "Closed without merging. The native Turso cutover was abandoned on 2026-10-12: Turso 0.8.0 is not mature enough to be TraceDecay's only store (a silent wrong-result bug and about ten planner gaps surfaced during the burn-down, and the concurrent-write motivation was never realized). The repository keeps its SQLite runtime. Branches are deleted."
- **Where**: Closing comment on #3402 (2026-10-12T00:32:42Z) and an identical comment on PR #3409 (00:32:40Z). https://github.com/ScriptedAlchemy/tracedecay/issues/3402
- **Author**: the ScriptedAlchemy account. The wording is agent-style. The user's original ruling is not in the tracker.
- **Relevance**: Gives the stated reasons: engine maturity (a silent wrong-result bug and planner gaps), plus the motivation that drove the cutover: concurrent writes, "never realized".

- **What it says**: #3402 body: "The user approved fresh native databases, disposal of old contents without conversion, native FTS token/ranking differences, and logical deletion without SQLite deleted-page scrubbing." The body also says "Native ordinary facade connections do not reproduce production ExactSql command ordering" and "Power-loss durability remains unproved."
- **Where**: #3402 body, created 2026-10-10.
- **Relevance**: An agent's summary of user rulings that defined the cutover's scope. Per the epistemics guide, this is a paraphrase. The original user words are not here.

- **What it says**: #3409 design section: "An in-place driver swap, chosen by a three-candidate arena. The writer actor, admission, receipt ledger, payload executors, exact SQL and the domain stores keep their code; only their driver calls moved to the new `tracedecay-sql` crate." Its status list includes "Delete ATTACH (U1); backups, export, snapshot copies and vacuum (U1b)". It also says "Owned registered stores fsync every commit on native (`SyncMode::Full`)".
- **Where**: PR #3409 body, 2026-10-11.
- **Relevance**: The cutover deliberately kept the writer-actor, admission, and receipt architecture and swapped only the engine. It also planned to delete ATTACH, backups, and vacuum, so those surfaces exist on SQLite master today.

- **What it says**: "Native Turso lexical ingest and search run 1.5-3.5x slower than SQLite": ingest median 15.0 s on SQLite against 28.7 s on Turso. "Native finalization merges sort in memory, because the driver forces in-memory temp storage". Closing comment: "Turso-specific; the native cutover was abandoned on 2026-10-12."
- **Where**: #3491 (2026-10-11, closed 2026-10-12).
- **Relevance**: A measured performance regression that was open at the time of abandonment. #3409 lists it as "PERF3 ... from ~2x to 1.38x so far, target 1.2x".

### Q1: Store unit, worktrees, and registered roots

- **What it says**: The tip binary showed "No path-hashed `proj_<worktree-path-hash>` directory was created; the profile's `projects/` contains only the canonical id. `tracedecay projects list` shows one project, one store." The comment continues: `hook_analytics_path` "resolves the enrolled layout through the shared repository identity (`tracedecay-project.json` + git common dir, which linked worktrees share) ... A hook can no longer be the thing that decides a directory is a project."
- **Where**: #470, closing comment, ScriptedAlchemy 2026-08-31.
- **Relevance**: Shows design intent on the #707 V2 line: linked worktrees resolve to one canonical project id through repository identity. This contradicts the observed 30+ per-root stores in #4099 (see Contradictions).

- **What it says**: "Linked worktrees share one project graph database, but each code scope projects into its own namespace. `CanonicalCodeGraphStoreLeaseV1` says 'the namespace remains exact to the requested repository/worktree/ref or snapshot scope'." Option 2, "One content namespace shared by identical scopes", was rejected: "This conflicts with the per-scope namespace invariant and with exact worktree snapshot authority, so I don't recommend it." The lane asked: "may the canonical relation-row digest ... change as in option 1?"
- **Where**: #2402 comment, "Fleet lane close-all-issues/graph-layers", 2026-10-01. Devin then picked up "the foreign-base layer shape" on 2026-10-04. The issue closed 2026-10-05 with no closing comment.
- **Relevance**: The most direct statement in the tracker that worktree sharing by repository identity was designed: one graph DB per repository, with namespaces per worktree scope. It also records that a linked worktree at an identical tree duplicated the sealed graph (+25 MB, "about 1.8 GB per linked worktree" on this repository).

- **What it says**: "`f347a0a46` (fix(index): require opt-in for linked worktree scopes) gates project-open code-index activation for a linked worktree behind `sync.watch_linked_worktrees` (default false)."
- **Where**: #912 body, 2026-09-06.
- **Relevance**: Linked-worktree indexing is opt-in, and off by default. #2398 shows the cost when it is on: a 6 GiB cgroup daemon was OOM-killed when "a second worktree of a large repository mounts".

- **What it says**: "`projects/` with more than 30 `proj_*` stores, one per registered root, so each agent worktree under `/fast/tmp` that ran TraceDecay carried its own full code-graph index and sessions shard".
- **Where**: #4099 body, 2026-10-16.
- **Relevance**: The observed state in the question. The issue does not say whether the `/fast/tmp` worktrees were `git worktree add` worktrees or separate clones.

- **What it says**: The user's expectation was "Worktree shards should be treated as their own distinct projects (their manifests already point at the worktree paths)", and "Repos driven by parallel agent workflows routinely have many live git worktrees."
- **Where**: #477 (external user HashemKhalifa, 2026-07-15, v0.0.66, pre-V2). Closed 2026-07-27 with no comment.
- **Relevance**: The pre-V2 layout had one profile shard per worktree root. The user wanted that kept distinct, which is the opposite of the later #470 resolution. The 0.0.x layout was `~/.tracedecay/projects/<project_id>/store_manifest.json`.

- **What it says**: `tracedecay projects forget <selector> --yes [--keep-store] [--dry-run]` "Removes exactly one project: its store directories under the profile root ... and its registry rows in one transaction".
- **Where**: #730 closing comment, 2026-08-31.
- **Relevance**: A supported per-project removal exists, but only manually. Nothing in that issue makes it automatic for deleted roots. #4099 later asks for GC of "project stores whose registered root no longer exists on disk".

### Q3 and Q1: Why a daemon-owned single writer (incident origin)

- **What it says**: "A single project store ... has corrupted and required manual `.recover` **three times in six days** ... The strong correlate is **multiple concurrent `tracedecay serve` writer processes** against the same shared store". The follow-up comment adds: "when that socket is **missing, every one of those paths silently falls back to opening `tracedecay.db` directly.**"
- **Where**: #480 (m4tty-d, 2026-07-18; follow-up 2026-07-22). Another user commented: "This has bitten me constantly since starting to use this tool" (JonWolfeDrata, 2026-07-23).
- **Relevance**: This is the incident behind single-writer ownership.

- **What it says**: "the silent fallback is gone; the v2 line is fail-closed ... `tracedecay tool …` refuses typed ... instead of opening the store directly ... Store writes are daemon-owned single-writer." The comment also says "mount-time quarantine ... reopens fresh so the replay journal re-projects every generation", and "The v0.0.66 store layout this was filed against (direct-write `tracedecay.db` + libsql sync) no longer exists on this line".
- **Where**: #480 closing comment, ScriptedAlchemy, 2026-08-31.
- **Relevance**: Links fail-closed daemon-only access and the single writer to a real corruption incident. It also shows that the pre-V2 store used libsql, so the 2026-10 Turso attempt was not the first time off plain SQLite.

- **What it says**: Invariants: "Preserve bounded queue admission, typed saturation/unavailability, attached write authority, statement validation, and the single serialized writer." The issue also says "The writer remains the only connection owner", "Do not create separate sync and async SQL engines", and "Keep genuinely synchronous callers supported through a narrow adapter to that same implementation where needed." Resolution: "Async writers now await the existing actor acknowledgement directly ... concurrent active blocking workers 9–17 before → 0 after".
- **Where**: #883 (architecture review of #707, 2026-09-05; integrated as `b501f5198` 2026-09-06).
- **Relevance**: States the writer-actor invariant, and the rule that sync and async paths share one engine with sync callers going through an adapter. It does not say why an actor was chosen originally.

### Q2: Sync Work/Workflow ports beside async store ports

- **What it says**: "Daemon-service work and workflow request handling then call synchronous Work/Workflow storage ports that wait on the SQLite writer in place, which parks a native worker." It counts "591 hits in daemon_suite. Every one is a synchronous Work or Workflow storage port reached from a broker-socket connection". Done-when: "every such call site in daemon-service runs on `native_runtime::spawn_blocking` (or the port becomes async over the writer)".
- **Where**: #4046 (2026-10-15, closed same day).
- **Relevance**: Shows the sync ports are a known hazard. The chosen remedy was off-thread execution, not making the ports async. #3736 (2026-10-16 comment): "Work attempts and fan-out recovery ... run on an abortable off-native-thread task."

- **What it says**: "Each wait parks a native worker for the whole read ... Wrapping each caller in spawn_blocking does not scale to this count." Proposed design: "route `dispatch_read` and the sync read paths of `ExactSqlHandle` through the async `Connection` in `tracedecay-runtime-core`, which already sends reads to the blocking pool, or make the Work/Workflow storage ports async."
- **Where**: #4085 (open, 2026-10-16). Counts: mcp_suite 2,683, unit_test 1,332, daemon_suite 131, session_suite 4. The biggest caller is `RuntimeExternalSourceStore::read_pending_projection` (3,318).
- **Relevance**: The open design question for Q2. It names both options and does not decide between them.

- **What it says**: "Storage settlement must outlive the request that started it. A cancelled MCP or HTTP request must not truncate an admitted write, a shard close, or a fact-store commit. This PR moves that settlement onto an Asupersync runtime, whose task ownership expresses it directly." The PR also says "The SQLite writer uses native channels and keeps its owned writer thread", and that `recv_writer_reply` kept a `block_in_place` guard so that "Synchronous storage ports called from Tokio workers no longer starve the daemon's runtime."
- **Where**: PR #3711 body (merged 2026-10-13).
- **Relevance**: The direct statement of why the native runtime was adopted, and of how sync ports were tolerated during the move.

### Q3: Shard and store-runtime layering

- **What it says**: "The shard registry could not move: `db::Database` is a client lease on a `ShardRuntime`, and global-db / sessions / session-memory / application consume the registry directly, so lifting it would invert the dependency direction. It is renamed `tracedecay_runtime_core::shard_runtime` and documented as the per-shard SQLite runtime the store runtime drives". The comment also says "`StoreRuntimeRegistry` and `DaemonSessionRuntimeRegistryV1` are layered, not parallel."
- **Where**: #1074 closing comment, 2026-09-08. The body says the split came from "an incomplete extraction ... That history should not become a permanent V2 architecture."
- **Relevance**: Explains why the shard runtime sits in runtime-core while store-runtime owns lifecycle. It does not explain why sharding exists.

- **What it says**: "the writer-presence guard, the queued-work guard, `record_storage_usage` and `set_health` are only called from tests. The shard fields they set never change in production."
- **Where**: #3578, 2026-10-12.
- **Relevance**: Shard bookkeeping had test-only state, which was removed.

### Q6: Fail-closed drain, growth, and retention

- **What it says**: "The corrupt cache then fails every projection of one queue head with `database disk image is malformed`, and the projection drain stops. Nothing has been projected into `lcm_raw_messages` for `/fast/projects/tracedecay` since 2026-10-10". The writer's TEMP cache uses `SQLITE_TEMP_STORE=2` and lives in the writer process heap. There were 1,617 drain failures from Oct 8 to Oct 16. "Each corrupt queue head belongs to exactly one daemon PID and clears when that PID exits". The cause is unknown; the hypotheses include "The writer connection being used from two threads at once. It is a `NO_MUTEX` connection."
- **Where**: #4094 (open, 2026-10-16).
- **Relevance**: The concrete fail-closed failure. The drain is strictly ordered: one bad head blocks everything behind it.

- **What it says**: "retries Git-evidence attribution **forever, with no backoff or retry cap**" … "Session stores had grown to 18 GB total (`user-sessions.db` 10.8 GB + project `sessions.db` 6.6 GB) ... every retry writes". Fix: a dead target "settles as unavailable on the first pass ... A Git failure in a repository that still exists retries at most 5 passes".
- **Where**: #3984 (external user spa5k, 2026-10-15; fixed in #4011).
- **Relevance**: An unbounded-retry drain incident tied to deleted worktrees. It shows deleted agent worktrees as a source of both growth and retries.

- **What it says**: "table payload growth measurement is unavailable" was reported for both stores. Each of the >30 `proj_*` stores carried "its own full code-graph index and sessions shard". There was a "229 MB WAL whose checkpoint stayed pending through 245,264 retry wakes". The needed work includes "Every GC goes through the daemon's own maintenance path, never a direct file delete."
- **Where**: #4099 (open, 2026-10-16).

- **What it says**: The doctor "never measures stores" over 64 MiB (`MAX_SYNCHRONOUS_TABLE_GROWTH_STORE_BYTES`). The measured path "discards every query error ... including SQLITE_CORRUPT ... and reports it as the same `Unknown`."
- **Where**: #4096 (open, 2026-10-16).
- **Relevance**: The direct mechanism behind the 153 GB going unreported.

- **What it says**: "`stores[].total_bytes` counts only the project SQLite family ... The one 'store' row covers 1 to 3% of the profile." The issue also says "The retention backlog is never available in production".
- **Where**: #2401 (2026-09-28, closed 2026-09-29).
- **Relevance**: An earlier finding of the same size-observability gap, three weeks before #4099.

- **What it says**: "The operator rule is 'never keep backups and old profiles' (AGENTS.md, *No secondary copies*)." The issue also says "LCM payload GC apply (`RegisteredGlobalDb::lcm_run_payload_gc_apply`) has no production caller. `docs/LCM-PAYLOAD-GC.md` describes a daemon scheduler (`gc_enabled`, `interval_seconds` = 6 h), but no code wires it".
- **Where**: #2138 (2026-09-26, closed the same day with no comment and no recorded decision).
- **Relevance**: Direct evidence that LCM payload GC existed only on paper as of 2026-09-26, and that the "no secondary copies" rule shaped storage surfaces.

- **What it says**: "nothing ever deletes a `segment-<sha256>.json` ... the store grows without bound across generations". Required shape: "key it on *unreferenced by every retained `generation_index` entry* under the store write lock ... otherwise ... the fail-closed read path would turn a healthy retained generation into `ResetRequired`." Reclamation "must run as bounded background maintenance ... never on admission or the query path".
- **Where**: #1218 (2026-09-11; closed by #1296 on 2026-09-15).
- **Relevance**: States the retention design principles: reference-based reclamation, maintenance-path only, and a journaled, idempotent sweep.

- **What it says**: `wal_bytes=780,299,192` on `proj_a5b3d7e3ebe14ca7` against a hard limit of `268,435,456`. Root cause: "`CheckpointPressure::BlockGeneral` ... only production consumer is the reader pool ... not product-write admission", and "The writer does not own the snapshot lease registry". The fix (#829) gates writes so the WAL cannot pass the hard limit silently. "No WAL limit was raised or overridden."
- **Where**: #798 (2026-09-03).
- **Relevance**: Shows the WAL soft and hard budgets (32/256 MiB) as a deliberate contract. The same shard id shows a pending checkpoint again in #4099.

- **What it says**: 2.04 TB of `read(2)` in 3 h. The reads hit `sessions.db` (16.4 GB) and `user-sessions.db` (14 GB), with temp B-trees in `/var/tmp/etilqs_*`. Resolution: "streamed_message_ingest_reads_grow_only_with_index_depth passed across an 8x store".
- **Where**: #2521 (2026-09-28 to 2026-10-06).
- **Relevance**: Session stores had reached 14–16 GB each by late September. The fixes addressed read amplification, not size.

### Q4: Grafeo

- **What it says**: "`tracedecay.grafeo` (12.4 GB) with a permanent CRC fault" after "a SIGKILL during a WAL write". The fix quarantines the container family "then reopens the vacant path fresh so the relational replay journal re-projects every generation". The forensic copies are "never deleted".
- **Where**: #763 (2026-08-30, closed 2026-08-31).
- **Relevance**: The Grafeo store is treated as a re-projectable derivative of a relational replay journal. Quarantined `.corrupt-<micros>` directories are kept forever ("never collected as loose debris"), which adds to growth. No issue states why Grafeo was chosen over SQLite.

### Context: frozen 24 h grant (#4100)

- **What it says**: "`GRANT_HORIZON` is 24 hours ... computes `grant_expires_at = observed_at + horizon` once, when the project opens." The memory curator then "retried on every 60 s tick for 28 hours (1568 attempts, no ledger rows)."
- **Where**: PR #4100 (open, 2026-10-16).
- **Relevance**: A retained-store surface that wedges and retries without bound. It is not storage-engine design.

## Indirect / Circumstantial Evidence

- **Concurrent writes motivated Turso.** #3402's close says "the concurrent-write motivation was never realized", and #3409's title is about cutting registered stores over. Suggested reading: the single serialized writer was felt to be a bottleneck, and Turso MVCC was the intended fix. Alternative reading: concurrency was one motivation among others. The #3402 body lists FTS and secure-delete differences, which suggests broader engine goals. I found no issue stating the original Turso motivation in full; it would be in #3403 or `benchmark_data/storage-concurrency/*.md` on the deleted branch.
- **Pre-V2 per-worktree shards.** #477 shows the 0.0.x layout minted one shard per worktree root. #470 shows hooks minting path-hashed `proj_*` directories for worktrees. #470's resolution says V2 collapses linked worktrees onto one canonical id. Suggested reading: per-root stores are a legacy behavior that V2 meant to remove for linked worktrees. Alternative reading: the `/fast/tmp` worktrees in #4099 are independent clones or have no `.git/tracedecay-project.json`, so each registers as its own project by design. The tracker does not show which.
- **Store growth is a recurring defect class, not a new one.** #1218 (segments unbounded), #2400 (superseded generations pile up), #2401 (report blind to graph and text), #2138 (LCM GC has no caller), #4096 (doctor gate), and #4099 (153 GB). Each was fixed locally. No issue proposes a profile-wide bound before #4099.
- **Fail-closed is a stated design value.** #480 says "the v2 line is fail-closed", and #1218 says "the fail-closed read path would turn a healthy retained generation into `ResetRequired`". #4094 and #3984 show the cost of fail-closed with unbounded retries. No issue was found that explains why the projection drain specifically is strictly head-of-line ordered.

## Contradictions

1. **Do linked worktrees share one project store?** #470 (2026-08-31) says "`projects/` contains only the canonical id" for a linked worktree, and #2402 (2026-10-01) says "Linked worktrees share one project graph database". But #4099 (2026-10-16) reports ">30 `proj_*` stores, one per registered root, so each agent worktree under `/fast/tmp` ... carried its own full code-graph index and sessions shard". Each is cited in the Direct Evidence section. These can be reconciled only if the `/fast/tmp` worktrees were not git-linked to a registered repository, or if identity resolution regressed. The tracker does not show which.
2. **User expectation versus V2 design.** #477 asked that "Worktree shards should be treated as their own distinct projects". The V2 resolution in #470 routes them to the canonical id instead.
3. **The writer as sole connection owner versus reader lanes.** #883 says "The writer remains the only connection owner", but #2521 and #4085 describe a separate reader lane ("exact SQL reader lane is saturated"). These may be consistent: the writer owns the write connection and readers own read connections. I note it because #883's wording is absolute.
4. **"Unavailable" semantics.** #4007 says "'Unavailable' should mean the writer is gone", yet the writer reported unavailable under CPU load. This conflicts with the typed-saturation invariant in #883.

## Gaps

- **Why one SQLite file per registered root**, and why `global.db` is separate from `user-sessions.db`. No issue states the original rationale. Searched: `registered root`, `per-worktree`, `worktree store`, `shard`, `common-dir worktree`.
- **Why sessions are sharded** (a project `sessions.db` beside the profile `user-sessions.db`). The `shard` search returned only runtime-bookkeeping issues (#3452, #3578, #2472). No rationale found.
- **Why Grafeo rather than SQLite for the code graph.** The `grafeo` search returned only operational defects (#763, #799, #2012, #2962, #874, #1247). No selection rationale found.
- **Why the Work/Workflow ports are synchronous.** I found no issue from the 06047de72e V2 foundation (2026-08-18) era explaining the choice. The seed's `--search` queries did not surface one.
- **The original user wording for the Turso abandonment and the "no backups" rule** is not in the tracker. #3402 and #2138 carry agent summaries only. #2138 closed with no recorded decision.
- **Why the projection drain is fail-closed** and strictly ordered by queue head. Searched `projection drain`, `host admission`, and `corrupt`. I found the incidents (#4094, #3984) but no design rationale.
- **Not opened because of the gh budget:** PR #3403 (Turso draft), #4011, #829, #2616, #1296, #3996/#4005 (Cursor drafts), #4097, #4083, #4066, #2097, #2363/#2364, #2400, #2050, #2106, #1979, #1982, #2877, #3760, #3838, #3571, #2469, #1079, #1090/#1094, #799, #836, #764, #765.

## Additional Leads

- `benchmark_data/storage-concurrency/CUTOVER-DESIGN.md`, `TRIAGE.md`, `turso-prototype/NATIVE-RUST.md`, and `SECURE-DELETE.md`. These are cited by #3402 and #3409 and lived on deleted branches `codex/native-storage-concurrency-20261010` and `claude/turso-native-cutover`. A source-control investigator may recover them from reflog or PR refs.
- Operator memory notes `goal-turso-cutover-merged.md`, `no-backups-ever.md`, `live-daemon-staging-container-bloat.md`, and `dogfood-v2-recovery-2026-08-01.md` may hold the original user rulings that #3402 and #2138 paraphrase.
- AGENTS.md sections cited by the issues: "No secondary copies" (#2138), "Preserve byte-exact identity contracts" (#2402), and "an availability claim needs a real production caller" (#2401).
- `docs/LCM-PAYLOAD-GC.md` (#2138): the specified-but-unwired GC scheduler.
- Commits named in the issues: `f347a0a46` (linked-worktree opt-in), `b501f5198` (writer awaited directly), `9f91c1f97e` and `97da9468e7` (Grafeo quarantine), `ab41420da` (store-runtime collapse), `b82a73e8bd` (`block_in_place` guard), and `5808a7511` (retained identity keyed by scope).
- #912 notes that branch `fable/verify-landed-855-908-909-912` "carries a different, deeper take on store-authority-keyed retained identity". An alternate design that was not merged.
- #2402 closed 2026-10-05 with no closing comment after Devin claimed the "foreign-base layer shape". Source control should check whether cross-worktree graph reuse landed.
