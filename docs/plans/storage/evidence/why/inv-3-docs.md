# Investigator 3: long-form documents (in-repo docs tree)

## Source

Long-form documents in the repository at origin/master e5a8ede6ea: the current
`docs/` tree (V2 plan set, operating model, user guide, retention, Grafeo,
graph-at-rest, serving path, LCM payload contracts), root `AGENTS.md`, crate
READMEs, and deleted docs recovered with `git show <sha>^:<path>`. The Notion
playbook was adapted: "pages" are markdown files, "backlinks" are plan
cross-references, "page history" is `git log`/pickaxe on `docs/`.

## What I searched

- Listed all 107 current files under `docs/`; `find crates -maxdepth 2 -name README.md -o -name AGENTS.md`
  (only `crates/tracedecay-capture/README.md` and `crates/tracedecay-sdk/README.md` exist; neither discusses storage shape).
- Keyword sweep (case-insensitive) over `docs/`, crate READMEs, `AGENTS.md` for:
  `turso`, `libsql`, `writer actor`, `writer thread`, `reader pool`, `exact_sql`,
  `connection pool`, `r2d2`, `deadpool`, `sqlx`, `one database`, `per root`,
  `per-root`, `per worktree`, `per-worktree`, `repository identity`, `shard`,
  `grafeo`, `retention`, `fail-closed`, `fail closed`, `projection drain`,
  `host admission`, `WorkAttemptStoragePort`, `StorePort`, `asupersync`,
  `spawn_blocking`, `synchronous`, `checkpoint`.
- Pickaxe over `docs/` history (`git log -S`): `exact_sql`, `writer actor`,
  `reader pool`, `writer thread`, `r2d2`, `deadpool`, `single writer`,
  `WorkAttemptStoragePort`, `asupersync`, `sessions shard`, `shard_runtime`,
  `StoreRuntimeClientLease`, `projection_drain`, `connection pool`,
  `busy_timeout`, `SQLITE_BUSY`, `Turso/libSQL`, `turso`, `libsql`, `rusqlite`,
  `rusqlite cutover`, `tracedecay-contracts`, `tracedecay-store-runtime`, and the
  exact sentences quoted below (to date their first appearance, because
  `git blame` attributes most plan text to the 2026-08-24 revert/restore pair
  52a9aaba23 / fc20c4f0cb).
- `git log --diff-filter=D -- docs/` filtered for storage words; read the deleted:
  `docs/architecture/v2/storage-and-consistency.md` (ADR-003),
  `logical-brain.md` (ADR-001), `privacy-and-retention.md` (ADR-005),
  `docs/plans/2026-07-09-tracedecay-brain-rewrite.md`,
  `docs/plans/tracedecay-v2/NEXT.md`, `docs/superpowers/plans/v2/pr19-cutover-runtime.md`,
  `pr15-multi-root.md`, `2026-08-08-v2-rc-recovery-design.md`,
  `2026-07-28-daemon-startup-database-recovery-design.md`,
  `audits/v2-implementation-audit-2026-08-14.md`, `MULTI-BRANCH-DESIGN.md`,
  `MULTIPROJECT.md`, `DESIGN-DOC.md` (V1), `archive/MEMORY-STORAGE-GROWTH-AUDIT.md`,
  `RUNTIME-BLOCKING-IO-AUDIT.md`, `DOGFOOD-RECOVERY-RUNBOOK.md`,
  `vendor-patches/README.md`, `2026-07-31-one-shot-crate-split.md`, and the first
  version of `PROFILE-STORAGE-SUPPORT.md` (35d3247f5d).
- Read in full or in the relevant sections: plans 00 (index), README, 02, 03, 16,
  24, 27, 28, 32, 33, 38, 39, 40; `V2-OPERATING-MODEL.md`,
  `PROFILE-STORAGE-SUPPORT.md`, `SERVING-PATH-PERFORMANCE.md`,
  `graph-at-rest/README.md`, `LCM-PAYLOAD-LIFECYCLE.md`, `USER-GUIDE.md`
  (storage sections), `development/runtime-performance.md`, root `AGENTS.md`.
- Checked `Cargo.toml` grafeo/asupersync comments only to confirm doc-vs-reality drift.
- `git log --since=2026-10-01 -- docs` to look for docs tied to #4085/#4097/#4099/#4100/#3402/#3409.

## Direct Evidence Found

### Q1. Store unit: per root, per project, per worktree; global/profile DB

