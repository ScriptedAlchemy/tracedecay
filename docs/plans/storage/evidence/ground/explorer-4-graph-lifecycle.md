# Explorer 4: code graph, code-index generations, store lifecycle (origin/master e5a8ede6ea)

Angle: crates/tracedecay-graph-db (Grafeo usage, WAL, generations, sealed store/layer, corruption, recovery,
publication, lease, owner, registry), tracedecay-code-index{,-runtime,-retention}, tracedecay-graph-query,
docs/graph-at-rest, tracedecay-maintenance, doctor + storage report, CLI `wipe` / `wipe --stale` / `init` /
`projects forget` / `storage reset-project-store`, docs/PROFILE-STORAGE-SUPPORT.md.
All paths relative to the worktree root unless absolute. Grafeo source read from the pinned fork checkout
`/home/zack/.cargo/git/checkouts/grafeo-751639c44c064cf7/be4713a` (Cargo.toml:88-92 pins rev be4713a5).
Not read (permission-refused): crates/tracedecay/src/daemon/project_server_lifecycle.rs.
Live profile note: `~/.tracedecay` was wiped at 07:31 today (only `projects/proj_a5b3d7e3ebe14ca7/` with
hook spool + dashboard files, 80 KB), so the on-disk layout below is reconstructed from code, not observed.

## Components Found

- GraphDb (crates/tracedecay-graph-db/src/runtime.rs:52-110): one Grafeo `GrafeoDB` per `.grafeo` file behind
  `RwLock<Option<GrafeoDB>>` plus `snapshot_gate` (parking_lot RwLock), verified-generation state, quarantine set,
  sealed-generation reader map, verify-once markers. Lazy engines open on first use and hibernate.
- GraphDbLocation/OpenOptions (location.rs:8-160): only `.grafeo` single-file containers; `WalSync`
  (read-write, Grafeo sidecar WAL, `DurabilityMode::Sync`) or `SealedReadOnly` (shared lock, no WAL, never
  checkpoints). Format version constant 7 (location.rs:21-23).
- GraphDbRegistry (registry.rs; support.rs): per-process map shard -> owner; eager vs lazy owner attachment
  (registry.rs:938-956); terminal `Faulted` slot for ResetRequired/FormatSuperseded/Corrupt/DurabilityUncertain
  (registry/support.rs:249-257). Production capacity `MAX_RETAINED_GRAPH_DB_OWNERS = 2 + 3*4096`
  (tracedecay-store-runtime/src/session_registry.rs:107-122), wired at session_registry/mounts.rs:205-209.
- GraphDbOwner/LeaseV1 (owner.rs:44-84): last operation lease dropped -> `hibernate_if_lazy` (attached owner)
  or `close` (derived owner).
- Corrupt-store protocol (corrupt_store.rs:1-235): deterministic-corruption re-verify under a
  `<container>.corruption-lock` flock, then delete WAL dir, `.verified`, `.spill`, container; superseded format
  path also discards superseded sealed generations.
- Recovery (recovery.rs): open error mapping (523-553), format marker validation (90-195), WAL collapse at open
  (202-262), projection quarantine nodes (`__tracedecay_graph_db_recovery_quarantine`, recovery.rs:19).
- Sealed store (sealed_store.rs:1-45, 210-230): per-generation immutable compact container
  `<stem>.sealed/<physical-namespace-hex>/{generation.grafeo,sealed.json,sealed.checked}`, read-only, digest-
  proven after reopen; kill switch `TRACEDECAY_GRAPH_SEALED_STORE=off` (sealed_store.rs:216, 486-497).
- Sealed layer (sealed_layer.rs; docs/graph-at-rest/README.md:47-58): graph format 7 refresh = delta container
  over a hard-linked cold base `base.grafeo`; >1/8 files changed seals cold.
- Staging release (generation_runtime.rs:44-112, 1540-1745): releases duplicate staging rows of a sealed,
  dependency-free generation; ten typed retention reasons.
- Row spill (generation/spill.rs:1-60): 32 MiB sorted runs under `.rows-<pid>-*` in the sealed root, swept at
  eager open (sealed_store.rs:731-755; runtime.rs:291-292).
- Verified marker (verified_marker.rs:274): `<stem>.verified` file binding container identity to completed
  digest proofs (a cache; loss costs a re-proof).
- Code-index retention kernel (crates/tracedecay-code-index-retention/src/code_index_generations.rs:1-200):
  file-based mark-and-sweep of sealed code generations, text artifacts, segments; journals, quarantines,
  receipts, graph-replay release queue and replay pool.
- Code-index publication store (tracedecay-code-index-runtime/src/code_index_scheduler/publication_store.rs:1-80):
  durable publication pointer + bounded generation index (`retain_bounded_generation_index`).
- Code-index corrupt publication reset (code_index_scheduler/registry/mount.rs:58-70, 263-318;
  retention `reset_code_index_scope_store` code_index_generations.rs:2299-2340).
- tracedecay-graph-query (src/lib.rs:1-40): read-only generation-pinned query authority over graph-db +
  code-index; owns no storage.
- Maintenance crate (tracedecay-maintenance/src): tick policy (tick.rs), store_maintenance (code generation
  retention, graph replay reconcile, scope reconciliation), retention (global table pruning, cold/orphan store,
  incident debris, live SQLite compaction, storage_report), telemetry (table growth).
- Daemon maintenance coordinator (crates/tracedecay/src/daemon/maintenance.rs:472-570, 709-1100).
- Doctor storage kernel (crates/tracedecay-daemon-service/src/doctor_kernel.rs:400-780).
- CLI: wipe (crates/tracedecay-cli/src/commands/storage.rs:27-48, 296-338, 355-560), wipe --stale
  (commands/stale_store_reset.rs), projects forget (project_cmd.rs:64-180 ->
  tracedecay-global-db/src/profile_registry_maintenance.rs:196-258 -> registry_maintenance/lifecycle.rs:267-298),
  init (commands/index.rs:67-300), storage report / reset-project-store (commands/profile_storage.rs).

## On-disk layout (reconstructed)

Profile root `~/.tracedecay/` (= `ProfileRoot::data_dir`):
- `global.db` (+`-wal`/`-shm`): profile authority SQLite (registry tables code_projects, project_aliases,
  store_instances, graph_scopes, store_artifacts: tracedecay-global-db/src/schema_stages.rs:55-111).
- `user-sessions.db`, `user-memory.db` (+ sidecars); profile graph containers `user-sessions.grafeo`,
  `user-memory.grafeo` (+ `.grafeo.wal/` dirs) (cli storage.rs:28-48). Two profile-wide graph owners
  (session_registry.rs:111).
- `maintenance/retention-cold-store-cursor-v1.json` (cold_store.rs:16-18; observed live).
- `projects/<project_id>/` = `profile_sharded_data_root` (tracedecay-runtime-core/src/storage/layout.rs:69-76),
  project id = `proj_` + 16 hex of sha256(identity root) (layout.rs:88-93); linked worktrees resolve to the
  repository identity root (layout.rs:113-124), so a worktree does NOT get its own project store by default.

Per project store `projects/<id>/`:
- `tracedecay.db` (+ sidecars): project relational store; holds the graph publication journal tables
  `graph_publication_replay_v1`, `graph_verified_heads_v1`, `graph_publication_replay_dependencies_v1`,
  `graph_publication_replay_tombstones_v1` (doctor_kernel.rs:708-763 queries them).
- `tracedecay.grafeo`: THE project graph staging container (graph locator = relational stem + `.grafeo`,
  tracedecay-store/src/runtime/identity.rs:500-527). Holds project memory graph rows and every code scope's
  namespace (`code-shard:<sha256>` per code shard, registry/code_graph_namespace.rs:1-35). Code shards are not
  physical stores: `resolve_graph_key` refuses Code scopes (store_locator_resolver/graph.rs:9-23).
- `tracedecay.grafeo.wal/wal_NNNNNNNN.log`: Grafeo sidecar WAL DIRECTORY (grafeo manager.rs:482-492,
  log.rs:717), 64 MiB segment rotation (log.rs:96).
- `tracedecay.verified`, `tracedecay.spill`, `tracedecay.grafeo.corruption-lock` (corrupt_store.rs:204-210, 145-150).
- `tracedecay.sealed/<hex>/generation.grafeo|sealed.json|sealed.checked`, layered dirs with hard-linked
  `base.grafeo`; `.staging-*` (in-flight seal), `.rows-<pid>-*` (spills).
- `tracedecay.graph-replay/`: replay pool of retired code-generation files, hard links
  (store_maintenance/mod.rs:331-337; generation_transactions.rs:358).
- `sessions.db` (+ spools) and `sessions.grafeo` (session relation graph) (identity.rs:654).
- `code-index-v1/` (scope_roots.rs:254-256):
  - `<sha256(canonical worktree root)>/` per worktree scope (code_index_generations.rs:790-861):
    `active-code-generation-v1.json` pointer, `code-generations-v1/generation-<digest>.json` (sealed generation
    files, "routinely ~1 GiB each" doctor_kernel.rs:404-409), `code-text-artifact-staging-v1/`,
    `graph-replay-release-queue/release-*.json`, receipts/quarantine/journal/lock files (lines 140-170).
  - project-shared `code-generation-segments-v1/` and `code-text-artifacts-v1/` (segments and text artifacts
    dedup across worktrees, code_index_generations.rs:794-840).
  - scope-retention receipts/journal/quarantine at the `code-index-v1/` level (lines 154-168).
- `store_manifest.json`, hook spools, dashboard, `branches/` (retired per-branch layout = incident debris,
  tracedecay-contracts/src/storage/debris.rs:23-39).

## Flow

### A. Graph store open (staging container)
1. Registry attaches an owner: eager `resolve_owner_attachment` or lazy `resolve_lazy_owner_attachment`
   (registry.rs:938-956). Durable relational/session owners use lazy (comment at :947-950; caller
   store-runtime/src/session_registry/code_graph/graph_attachment.rs:113).
2. Eager: `open_registered_database` (registry/support.rs:126-190) -> `GraphDb::open_with_store_state`;
   lazy: `open_registered_graph_lazy` (support.rs:104-124) -> engine opens at first use via `open_lazy_engine`
   (runtime.rs:146-199).
3. `open_validated_graph` (runtime.rs:1840-1894): `GrafeoDB::with_config` "replays the whole serialized LPG block
   log through the live mutation path and any sidecar WAL" (comment :1844-1846) => heap O(container rows).
   Logs `graph_engine_opened` with container_bytes and elapsed. Then `validate_or_initialize_format` (format
   marker node, schema FINAL_SCHEMA, version 7; recovery.rs:122-195), re-declares property indexes
   (recovery.rs:98-117), loads FormatState and quarantined projections, `collapse_replayed_wal`.