1. **V1 origin: one database per repo root plus a user-level global DB for accounting.**
   - "A global database at `~/.tracedecay/global.db` aggregates totals across all projects" and token savings accumulate "in the per-project database."
   - Where: deleted `docs/DESIGN-DOC.md` (Tokensave design doc, added 786e4a4c61 2026-03-26, deleted bff5690451 2026-09-17), "Token Tracking" section. Module list: "`global_db.rs` Cross-project token tracking (~/.tracedecay/global.db)".
   - Relevance: the global DB began as cross-project accounting beside a per-repo graph DB, not as a designed storage partition.
   - `MULTIPROJECT.md` (Enzo Lombardi, 207cc6025d, 2026-04-17): "tokensave currently assumes one project per database." It chose a `project` column inside one `.tokensave` DB for monorepos (Approach A), so the per-root DB unit already existed.

2. **Profile-sharded stores replaced repo-local `.tracedecay/` (2026-06-20).**
   - "Profile storage adds profile-sharded code-project stores such as `~/.tracedecay/projects/<project_id>/`. A repository may then have only an enrollment marker while graph DBs, sessions, payloads, response handles, branch DBs, and dashboard sidecars live in the profile shard." Also: "`~/.tracedecay/global.db` remains user-level accounting/registry state, not the canonical graph DB."
   - Where: first version of `docs/PROFILE-STORAGE-SUPPORT.md`, commit 35d3247f5d (Zack Jackson, 2026-06-20, "Squash merge PR #56").
   - Relevance: origin of "shard" as a word for the per-project store directory; one store per `project_id`.

3. **ADR-003 (V2 Phase 0) designed three shard kinds, not one per root.**
   - "Host-local SQLite owns mutable catalog, profile activity, and repository/privacy-domain project shards. `catalog.db` contains content-free identity allocation/routing and receipts; `activity.db` owns provider/agent/session/Turn/message/task and cross-project activity; `project.db` owns code/Git/delivery evidence and locators, never copied canonical messages. Cross-shard operations use journals/outboxes/sagas and vector watermarks."
   - Rejected alternatives: "One giant database: couples privacy, backup, and failure domains." and "Per-project session copies or per-branch/per-board databases: create competing authorities."
   - Where: deleted `docs/architecture/v2/storage-and-consistency.md` ("ADR-003: Local Shards, Packed Generations, and Consistency", Status "Accepted for V2 Phase 0"), introduced 0561026d6c/f51367cdfb (ScriptedAlchemy, 2026-07-12), deleted 5f0e721f6d (2026-07-14, "remove rewrite meta-infrastructure").
   - ADR-001 (same dir) also rejected "One giant SQLite file or one generic `core/common/services` crate: hides ownership and failure boundaries."
   - The brain rewrite plan gave the reason sessions sit in a profile shard: "Canonical provider transcripts live in a profile activity/session shard because a session may touch zero, one, or many projects. Project shards own scoped projections and locators, not duplicate canonical messages." (deleted `docs/plans/2026-07-09-tracedecay-brain-rewrite.md` line 34; drafted 71694e8697, 2026-07-10.)
   - Relevance: the most explicit "why multiple DB files" rationale in docs: privacy, backup, and failure-domain separation. See Contradictions for how this diverged.

4. **Designed unit: project-wide store shared across worktrees; only code indexes per worktree.**
   - "Project facts and project session/LCM data live in one canonical project-wide store shared across branches and worktrees. / Profile-wide user activity lives in the user/profile store. / Only code indexes are branch/worktree/snapshot scoped. / Worktrees resolve their project through the project registry and Git common directory. Missing or ambiguous authority fails closed. / No path may create a worktree-local, source-adjacent, in-memory, recovery, or direct-database fallback writer."
   - Where: `docs/plans/tracedecay-v2/README.md` lines 58-71, "Storage and authority". Pickaxe dates "Only code indexes are branch/worktree/snapshot scoped" to 541f215bbf (2026-07-15, "docs(v2): reconcile complete product roadmap").
   - Same rule in `02-store-crate.md` lines 53-54 ("a separate database per branch. Only code-graph indexes are branch/worktree scoped") and 76-77 ("Project facts and sessions remain project-wide; user sessions remain profile-wide; code graphs retain exact repository/worktree/ref scope").