4. `collapse_replayed_wal` (recovery.rs:202-262): if the WAL has history beyond the durable checkpoint, run
   `database.wal_checkpoint()` (full container rewrite) and delete covered `wal_*.log` segments (keeps
   checkpoint segment + 2 predecessors, :264-284). Failure is a warn; next open retries.
5. Eager open also sweeps abandoned sealed `.staging-*` dirs and foreign `.rows-*` spills (runtime.rs:291-292,
   1715; sealed_store.rs:674-755).

### B. Graph write path
- Mutations go through `run_gated_batch`/`apply_locked` under the snapshot gate; after commit `sync_wal`
  (runtime.rs:1347-1385, 1916-1930). Grafeo "does not surface every WAL append failure from session commit"
  (location.rs:42-45, runtime.rs:1912-1915), so durability is "requested", not proven.
- There is no TraceDecay-driven periodic checkpoint: the only `wal_checkpoint` call is the open-time collapse
  (rg in graph-db src); Grafeo's `checkpoint_interval` is never configured (grafeo config.rs:310,415 default
  None). Container rewrite happens at close/hibernate (Grafeo close checkpoints unless the container is
  current, runtime.rs:1026-1032).
- Grafeo checkpoint mechanics (grafeo-storage file/manager.rs:620-930): each checkpoint writes ALL sections
  out of place (below the live generation if it fits, else appended), fsyncs, flips the inactive header slot,
  then truncates past the new end. File "oscillates around one-to-two generations in size" (:631-635). Deleted
  rows therefore only shrink the file at the next full checkpoint.
- Hibernation: last lease drop -> `hibernate_if_lazy` (owner.rs:58-84); a `GraphServingEnginePin` blocks it
  (runtime.rs:87). Hibernation closes the engine -> full checkpoint if dirty + WAL removed.

### C. Seal and publish of a code generation (graph side)
1. Code-index produces a sealed code generation file; graph publication stages the generation's rows (spill or
   manifest) and builds a per-generation compact container in `.staging-<digest>` then renames into
   `tracedecay.sealed/<hex>/` (sealed_store.rs:1-30, 660-672).
2. Sealed container reopened read-only and proven against the recovered digest before serving
   (sealed_store.rs:16-25); `sealed.checked` written only after proof (:217-219).
3. Dependency-free generations: staging rows become releasable once the relational head is seated; dependency-
   bearing generations keep staging rows (sealed_store.rs:17-22).
4. Refresh seals as a layer over a hard-linked cold base (sealed_layer.rs; README:47-58).
5. Relational verified head CAS in tracedecay.db journal; superseded replays retire via
   `retire_superseded_projection_replays` (registry/publication.rs:809-833); retirement deletes the sealed dir
   (`retire_sealed_generation_store`, sealed_store.rs:1661; `discard` :479-482).
6. Activation/recovery of a head: `verify_activated_generation` against relational digest; on
   `GenerationMismatch` the generation is quarantined (registry/publication.rs:2424-2430) and only a full replay
   of the adopted head repairs it (:2465-2470). Matching sealed artifact on disk is adopted
   (`open_sealed_generation_store_if_present`, :2434-2438). Direct sealed open without staging:
   `open_direct_sealed_generation` (sealed_store.rs:96-200).

### D. Staging row release (the 8.6 GB incident's fix)
- `release_sealed_generation_staging_rows_with` (generation_runtime.rs:1540-1586): evidence = seated sealed
  reader or on-disk receipt bound to the relational digest (:1588-1607); authority = resident lease or (new)
  the sealed artifact itself when no lease exists in this process (:91-112, 1700-1745). A hibernated staging
  engine is opened for the release and re-hibernated (:1556-1578). Each retained outcome logs
  `graph_staging_rows_retained` with reason (:1662-1679).
- Driver: maintenance `reconcile_graph_replay_releases` calls
  `store_runtime().release_one_sealed_generation_staging_rows(..., cursor)` ONE projection per call
  (tracedecay-maintenance/src/store_maintenance/graph_replay.rs:158-181 ->
  tracedecay-store-runtime/src/session_registry/code_graph.rs:2724-2800, page size 1 at :2796). The cursor
  advances only on Ok (graph_replay.rs:169-172); on Err it logs `graph_staging_release_failed` and the same
  projection is retried next tick.

### E. Code-generation retention (file side)
1. Bounded history at publish: `retain_bounded_generation_index_accounted` keeps active generation + active
   text head + newest suffix within 32 entries, 8 GiB, and 7 days relative to the active generation's sealed_at
   (code_index_generations.rs:110-115, 288-400).
2. Maintenance pass per mounted project `run_code_generation_retention` (store_maintenance/mod.rs:313-490):
   protection set = scheduler serving generation + seated graph generation + latest text serving + live
   native-preview bindings from global.db (mod.rs:111-137, 588-620); defers if the replay pool flock is held
   (mod.rs:352-354); plans with FULL digest verification on the blocking pool (mod.rs:146-200); reconciles graph
   replay releases (mod.rs:410-431); executes collection (mod.rs:204-299); reconciles again.
3. Collection hard-links each retired generation into `tracedecay.graph-replay/` before its release event is
   durable, writes `graph-replay-release-queue/release-*.json` (graph_replay_release.rs:34-70;
   generation_transactions.rs:358); pool entries are deleted only when the graph confirms the replay is no longer
   needed (`complete_code_generation_graph_replay_release`, graph_replay.rs:214-230).
4. Unmounted registered projects: `run_registered_code_generation_retention` (mod.rs:539-636) per scope,
   skipping mounted scopes and scopes with no provable root.
5. Scope-root reconciliation (stranded worktree scopes): plan_scope_root_retention, 7-day quiet gate unless the
   recorded root is gone (code_index_generations.rs:117-121; scope_roots.rs:154-200); runs only from a mounted
   project's full tick (tracedecay-maintenance/src/generation.rs:63-74).

### F. Maintenance cadence (daemon)
- Coordinator spawned at daemon/maintenance.rs:472-570; interval = `retention.interval_hours` (default 24,
  configuration/src/config/model.rs:17-19) (maintenance.rs:549). Retry/continuation delay = min(interval, 60 s)
  (tick.rs MaintenanceCadence::new). Each sealed code-generation publication pulls the next tick forward
  (`request_due`, maintenance.rs:520-546, 578).
- Tick (maintenance.rs:709-1060): background-CPU unit required; work list = mounted session DBs + mounted
  project graphs + every registered project with a shard dir; round-robin window of 8 units
  (MAINTENANCE_STORE_PAGE_LIMIT, :31) under per-unit `try_with_writer`; then profile observability pruning,
  global.db incremental-vacuum compaction, one cold-store/orphan page (8 stores, cold_store.rs:16), daily branch
  GC. Graph unit = code-generation retention + scope reconciliation + incremental vacuum of tracedecay.db
  (generation.rs:27-96). Session unit = LCM/observation retention + payload GC + compaction (maintenance.rs:136-230).
- Store telemetry: `advance_registered` reads SQLite table-growth for the sampled SQLite stores, keeping
  watermarks in process memory only (telemetry.rs:30-38, 204-282, 585-625); the tick only counts
  observed/unavailable (maintenance.rs:832-843).

### G. Corruption recovery at open
- `map_open_error` (recovery.rs:523-553): InvalidData/UnexpectedEof IO or StorageCorrupted/
  StorageRecoveryFailed/SerializationError on a preexisting store -> `Corrupt`; everything else -> `Unavailable`.
- Corrupt on an existing container (eager support.rs:144-171 or lazy runtime.rs:150-176): take
  `.corruption-lock` (try-lock; contended -> Unavailable, nothing touched, corrupt_store.rs:155-189), re-run the
  identical open; success -> serve; byte-identical fault -> delete family (WAL dir, `.verified`, `.spill`,
  container last; corrupt_store.rs:194-235), log `store_corrupt_deleted`, reopen fresh, log
  `store_rebuilt_after_corruption`; differing fault -> terminal `Corrupt` ("hardware-shaped").
- FormatSuperseded (older format < 7): same lock, delete family + `discard_superseded_sealed_generations`
  (sealed_store.rs:624-661), reopen fresh (corrupt_store.rs:107-134).
- ResetRequired (no format marker on non-empty store, marker count != 1, schema != FINAL_SCHEMA, newer/invalid
  version; recovery.rs:128-194): NOT auto-recovered; retained as a terminal `Faulted` registry slot.
- Sealed compact base: pages verified lazily on first touch; the engine latches the first failure and every
  later read of that open returns Corrupt (runtime.rs:1896-1903). Sealed open/proof failures map to
  `Unavailable("sealed generation store ...")` (sealed_store.rs:781-787) except Projection/GenerationMismatch.
  Layered base row-index corruption -> `RowIndexMismatch` absence, not an error (sealed_layer.rs:249-265).

## Store Inventory (this angle)

1. Project graph staging container `projects/<id>/tracedecay.grafeo`
   - Engine: Grafeo single-file container (pinned fork), LPG in RAM, sidecar WAL dir `tracedecay.grafeo.wal/`.
   - Unit: one per project (all code scopes/worktrees/branches of the repo + project memory graph as namespaces).
   - Writer: GraphDb under snapshot gate + database RwLock, called from store-runtime graph publication on the
     blocking pool (e.g. code_graph.rs:2764 spawn_blocking). Exclusive Grafeo file lock per open.
   - Readers: same GraphDb; read guards over the in-RAM LPG; no async port (sync API).
   - Schema/version: format marker node, version 7 + FINAL_SCHEMA; older -> delete + rebuild; anything else ->
     ResetRequired; no migrations ("no migration exists", README:57-58).
   - Growth bound: none explicit. Live size = rows retained: memory graph rows + staging rows of dependency-
     bearing or not-yet-released generations. Release driven 1 projection per maintenance call. File = 1-2x live
     generation size after checkpoint; WAL bounded only by open/close/hibernate checkpoint (64 MiB segments).
     Heap at open = whole container replay (8.6 GB container cost 20+ GB heap, generation_runtime.rs:91-99).
   - Failure containment: one corrupt container blocks the whole project graph (all code scopes + memory) until
     deterministic-delete rebuild; nondeterministic corruption or ResetRequired leaves a terminal Faulted slot
     for the daemon's life (registry.rs:930-937). One bad projection is quarantined per projection.
2. Sealed generation stores `tracedecay.sealed/<hex>/`
   - Engine: Grafeo compact (columnar) container, read-only, mmap base (GRAFEO_COMPACT_BASE_MMAP default on,
     grafeo mod.rs:3477-3493), zero-heap open.
   - Unit: one per (code shard projection, generation); layered generations hard-link a cold base.
   - Writer: seal path once (write_compact_container); never written after (`SealedStoreImmutable`).
   - Version: receipt version 1 + graph_format; mismatched receipt -> refused/discarded.
   - Growth bound: journal liveness (verified heads + pending replays + dependency closure). Retirement on
     supersession; abandoned `.staging-*` swept at eager open. No size cap.
   - Containment: per generation; corruption fails reads of that generation only; can be re-proved/republished.