5. **Worktree-to-primary collapse by repository identity was designed and recorded as shipped (2026-07-26).**
   - "**Local identity correction (2026-07-26).** Repository-level project/store identity now collapses a linked worktree onto its primary checkout through `repository_identity_root` at the single `default_profile_project_id` minting door. This does not merge worktree snapshot authority: exact worktree/ref identity still selects graph generations. Durable profiles also refuse roots under the OS temporary directory at `upsert_code_project`, and registry reaping retains a dead-looking project row whenever its store directory still contains data; it deletes registry rows only, never stores or session artifacts."
   - Where: `docs/plans/tracedecay-v2/16-cross-project-repository-worktree-scope.md` lines 28-35; added by 9d0432e1d5 (ScriptedAlchemy, 2026-07-26, "docs(plans): align shipped runtime capabilities").
   - Root `AGENTS.md` "Learned Workspace Facts" (from 79c28742db, 2026-07-28): "Linked worktrees share the primary checkout's project/store identity while retaining exact worktree snapshot authority."
   - `docs/USER-GUIDE.md` lines 139-142: "Linked git worktrees do not need their own `tracedecay init`. They resolve to the same registered project authority while each code generation retains exact worktree/ref/commit/snapshot identity." Lines 1336-1341 say the project store lives at `~/.tracedecay/projects/<project-id>` and "a git repository also carries its identity in `.git/tracedecay-project.json`".
   - Relevance: answers "was worktree sharing by repository identity ever designed?" Yes. Every current doc says linked worktrees share one project store. None of them describes or explains the 30+ per-worktree stores the question reports.

6. **Rationale for keeping the profile registry separate from session stores: schema-reset blast radius.**
   - "The profile authority holds no session data, so a session-feature schema change (LCM, git correlation, workflows) resets only the session stores and never the registry."
   - Where: `docs/USER-GUIDE.md` around lines 1296-1298 (wipe/reset table that lists `global.db`, `user-sessions.db`, `projects/<id>/sessions.db` as separately reset stores).
   - Relevance: a present-day stated benefit of the separate global/profile DB under the "exact final shape or `ResetRequired`" policy.

### Q3. Writer actor and reader pool

7. **The V2 plan specified one bounded writer actor per SQLite shard, with read-only snapshot pools.**
   - "Each SQLite shard has one bounded writer actor. It owns the write connection, serializes short transactions, and exposes typed commands rather than a shared connection mutex." / "Read services use bounded read-only connection pools and SQLite snapshot transactions. They never wait behind background compaction/migration without returning an explicit busy/partial state."
   - Also: "One bounded writer actor owns each SQLite shard connection. Concurrent agents enqueue append batches through a private spool; read services use short-lived read-only snapshot pools." and "The daemon is the only ordinary live process allowed to construct the V2 store factory, own mutable SQLite connections, checkpoint WAL, run query read pools, publish snapshots, or swap generations."
   - Where: deleted `docs/plans/2026-07-09-tracedecay-brain-rewrite.md` §7.7 "Write ownership and consistency" (lines 833-835) and §5.3A ordering/concurrency contracts (lines 457-458). First drafted 71694e8697 (2026-07-10); file deleted 5f0e721f6d (2026-07-14). No surviving doc repeats the writer-actor sentence.
   - Same plan: "WAL, busy timeout, queue depth, maximum batch bytes, maximum transaction duration, and checkpoint thresholds are explicit configuration with safe defaults and telemetry." It also bans filesystem clones of a live shard: "no copy, reflink, hard link, main-file-only archive, or DB-without-WAL family operation is legal on a live shard."
   - Relevance: the writer-actor + reader-pool shape was a written design choice "rather than a shared connection mutex". The plan states no comparison with a pooled-writer or async driver.

8. **Per-store writer gates exist because a daemon-wide writer mutex stalled unrelated projects.**
   - "**Writer administration was one daemon-wide mutex.** Its own comment conceded that 'a background refresh or a generation rebuild can hold this gate for minutes', and a git-watch sync held it across a full `cg.sync()`. The first request for an *unrelated* project parked behind it with no deadline. The gate is now per store (`daemon/store_writer_gate.rs`) with three classes, `Destructive` ..., `Owner` ... and `Content` ..."
   - Where: `docs/SERVING-PATH-PERFORMANCE.md` §7 "A request never parks on a store-sized hold" (lines ~244-259). The baseline at the top of the doc is "Measured baseline (2026-08-01, live daemon, 151K-node store, 8 concurrent readers): 670% CPU, one search 6+ minutes."
   - Relevance: incident-driven reason for the per-store `writer_gate.rs` in store-runtime.

### Q4. Grafeo for the code graph instead of SQLite

9. **Stated goal: move graph-shaped data off SQLite and custom adjacency structures.**
   - "**Goal:** Replace custom adjacency structures and graph-shaped SQLite storage with one embedded Grafeo runtime boundary while retaining SQLite only for genuinely relational, transactional, and content-bearing records."
   - Where: `docs/plans/tracedecay-v2/39-embedded-grafeo-graph-database.md` line 12, unchanged since the first version 955552d44b (ScriptedAlchemy, 2026-08-03, "docs(v2): plan embedded Grafeo graph cutover"). The code commit 0e3284dd2f ("feat(query): embed Grafeo graph projection") landed earlier the same day. All of these commit messages have empty bodies.
   - Plan index (00, lines ~1003-1013): "graph-shaped data moves off custom adjacency structures and SQLite. Relational journals, receipts, raw content, configuration, and execution fencing remain in SQLite."
   - Relevance: the only stated "why". No doc compares Grafeo against SQLite recursive CTEs or adjacency tables on a measured workload before adoption. Task 12 ("Compare against the pre-Grafeo baseline") remained unchecked, and the 2026-08-14 audit lists "E18 POST-RC Grafeo aggregate/performance provenance ... pre-Grafeo performance comparison" as outstanding (deleted `audits/v2-implementation-audit-2026-08-14.md`).