3. Graph replay pool `tracedecay.graph-replay/`
   - Flat hard links to retired code-generation files; flock'd during publication hashing (GRAPH_REPLAY_POOL_
     ACQUIRE_BUDGET 50 ms, code_index_generations.rs:123-133).
   - Growth bound: none; entries leave only when the graph reconcile answers `Ok(true)`; `Ok(false)` (still
     needed) retains indefinitely (graph_replay.rs:206-238).
4. Code-index scope store `code-index-v1/<scope-hash>/` + shared segments/text artifacts
   - Engine: flat files (JSON pointer, sealed generation files, content-addressed segments, text artifacts),
     framed atomic writes, journals + quarantine + receipts.
   - Unit: per worktree root (scope) inside the project shard; segments/text artifacts per project.
   - Writer: code-index scheduler publication store under the generation store flock; retention under the
     same lock on the blocking pool.
   - Version: SEALED_GENERATION_FORMAT_REVISION_V1 gate; newer revision -> UnsafeState; retired revisions
     collectable (code_index_generations.rs:1076-1090).
   - Growth bound: per scope 32 generations / 8 GiB / 7 days of history (only removable entries; active and
     text head always kept). Per project: N scopes x 8 GiB + shared segments + text artifacts + replay pool.
     Stranded scopes need 7 days quiet unless the root is gone. Collection runs only on maintenance ticks.
   - Containment: corrupt pointer -> one automatic delete-and-rebuild of that scope per mount, then parked
     (mount.rs:263-318). A single generation file whose name does not match its digest makes the whole scope's
     retention plan fail UnsafeState (code_index_generations.rs:1060-1073) => that scope never collects again
     until repaired; Doctor census for the scope goes Unknown.
5. Global telemetry tables (global.db analytics_events, lcm_raw_messages): 180-day default windows,
   1000-row committed slices (tracedecay-maintenance/src/retention.rs:1-20, 112-118;
   tracedecay-automation/src/config.rs:11-12). Driver: daemon/scheduler.rs:1590.

## What bounds disk growth today, per store (summary)

- tracedecay.grafeo: no cap. Indirectly: staging release (1 projection/call), retirement of superseded replays
  (needs engine open; "a hibernated engine is never opened to delete", doctor_kernel.rs:622-624 counts
  `deferred_native_retirement_count`), checkpoint truncation at close/hibernate.
- tracedecay.grafeo.wal/: no cap; collapsed at open and removed at clean close/hibernate. A long-pinned serving
  engine accumulates segments. No TraceDecay-driven periodic checkpoint.
- tracedecay.sealed/: journal liveness only; layered hard links keep base bytes alive while any layer names them.
- tracedecay.graph-replay/: no cap; graph-confirmation gated.
- code-index-v1 scope: 32/8 GiB/7 d per scope; maintenance-cadence collection; blocked entirely by one bad file.
- code-index-v1 shared segments + text artifacts: unreferenced sweep, batch 32 per pass
  (code_index_generations.rs:171-182).
- Stranded scopes: 7 days quiet / root-gone; only from mounted projects' full ticks.
- Orphan project shards (whole `projects/<id>/`): `orphan_store_gc_days` default 30 (model.rs:13-15,38-41);
  8 stores per tick (cold_store.rs:16); relinkable stores never collected (orphan_stores.rs:13-20).
- Incident debris (`*.corrupt-*`, `*.recovered*`, `recovery-*`, `branches/*`): collected by the cold-store page.
- SQLite stores (global.db, tracedecay.db, sessions.db): incremental vacuum by free-page threshold
  (live_compaction.rs) - reclaims free pages only, does not bound live rows.
- Soft byte budgets: `store_soft_budgets_bytes` default empty (model.rs:45-48); only Doctor/dashboard compare,
  nothing enforces.

## What is measured vs unmeasured

Measured (on demand only):
- `tracedecay storage report` (CLI, offline-capable) and the daemon-paged admin variant
  (tracedecay-mcp/src/handlers/admin_cli.rs:429-439): per-store total bytes by family, SQLite free pages of
  `tracedecay.db`, code-generation retention dry run, unregistered dirs, full profile census
  (storage_report.rs:370-407, 1178). Not scheduled; no history.
- Doctor storage findings (doctor_kernel.rs:448-780): soft-budget over-budget (default none), SQLite table
  growth preview, code-generation census (superseded/collectable/stranded bytes, sealed head/superseded/
  abandoned-staging bytes, live graph container bytes, deferred retirements).
- Event logs: `graph_engine_opened` (container bytes, open ms), `graph_wal_checkpoint` (wal_bytes),
  `graph_staging_rows_released/retained`, `retention_code_generations` (bytes reclaimed),
  `retention_maintenance_tick` (reclaimed_bytes, deferred), `store_corrupt_deleted`.
- RSS sampler every 30 s (maintenance.rs:1218) - memory, not disk.

Unmeasured or mis-measured:
- No periodic profile-total or per-store disk sample anywhere in the daemon; scan_full_profile_size is called
  only by the CLI (profile_storage.rs:414). This is how 153 GB went unmeasured.
- Table-growth watermarks live in process memory (telemetry.rs:57, 296-300): every restart re-baselines; the
  maintenance tick discards samples (counts only). Doctor previews table growth only for stores <= 64 MiB
  (doctor_kernel.rs:400, 437-446), i.e. never for the large stores that matter.
- Storage report family classifier (storage_report.rs:106-139) only matches `tracedecay.db*`,
  `tracedecay.sealed`, `tracedecay.graph-replay`, `code-index-v1`, `sessions.db*`; `tracedecay.grafeo`,
  `tracedecay.grafeo.wal/`, `sessions.grafeo*`, `.verified`, `.spill` fall into `other`. The staging
  container (the 8.6 GB incident file) is reported as "other".
- Doctor `live_graph_container_bytes` (doctor_kernel.rs:765-773) stats `tracedecay.grafeo.wal` with
  `metadata().len()`, but that path is a DIRECTORY of wal_*.log segments (grafeo manager.rs:482-492,
  log.rs:152,717): WAL bytes are under-reported as one directory inode size.
- Doctor code-generation census record has no replay-pool field (doctor_kernel.rs:672-698): pool bytes are
  invisible to Doctor (storage report lumps them into generation_artifacts).
- Doctor census refuses (Unknown) when `code-generations-v1` has > 4096 entries (doctor_kernel.rs:410-435) or
  when any generation file fails the metadata planner.
- Profile-level graph containers (`user-sessions.grafeo`, `user-memory.grafeo`) are not in any family of the
  storage report except the full-profile census total.

## When a single file is corrupt: who is blocked

- `tracedecay.grafeo` (staging) deterministic corruption: first open (eager mount or lazy first use) pays a
  second full open under the corruption lock, then deletes and rebuilds; until canonical replay re-projects,
  every code scope and the memory graph of that project is unserved, and re-projection replays from sealed
  code generation files / relational sources (README, corrupt_store.rs:1-31). Nondeterministic fault or a lock
  contended by another authority: typed Corrupt/Unavailable; eager path retains a terminal Faulted slot for the
  daemon lifetime (registry.rs:930-937), blocking all graph operations for the whole project.
- `tracedecay.grafeo` ResetRequired (missing/duplicate format marker, foreign schema, newer format): no
  automatic path; `wipe --stale` does not cover the project graph (stale_store_reset.rs:224-262 targets only
  ProfileAuthority, ProfileSessions, ProjectSessions, hook admissions); `storage reset-project-store` deletes only
  `tracedecay.db` (+wal/shm) (profile_storage.rs:241-300). Remedy is local `wipe` or `projects forget`.
- One WAL segment torn: surfaces as a Corrupt open of the container (WAL replayed at open) -> same as above.
- One sealed generation container corrupt: reads of that generation fail Corrupt (latched per open,
  runtime.rs:1896-1903); open/proof failures -> Unavailable for that generation; other generations unaffected.
  A verification mismatch quarantines only that projection (registry/publication.rs:2424-2430).
- One `sealed.json` unreadable: census counts it `unrecognized` (sealed_store.rs:567-574); open refuses
  ("receipt does not bind").
- Code-index active pointer corrupt: that worktree scope gets one automatic derived-store reset per mount,
  then parks (mount.rs:263-318). Other scopes unaffected. The reset leaves segments/text artifacts and does not
  queue graph replay releases (code_index_generations.rs:2299-2340).
- One code-index generation file with a wrong name/digest: whole-scope retention plan fails UnsafeState
  (code_index_generations.rs:1060-1073), every tick logs `retention_degraded retention_plan_failed` and the
  scope stops collecting (unbounded growth for that scope); Doctor census -> Unknown.
- One graph replay release JSON unparsable: `graph_replay_release_evidence_invalid`, reconcile Failed every
  tick; collection still proceeds (mod.rs:401-431) but the tick outcome is Retry (60 s cadence).
- One staging release that keeps erroring: cursor not advanced (graph_replay.rs:169-181), so that projection
  is retried forever and staging release for every later projection in the page order stalls (head-of-line).
- Corruption-decision lock held by another authority: "leaving the store untouched" Unavailable
  (corrupt_store.rs:175-181) - retried per open.

## Reset commands: exactly what each deletes

- `tracedecay wipe` (local, storage.rs:476-560): under profile-offline (exclusive lifecycle lease; on
  contention quiesces the managed daemon service, 10 s bound, storage.rs:262-300): targets =
  `global::gather_target_projects(cwd, parents, children)`; for each, `remove_store_directory(data_root)` =
  `remove_dir_all(projects/<id>/)` + parent fsync + absence check (profile_registry_maintenance.rs:54-91), then
  `retire_registry_project_paths` deletes ONLY `projects` path-ledger rows (`DELETE FROM projects WHERE path IN`,
  lifecycle.rs:241-258, 289-298). It does NOT delete the `code_projects` row (so aliases, store_instances,
  graph_scopes, store_artifacts survive) nor any other global.db/user-sessions.db rows keyed by the project.
- `tracedecay wipe --all` (storage.rs:27-48, 318-338): removes fixed profile paths `projects`, `stores`,
  `remote`, `agent_managed`, `user-sessions.grafeo`, `user-sessions.grafeo.wal`, `user-memory.grafeo`,
  `user-memory.grafeo.wal`, `.user-sessions.db.host-admission`, profile hook admissions dir, response-handles
  dir, `lcm-payloads`, `hook_analytics.jsonl(.lock)`, `maintenance`; plus SQLite families (`""`,`-wal`,`-shm`,
  `-journal`) of `global.db`, `user-sessions.db`, `user-memory.db`; verifies absence. Schema-independent (does
  not open the databases). Not removed: identity/config, `lifecycle.lock`, and graph sidecars named by
  `with_extension` of the profile containers (`user-sessions.verified/.spill/.sealed`,
  `user-sessions.grafeo.corruption-lock`, same for user-memory) if they exist (inferred from corrupt_store.rs:
  204-210 + sealed_store.rs:500-502 naming).