10. **Plan 40 states what the pre-V2 SQLite graph did well and what V2 adds.**
   - "Master (pre-V2) proved the opposite trade-offs work. SQLite adjacency graph, per-file incremental sync, no stored body text, open-and-serve startup, but lacked what V2 genuinely adds: immutable digest-sealed generations, verify-then-publish with a watermark, exact branch/revision identity. Doc 39 already assigns the durable graph to Grafeo; the cutover never completed, leaving the sealed JSON as both canonical authority **and** de facto serving store."
   - Where: `docs/plans/tracedecay-v2/40-partitioned-generation-storage-and-zero-copy-replay.md` §Problem (status "ACTIVE (decided 2026-08-22)").
   - Measured trigger in the same doc: "One sealed code generation for this repository is a single `generation-<digest>.json` of **1.39 GB**"; "Warmup on this repository read tens of GB, pinned the 8 GiB cgroup boundary, and starved queries."
   - Relevance: the doc credits immutable generations and verify-then-publish to V2's design. It credits the earlier SQLite graph with good operational economics.

11. **Why there is a Grafeo WAL plus verify-on-reopen: Grafeo does not surface every WAL failure.**
   - "Grafeo `0.5.42` does not propagate every session/direct-mutation WAL append failure. `GraphDurability::WalSync` therefore requests synchronous WAL flushing but is not a durable-publication receipt. ... persistent projection work remains unverified until close/reopen plus a full recovered-state digest matches its canonical replay manifest."
   - Where: plan 39 "Global Constraints"; restated in `docs/V2-OPERATING-MODEL.md` lines 53-60.
   - Plan 39 constraint: "The daemon registry assigns one ordinary `<canonical-store-root>/<relational-store-stem>.grafeo` database-file path ... Grafeo may manage its documented `.grafeo.wal` sidecar". This is why a project has a `.grafeo` file beside its SQLite files.

12. **Why the Grafeo dependency is a git-pinned fork.**
   - The fork is not explained in a long-form doc. The surviving explanation is a comment in root `Cargo.toml` (lines 75-87): "The published crates carry a tiered-storage arena whose `HotVersionRef` addresses a single 1 MiB chunk, which caps an epoch near 32 Ki node records and then panics. The fork spans epochs across chunks, types the exhaustion and oversize errors, fixes compact, overlay, close, and WAL recovery correctness, and adds the direct compact build ..."
   - Deleted `docs/vendor-patches/README.md` (removed c5ae9eee84, 2026-08-29) documented a `close()` patch. Its reason: closing "re-serializes the whole store even when nothing is dirty ... measured in the tiered-storage investigation at close-time serialization peaking around 14x the on-disk size."
   - Commits fa204136d7 (2026-08-27, vendor 0.5.42 "for local patching") and 18f3b7febd (2026-08-27, "consume the arena fix from our fork, drop vendor tree") mark the switch.
   - Relevance: the fork was driven by defects at TraceDecay's graph scale, not a planned choice (see Contradictions).

13. **Grafeo container growth and an open reclaim design.**
   - "The live `tracedecay.grafeo` for one project reached 11.1 GB while the newest sealed head it served was 0.75 GB." A correction later the same day says retained rows stay because "`delete_staged_generation_rows` returns `RetentionPending` when the engine is hibernated, by design, because opening a multi-gigabyte LPG container costs roughly twice its size in RAM". It lists two reclaim designs, "neither chosen yet".
   - Where: plan 39 "Dated amendment (2026-09-12, recorded decision): live container reclaim" and "Correction (2026-09-12, later the same day, measured)" (a0f1c05c5b).
   - `docs/graph-at-rest/README.md` measured a "14.3–14.4x publish peak" over on-disk size for replay-form LPG. It rejected whole-database `compact()` because "table count 50002 exceeds compact ID limit of 32767" under TraceDecay's one-label-per-entity schema.

### Q5. Turso/libSQL

14. **Recorded rejection (2026-09-30).**
   - "**Turso/libSQL as the local runtime is rejected (decided 2026-09-30).** Storage stays on rusqlite with bundled SQLite behind the `tracedecay-rusqlite-runtime` path and daemon-owned SQLite authority; future remote work composes over that authority. `libsql` 0.9.30/0.10-pre is async-only with a private raw connection (no online backup, progress handler, per-connection limits, worker-thread config, custom functions, or statement cache), bundles SQLite 3.45.1 with CVE-2025-29087 unpatched, and has the open double-close bug tursodatabase/libsql#2251. `turso` 0.8.1 lacks FTS5, cross-process file sharing, and backup/limits. `libsql-rusqlite` 0.9.30 inherits the old engine. Revisit only when a libSQL release ships on current SQLite with #2251 fixed."
   - Where: `docs/plans/tracedecay-v2/00-plan-set-index.md` lines 196-206, "Rejected and superseded decisions" (stated to be "normative"). Commit 81026d2ab7, author Zack Jackson, 2026-09-30, "docs(plans): record rusqlite storage decision (#2772)".
   - This text is a plan-register entry in Zack's commit. It is not a verbatim chat ruling. The commit body is empty.
   - Relevance: the strongest written reason for "one engine, SQLite". It names missing capabilities (online backup, progress handler, limits, FTS5, cross-process sharing), a CVE, and an open bug.

15. **Earlier history: V1 ran libSQL "for async support". V2 cut over to rusqlite before 2026-07-25.**
   - V1: "The database uses `libsql` (a SQLite fork by Turso) for async support." (deleted `docs/DESIGN-DOC.md`, "Database Layer").
   - The original Rust port design (5169129e6d, 2026-02-26) listed "`rusqlite` (bundled) | SQLite database". libSQL arrived later in V1. The V2 brain rewrite plan (2026-07-10) specified "standard host-local SQLite semantics through a storage trait, initially implemented with `rusqlite`, FTS5, and WAL".
   - The register entry before 2026-09-30 read: "**libSQL as the local runtime is superseded.** Its compatibility driver and local runtime were removed after the rusqlite cutover. The `tracedecay-rusqlite-runtime` path and daemon-owned SQLite authority replace it; future remote work composes over that authority rather than reviving a libSQL runtime." (commit 2a7211f650, 2026-07-25, "docs: record rejected v2 plan decisions").
   - Plan 28 lines 149-151: "The retired libSQL compatibility/runtime path is not a remote seam to revive."
   - Relevance: the engine moved rusqlite → libSQL (V1, for async) → rusqlite (V2 cutover around 2026-07-22) → Turso re-evaluated and rejected 2026-09-30. The re-evaluation that produced #3402/#3409 is not documented.

### Q6. Fail-closed behavior and missing growth bounds

16. **The original retention default was to keep everything until an owner policy says otherwise.**
   - "Defaults are exact: ordinary sanitized observations and canonical evidence are retained until a versioned owner policy says otherwise; user deletion uses strict `ingested_at < cutoff`, preserves minimal provenance/tombstone/hold evidence, and is resumable."
   - Rejected alternatives include "Retention anchored on optional occurred time or immediate hard deletion without receipts."
   - Where: deleted `docs/architecture/v2/privacy-and-retention.md` (ADR-005, "Accepted for V2 Phase 0"), 2026-07-12 → deleted 2026-07-14.
   - Relevance: the founding design treated "retain by default" as policy, so unbounded growth was the default rather than an oversight.

17. **Soft budgets are inert by default, and retention never deletes live evidence.**
   - "Store soft budgets are owner-configured and inert by default." (plan 00 index, "Storage retention, size, and efficiency" paragraph; first written 97a4455352, 2026-07-24, "docs(plan): record completed retention runtime").
   - Plan 38 §7: "Owners can set soft budgets; exceeding one is a finding, never silent." Non-goals: "No lossy deletion of live, referenced evidence: retention acts on superseded, orphaned, or projected-and-durable data only." and "No background compaction that competes with foreground writes."
   - Plan 38 was created c3422239db (2026-07-23). Its commit message cites "one 256GB profile reduced to ~75GB by removing never-retained data."
   - Doctor is diagnostic only: "Doctor is read-only diagnostic authority. ... It owns no mutation registry, repair dispatcher, preview/confirm/apply route, cleanup, GC, retention action, or recovery command." (plan 00 register item 10.)
   - Relevance: direct evidence that a hard bound was deliberately absent. Budgets report a finding and never enforce, and retention is limited to superseded or projected data.