- `tracedecay wipe --stale --yes` (stale_store_reset.rs:32-60): requires a reachable daemon, reads the
  daemon's reset-required list, refuses unless every store is a `ResettableStoreV1`; under profile-offline
  deletes: ProfileAuthority -> every `global.*` / `.global.db.*` entry in the profile root; ProfileSessions ->
  `user-sessions.*`; ProjectSessions -> `projects/<id>/sessions.*` and `.sessions.db.*` (includes
  `sessions.grafeo*`); hook admissions -> fixed dirs (stale_store_reset.rs:224-300). Never touches
  tracedecay.db/.grafeo/code-index. After a ProfileAuthority reset it prints `tracedecay init <root>` lines from
  each store_manifest.json.
- `tracedecay projects forget <sel> --yes` (project_cmd.rs:84-170): profile-offline; resolve registry
  context; for each `store_instances.store_relpath`, `remove_store_directory` (unless `--keep-store`); then one
  transaction: `DELETE FROM code_projects WHERE project_id=?` (cascades project_aliases, store_instances,
  graph_scopes, store_artifacts via FK, schema_stages.rs:68-104) and `DELETE FROM projects WHERE path IN
  (canonical, display, aliases)` (lifecycle.rs:267-287). Other global.db / user-sessions.db rows referencing
  the project id are untouched (not traced further; out of angle). `--dry-run` previews via the daemon.
- `tracedecay init [path]` (commands/index.rs:67-300): deletes nothing. Requires a running daemon; calls
  `tracedecay_status` admission-only with a 120 s deadline (cold project open), then `admin_sync` to queue the
  code-index reconcile; `--wait` polls `wait_for: ready` 110 s. `--fresh`/`--adopt-project` choose moved-store
  adoption (index.rs:122-143). ResetRequired errors are annotated with `storage reset-project-store`.
- `tracedecay storage reset-project-store --yes` (profile_storage.rs:52-300): requires the exclusive
  lifecycle lease (daemon must be stopped; no auto-quiesce, unlike wipe/forget); refuses a store already at the
  canonical SQLite schema; deletes `projects/<id>/tracedecay.db`, `-wal`, `-shm` only; with `--project-root`
  also removes the retired checkout-local `.tracedecay/`. Leaves tracedecay.grafeo, tracedecay.sealed,
  tracedecay.graph-replay, code-index-v1, sessions.* in place.

## Files Read

docs/graph-at-rest/README.md (1-140 + headings), docs/PROFILE-STORAGE-SUPPORT.md (all);
graph-db: lib.rs, location.rs, runtime.rs (1-200, 1020-1140, 1840-1930), generation_runtime.rs (1-125,
1540-1830), sealed_store.rs (1-330, 470-787), sealed_layer.rs (235-285), corrupt_store.rs (1-270),
recovery.rs (1-300, 505-556), registry.rs (1-140, 930-960, 1725-1745, 1860-1910), registry/support.rs
(100-200, 249-265), registry/publication.rs (2395-2470), registry/code_graph_namespace.rs, owner.rs (40-110),
error.rs (164-210), generation/spill.rs (1-60);
grafeo fork: grafeo-engine database/mod.rs (3405-3500), grafeo-storage file/manager.rs (1-80, 482-492,
565-660, 900-930), wal/log.rs (grep);
tracedecay-store/src/runtime/identity.rs (470-700); store-runtime store_locator_resolver/graph.rs,
session_registry.rs (70-130), session_registry/mounts.rs (190-240), session_registry/code_graph.rs (2724-2800);
runtime-core storage/layout.rs (1-140), config.rs/storage.rs constants;
code-index-retention: code_index_generations.rs (1-200, 284-400, 786-870, 1040-1080, 2290-2384),
scope_roots.rs (grep), receipt_store.rs, file_quarantine.rs/scope_quarantine.rs headers,
graph_replay_release.rs (1-70); code-index-runtime publication_store.rs (1-80), registry/mount.rs (50-70,
255-320), git_transactions/store.rs header; graph-query lib.rs, verified_query.rs header;
maintenance: lib.rs, retention.rs, tick.rs, generation.rs, store_maintenance/mod.rs, graph_replay.rs,
scope_reconciliation.rs (grep), retention/storage_report.rs (1-410, 1083-1100), telemetry.rs (1-120,
200-330, 585-625), cold_store.rs, orphan_stores.rs, incident_debris.rs headers, live_compaction.rs (1-60);
configuration model.rs (RetentionConfig), automation config.rs, lcm retention config;
daemon maintenance.rs (1-300, 520-612, 700-1100, 1240-1278); daemon-service doctor_kernel.rs (400-780,
930-1060); daemon core_doctor.rs header;
CLI cli.rs (Wipe, Forget, ProfileStorageAction), main.rs dispatch, commands/storage.rs (1-720),
stale_store_reset.rs (1-330), project_cmd.rs (55-200), commands/index.rs (115-300), profile_storage.rs (1-300);
global-db profile_registry_maintenance.rs (28-258), registry_maintenance/lifecycle.rs (1-330),
schema_stages.rs (55-111); memory notes live-daemon-staging-container-bloat.md, graph-seating-noop-starvation.md;
/fast/tmp/arch-native-exec/ground-*.md + verdict.md.

## Boundaries

- graph-db depends on grafeo-{engine,core,storage,common} (pinned fork), tracedecay-store (lease/locator
  contracts, GraphRecoveredGenerationDigestV1), tracedecay-domain, tracedecay-private-fs. It is synchronous;
  every caller hops to the blocking pool (store-runtime code_graph.rs:2764; maintenance plan/execute via
  native_runtime::spawn_blocking, store_maintenance/mod.rs:157, 221).
- Relational authority for graph heads/journal = project `tracedecay.db` (SQLite, other explorers). The
  Grafeo container is declared derived ("a derived index in every namespace it serves", corrupt_store.rs:3-8).
- code-index-retention depends on code-index (format revision) and runtime-core; below application; used by
  maintenance, doctor, code-index-runtime (publication_store.rs:22-29).
- maintenance depends on code-index-retention, code-index-runtime (scheduler registry), global-db,
  graph-db (error types); never names daemon types (lib.rs:1-7).
- Doctor (daemon-service) reads store telemetry ports owned by maintenance, the code-index planner, sealed
  census, and SQL against tracedecay.db journal tables.
- CLI destructive commands go through the lifecycle lease (`take_profile_offline`), never through the daemon,
  except `wipe --stale` (reads the list from the daemon first) and forget `--dry-run`.

## Non-Obvious Things

1. One Grafeo container per project holds every worktree/branch code scope plus project memory; the per-worktree
   unit exists only in code-index-v1 (files) and as namespaces. Corruption or a Faulted slot of that one file
   takes down every scope of the repo.
2. Opening the staging container is O(rows) heap (full LPG replay); "never compacted" except by full-container
   rewrite at checkpoint. The sealed lane (mmap compact) is what makes opens cheap, and only for released,
   dependency-free generations. Memory-graph rows and dependency-bearing generations always stay in staging.
3. Grafeo WAL is a directory with 64 MiB segments; TraceDecay checkpoints it only at open (collapse) and via
   close/hibernate. A pinned serving engine never hibernates (runtime.rs:87), so WAL growth while pinned is
   bounded by nothing in TraceDecay.
4. Durability is weaker than "WalSync" suggests: Grafeo can swallow WAL append errors; sync_wal only observes
   current WAL state (location.rs:42-45; runtime.rs:1912-1915).
5. `SealedReadOnly` exists because a write-capable reopen of a 3.2 GB sealed artifact re-serialized it to 6.4 GB
   (location.rs:46-55).
6. `COMPACT_ROUND_TRIPS_BYTES` stays off; a 431-file generation failed its compact digest proof at scale and
   stayed "permanently unseated behind activation retries" (README:33-45).
7. Staging release runs one projection per maintenance call and the cursor stalls on error (head-of-line).
   Release needs the engine open, so a hibernated store is opened (full heap replay) just to delete rows
   (generation_runtime.rs:1556-1578).
8. Retention plans with FULL digest verification of multi-GiB generation files on every pass that has work
   (store_maintenance/mod.rs:140-145), under a 50 ms pool-lock budget with deferral.
9. Hard links: replay pool and layered bases mean deleting a generation file or a sealed dir may free zero
   bytes; census dedups by (dev, ino) only within the sealed root (sealed_store.rs:582-609).
10. Local `wipe` leaves the `code_projects` registry identity; the next open re-registers the same project id
    and recreates the store. `forget` is the only command that retires the identity.
11. `reset-project-store` deletes the relational graph journal (`tracedecay.db`) but leaves the Grafeo
    container and sealed dirs, whose heads were journaled in the deleted DB; the census can then only classify
    sealed dirs as superseded (no heads) - inferred, not tested.
12. Default maintenance interval is 24 h; short 60 s cadence only while a unit reports MoreWork/Retry. A store
    whose retention plan fails every time keeps the daemon on the 60 s retry cadence forever.
13. Corruption recovery deletes before the fresh open and keeps no copy (corrupt_store.rs:23-25); recovery
    correctness depends on canonical replay sources still existing (code generation files / replay pool).

## Open Questions

- Who retires the graph projection (verified head, journal rows, sealed dir) of a code shard whose worktree was
  deleted? Scope reconciliation (scope_reconciliation.rs) retires scheduler owners and the code-index scope dir;
  the only graph retirement found is `retire_superseded_projection_replays` (registry/publication.rs:809-833),
  which handles superseded replays, not dead projections. If none, deleted-worktree heads keep sealed bytes
  forever and count as "head_bytes" in Doctor.
- Does `reset_code_index_scope_store` (deletes generation files without release events/pool links) strand graph
  sealed generations whose replay source is now gone? Not traced.
- Whether the lazy-owner open path also converts a Corrupt/ResetRequired into a sticky registry Faulted slot
  (eager path does; lazy faults surface in `open_lazy_engine` per call). Not fully traced.
- What a Faulted project graph blocks at project open (project_server_lifecycle.rs is permission-refused; the
  project-open orchestration was not read).
- Why 30+ project stores exist when linked worktrees resolve to the repository id (layout.rs:113-124): older ids
  minted before repository collapse, `.git`-less worktrees, or marker misses. Identity is another explorer's
  angle.
- `tracedecay_storage_status` "bounded size history": its source was not located (no `size_history` symbol);
  not traced.
- Retention of `sessions.grafeo` / profile graph containers (session relation projections) - out of angle,
  not traced.
- Whether any path bounds the number of `code-shard:*` projections in one container (one per worktree/branch
  ever indexed) - none found.