18. **Measured growth classes and residues.**
   - Plan 38, "Measured failure classes", historical 2026-07-23 profile: "The profile totalled 106 GB: `projects/` was 101 GB across 464 shards, all `sessions.db` files totalled 35.7 GiB, `branches/` 30.8 GiB, `code-index-v1/` 22.2 GiB, and `global.db` 0.98 GiB." Census on 2026-07-31: "`sessions.db` ≈ 39 GB, `sessions.db-wal` ≈ 13 GB".
   - Class 4: "**Unbounded session retention with structural duplication.** ... `lcm_raw_messages` and `session_messages` can retain the same conversations in raw and projected form".
   - Plan 38 dated amendment 2026-09-12 lists two residues "each blocked on a design decision outside retention": `retrieval_anchors` (0.67 GB) and `observations.observation_json` (1.3 GB). The second "cannot be released on projection durability today because temporal retrieval reads `observation.observation_json` as the content authority".
   - Deleted `NEXT.md` (2026-08-30 wave): "Retention convergence landed with a binary handoff; the store was growing 64.9→83.3 GiB per 2 h on the old binary."
   - Deleted `2026-07-28-daemon-startup-database-recovery-design.md`: "An abandoned dirty marker held admission for about 7 minutes 50 seconds while `PRAGMA quick_check` examined a 645 MB graph database with about 5.1 GB of WAL" and "The project sessions database is about 17.6 GB with about 10.7 GB of WAL". Decision: "Graph admission remains fail closed behind one complete `PRAGMA quick_check`".
   - Relevance: large WAL-to-DB ratios were already measured in July. That bears on the WAL checkpoint-mode question (#4097), though no doc names PASSIVE vs RESTART.

19. **Fail-closed stance (general).**
   - `02-store-crate.md` line 59: "The store foundation routes CLI, MCP, dashboard, hooks, analytics, LCM, and ingestion through the daemon authority; daemon unavailability fails closed."
   - Brain rewrite plan: "Queue saturation applies tiered backpressure: coalesce rebuildable notifications, spill canonical observations durably, and reject/mark optional enrichment. It never silently drops prompts, tool events, approvals, edits, or outcomes."
   - Plan 38 Delivery: "Reconciliation remains deliberately fail-closed. If the worktree registry, durable configuration-root inventory, vector-stage dependency census, lease authority, or physical/logical scope binding cannot produce one complete revision-bound receipt, it retains every candidate scope".
   - Relevance: fail-closed (retain or refuse rather than drop) is a repeated principle. No doc explains `host-admission/projection_drain.rs` specifically. The only "projection drain" mention is plan 00 line 71 (shutdown cancellation "through host transcript discovery, provider ingest, projection drain, and code-index reconciliation").

## Indirect / Circumstantial Evidence

- **Branch DB copies were V1 design, later retired.** `MULTI-BRANCH-DESIGN.md` (Enzo Lombardi, ff828207ae, 2026-04-02; deleted 9c55bc7f0f, 2026-08-30) chose "Branch-Scoped DBs with Copy-on-Switch" (`cp(active_db, target_db)`). It rejected a branch column because "every branch multiplies row count". Plan 38's 2026-08-07 amendment retired per-branch SQLite copies ("there is no longer a per-branch database to accumulate or collect"). Suggests: the habit of one SQLite file per checkout context is V1 inheritance. Alternative reading: per-worktree stores have an unrelated cause.
- **Per-worktree code-index scope dirs are keyed on the canonical project root.** Plan 38 class 2: "Retention's unit is `code-index-v1/<sha256(canonical_project_root)>/`, and every caller derives exactly one scope from the project root it was handed ... One repository carried three scope directories under a single repository discriminator, two stranded by deleted agent worktrees, holding 7.2 GiB". Suggests: code-index storage is per worktree by design, and deleted agent worktrees strand it. This is code-index scope, not a whole project store, so it does not by itself explain 30+ project stores.
- **Operating assumption for agent churn.** `SERVING-PATH-PERFORMANCE.md`: "Agents edit the codebase continuously, commits, branch switches, and transient worktrees arrive at all times ... transient agent worktrees are indexed lazily on first query, never eagerly on registration, and deregistered on deletion." Suggests the design expected many short-lived worktrees.
- **Multi-worktree lifecycle authority was unproven at RC.** Deleted 2026-08-14 audit, B17: "Multi-worktree artifact reuse and move/delete lifecycle authority | No Plan 16 production authority proves content reuse without logical identity sharing, move-preserved identity, or delete/recreate identity replacement."
- **Identity drift created orphan stores.** Plan 38 class 3: "**Identity-drift orphan stores, ~41 GB.** A project-root path migration re-registered repositories under new project IDs; the old-identity stores remained silently". It was still "operationally unverified" as of 2026-08-08. Suggests a store-count inflation path that is unrelated to worktrees.
- **`exact_sql` was renamed from migration SQL.** Commit 1743b7bd68 (2026-08-03, "docs(storage): update exact SQL terminology") changed `MIGRATION_SQL_EXECUTION_LIMIT` to `EXACT_SQL_EXECUTION_LIMIT` and `migration_sql.rs 2828` to `exact_sql/ 2177`. Suggests `exact_sql/` grew out of the migration/schema-execution guard (30 s per guarded execution plus a progress handler). The progress handler is one of the capabilities the Turso decision says libSQL lacks.
- **Blocking work on async workers caused a real incident.** Deleted `RUNTIME-BLOCKING-IO-AUDIT.md` (2d7e1adec2, 2026-09-13): "On a real macOS profile the daemon's unix accept loop starved because every runtime worker sat in synchronous `gix::discover` and loose-ref `open()` inside connection admission." Also "`tokio::time::timeout` can only preempt at an await point". `SERVING-PATH-PERFORMANCE.md` records the move from `block_in_place` to `spawn_blocking` for ingest. Bears on Q2/Q3 only indirectly: it shows sync-vs-async placement is a live concern, not why the ports split.
- **Library-first rule.** Plan 00 register item 9 rejects "Custom infrastructure ... where a mature maintained library or existing TraceDecay authority already owns the mechanism". `AGENTS.md` requires custom "filesystem durability layers" to "delete more complexity than they add". These are relevant to any pool/actor redesign.
- **No-backups rule.** `AGENTS.md` "No secondary copies": "Never create or retain rollback copies, profile backups, old databases ...". With "exact final shape or `ResetRequired`" (`PROFILE-STORAGE-SUPPORT.md`, `V2-OPERATING-MODEL.md`), this suggests the design accepts reset or recreate rather than migration. `PROFILE-STORAGE-SUPPORT.md` allows one exception: additive adjacent-stamp steps (e.g. v34→v35, #834).

## Contradictions

1. **Designed worktree sharing vs observed per-worktree stores.**
   - Docs since 2026-07-26 say worktrees share the primary checkout's project/store identity: plan 16 lines 28-35, `AGENTS.md` learned fact, `USER-GUIDE.md` lines 139-142, README "Storage and authority".
   - The question states that each agent worktree has its own project store in practice (30+ on the operator host).
   - No doc reconciles these. This investigator did not verify the operator count (forbidden source).
2. **ADR-003 session placement vs current placement.**
   - ADR-003 (2026-07-12) and the brain rewrite plan put canonical sessions in one profile `activity.db` "because a session may touch zero, one, or many projects".
   - From 2026-07-15 the README says "Project facts and project session/LCM data live in one canonical project-wide store". `USER-GUIDE.md` lists per-project `projects/<id>/sessions.db` plus profile `user-sessions.db` for projectless sessions.
   - No doc found records why the profile-activity shard was dropped.
3. **ADR-003 branch handling vs V1 practice.** ADR-003 says "Branches use generation reuse/overlays, not database copies", while V1 `MULTI-BRANCH-DESIGN.md` used copy-on-switch. They were reconciled only on 2026-08-07 (plan 38 amendment).
4. **Plan 39 forbids a fork; the build uses one.**
   - Plan 39: "Depend on the exact published `=0.5.42` release. Do not pin an unpublished Git revision, maintain a TraceDecay fork, vendor Grafeo, or reimplement its storage, WAL, checkpoint, or recovery machinery." Step 3 (checked): the plan set contains no instruction to introduce an "unpublished Grafeo revision".
   - `Cargo.toml` pins `ScriptedAlchemy/grafeo` rev be4713a5 ("grafeo 0.5.43 from our fork") since 2026-08-27.
   - `docs/V2-OPERATING-MODEL.md` (edited 2026-10-15) still says "the workspace-pinned embedded Grafeo `=0.5.42` runtime".
5. **Plan 39's vector role vs later retirement.** Plan 39 makes Grafeo the "sole persisted and queried graph/vector projection". Plan 00 register item 11 (2026-09-13) retired dense retrieval and "Grafeo vector indexes", and the operating model now says Grafeo "holds no vector index". The part of the Grafeo case that rested on vectors no longer applies.
6. **Turso decision date vs PR closure date.** The plan register records the Turso/libSQL rejection as "decided 2026-09-30" (#2772). The question cites #3402/#3409 closed 2026-10-12, which would be a later re-attempt after the documented rejection. No doc after 2026-09-30 mentions Turso.
7. **Plan 38 retention status flip-flops.** Its own text records corrections on 2026-07-26 ("prior blanket claim ... withdrawn"), 2026-07-31 (generation retention "was stale"), and 2026-08-08 (compaction "class is closed"). Status claims in this doc have been wrong in both directions, so treat its per-mechanism status as dated.

## Gaps

- **Q2 (sync Work/Workflow ports beside async store ports): no doc rationale found.** Searched `docs/` and history for `WorkAttemptStoragePort`, `StorePort`, `synchronous`, `async port/trait`, `sync port`, `runtime-neutral`, `tracedecay-contracts`, `tracedecay-store-runtime`, `asupersync`. The only `tracedecay-contracts` docs commit (bf153536c1, 2026-09-08) renames layers with an empty body. Plans 24/32 specify "rusqlite transactions for atomic claims" and "owner shard" atomicity but say nothing about sync vs async.
- **Q3: no doc compares the writer actor + reader pool against a connection pool (r2d2/deadpool) or an async driver.** Pickaxe for `r2d2`, `deadpool`, `sqlx`, `reader pool` returned nothing. The writer-actor statement exists only in the deleted 2026-07-10 plan and gives the reason "rather than a shared connection mutex". The libSQL rejection lists missing rusqlite-level capabilities, an indirect argument against async drivers.
- **"Sessions shard" / `shard_runtime`:** pickaxe on `docs/` for `sessions shard`, `shard_runtime` found nothing. "Shard" in docs means a per-project profile store directory (2026-06-20) or a remote fenced authority unit (plan 28). No doc explains intra-project sharding of sessions.
- **Q4: no measured SQLite-vs-Grafeo comparison before adoption.** Plan 39 Task 12 Step 4 is unchecked, and audit item E18 says the comparison is outstanding.
- **Q5: no doc covers the October Turso re-attempt or its 2026-10-12 closure.** `git log --since=2026-10-01 -- docs` shows no storage-engine docs.
- **Q6: no doc explains `crates/tracedecay-host-admission/src/projection_drain.rs` or why its drain is fail-closed.** Searched `projection drain`, `drain`, `host admission`, `spool`. Nothing documents a hard size bound, a 153 GB profile, #4099, #4100 ("24 h grant freeze"), or #4097 (WAL PASSIVE vs RESTART).
- **Per-worktree store proliferation:** no doc, deleted doc, or amendment explains why worktrees get their own project stores despite the 2026-07-26 identity collapse.
- `docs/superpowers/` and `docs/development/` contain no storage-shape rationale beyond `runtime-performance.md` (benchmark methodology only).

## Additional Leads

- **Source control:** commits 977d957b33 (2026-07-26, "fix(storage): collapse worktree identity, refuse temp roots"), da66eeee6b (2026-08-05, "fix(storage): unify linked-worktree database authority"), 8861d844d6 (2026-07-28, "fix(daemon): canonicalize linked enrollment authority"), 144600dcbe (2026-09-04, "fix(daemon): alias same-branch linked worktree routes"), and 357195e98d (2026-10-02, "fix(cli): list only projects this profile holds a store for"). These are where the worktree-collapse design meets the code; they may show where per-worktree stores re-emerged. Also 4d418598ee (2026-07-22, "test(storage): cover SQLite cutover") and 9e3ca9fd2c (libSQL → rusqlite cutover), 1743b7bd68 (`migration_sql` → `exact_sql` rename), 79683eb527 (SQLite graph authority deleted), and e19add066a / 712f69ec04 (branch graphs moved into the single project store).
- **Issue/PR tracker:** #2772 (rusqlite decision PR body/review), #3402/#3409 (Turso re-attempt and closure reason), #834 (adjacent-stamp additive step), #2400/#2401/#2500/#2531 (scope retention), #2731 (per-branch DB residue deletion), #3003/#2984 (Grafeo fork as a direct git dependency), #3874 (asupersync 0.6.0 pin), plus the PRs named in the prompt (#4085, #4097, #4099, #4100, #3711, #3736, #3996/#4005).
- **External:** tursodatabase/libsql#2251 (double-close bug cited in the decision), and the fork `ScriptedAlchemy/grafeo` branch `tracedecay/0.5.42-arena-multichunk` and fork PR #1 (3e43a1c408 "adopt reviewed grafeo fixes at fork PR #1 head").
- **Operator memory notes / chat transcripts:** the original wording of Zack's "one engine, SQLite" ruling (2026-10-12) is not in docs. The docs carry only the 2026-09-30 register text in a commit Zack authored. Session transcripts around 2026-07-09..07-14 may explain why ADR-001/003/005 and the brain rewrite plan were deleted ("remove rewrite meta-infrastructure") and why the profile-activity session shard was dropped.
- **Code comments:** `crates/tracedecay-runtime-core/src/storage/layout.rs:51,120` and `crates/tracedecay-global-db/src/project_registry.rs:2128` call `repository_identity_root`. Comments there may explain when the collapse is skipped.
- Prior runtime grounding at `/fast/tmp/arch-native-exec/*.md` was not read; it is outside this source.
