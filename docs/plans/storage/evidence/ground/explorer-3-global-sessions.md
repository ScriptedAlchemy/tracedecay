# Explorer 3: profile/global data and the session pipeline

Tree: worktree agent-a9a062786c04623b1 at origin/master e5a8ede6ea. Paths are relative to `crates/` unless absolute.
Prior grounding (/fast/tmp/arch-native-exec/ground-*.md) is assumed; runtime/executor facts are not repeated.

`gh pr list --search 4099` returned no PR. Issue #4099 (read instead) states the incident facts: 153 GB `~/.tracedecay`; `storage.table_growth` "unavailable" for `tracedecay.db` and `global.db`; >30 `projects/proj_*` stores; `lcm-payloads`; `user-sessions.db` + `user-sessions.grafeo` + WALs; a 229 MB `project_sessions` WAL; projection drain failing on a corrupt head since Oct 8. The live `~/.tracedecay` is now a fresh 144 KB profile, so no on-disk measurement is possible (`du` shows only `maintenance/`, `projects/`, two lock files).

## Headline answer (attribution of growth)

1. A transcript line is never in the host-admission spool. The spool holds hook *plans* (code-index sync, receipts). Transcript bytes are pulled from host files (`~/.claude/projects/**.jsonl`, etc.) by the per-database session refresh worker.
2. A captured line fans out to about 25 tables in ONE registered SQLite file: `~/.tracedecay/user-sessions.db` (profile scope) or `~/.tracedecay/projects/<proj_id>/sessions.db` (project scope). Large bodies may also produce a file under the sibling `lcm-payloads/` directory, and session topology goes to the sibling `*.grafeo`.
3. The authoritative copy (`observations.observation_json` or `session_canonical_bodies`) is **never released for live records**. Observation retention releases payloads only when a retrieval-anchor disposition is `superseded`/`deleted`. LCM retention drops `lcm_raw_messages` only after 180 days and only if a summary covers the row, and it never touches `observations`. `session_canonical_bodies` has no production DELETE. Every external-source ledger, the session-temporal tables, and the writer idempotency ledger are append-only.
4. Retention runs only on **mounted** session stores, at most one 500-row batch per kernel per store per tick, on a 24 h default cadence. Stores of unmounted projects get no session retention at all. Orphan-store GC keeps a store "Live" while any identity root exists, and the identity roots include the shared `git_common_dir`.
5. Fail-closed drain: projection is a single strictly ordered queue per database file (`projection_queue`, head = `MIN(observation_sequence)`, checkpoint must be `last+1`). A head that fails with SQLITE_CORRUPT gets no retry record and no skip. Every later observation in that file, from every provider and session, waits forever. Capture continues meanwhile, so `observations`, `projection_queue` and the `external_source_*` tables keep growing while nothing reaches `lcm_raw_messages`, the temporal tables, or summaries. Without summaries, LCM retention can never act.

## Components Found

- **HostAdmissionBroker / HostAdmissionRuntime / HostAdmissionSpool** (`tracedecay-host-admission/src/lib.rs:65`, `runtime.rs:38`, `spool.rs:104`): the daemon-owned, group-committed, bounded framed-file spool of hook event plans. There is one spool per registered sessions database.
- **HostAdmissionFacade** (`tracedecay-host-admission/src/lib.rs:288-905`): the capture, persist, cursor and drain API over a `RegisteredGlobalDb`, chosen by scope (profile or project) through `HostAdmissionAuthorities` (`authorities.rs:132-145`).
- **drain_projection_queue** (`tracedecay-host-admission/src/projection_drain.rs:21-342`): the fail-closed, ordered projection drain.
- **replay.rs** (`tracedecay-host-admission/src/replay.rs:12-76`): pass classification and backoff for spool replay workers (25 ms doubling to a 2 s cap).
- **schedule.rs** (`tracedecay-host-admission/src/schedule.rs:1-40`): a fair per-source scheduler over spool sequence numbers, strictly bounded, that holds references only.
- **GlobalDbSessionIngestAuthority** (`tracedecay-host-admission/src/session_ingest_authority.rs:32`): the adapter that lets `tracedecay-sessions` ingest write through the facade into one registered DB.
- **Profile host-admission replay registry** (`tracedecay-daemon-service/src/profile_host_admission_replay.rs`): coalesced per-profile replay workers; the replay pass is owner-installed.
- **MCP hook path** (`tracedecay/src/mcp/server/requests.rs:540-580`, `connection.rs:377-500`, `hook_dispatch.rs:82-163`): admit plan, replay, run plan, wake the session refresh workers.
- **SessionTemporalRefresh scheduler** (`tracedecay-session-runtime/src/session_temporal_refresh_scheduler/{worker,history,projector,registry,wake}.rs`): one worker per registered sessions DB. Each pass runs history ingest, then temporal projection, then the LCM summary convergence page.
- **Project/ProfileSessionHistoricalIngestor** (`.../history.rs:101-320`): wraps `tracedecay_sessions::runtime::ingest_*` with a GlobalDb authority.
- **tracedecay-sessions runtime** (`tracedecay-sessions/src/runtime/{ingest,hosts,source,observation}`): host transcript discovery, framing, capture requests, provider rotation, and per-provider drain calls.
- **GlobalDbObservationStore** (`tracedecay-global-db/src/observation_adapter.rs:63`): the observation and projection store adapter. Writes are submitted to the runtime writer. Reads are **sync** `dispatch_runtime_observation_read` (`:1752`).
- **Observation projection** (`tracedecay-global-db/src/observation_projection/{rebuild,apply,state,schema}.rs`): projects queued observations into `sessions`, `lcm_raw_messages`, dispositions, provenance, provider usage and workflow facts.
- **Registered schema stages** (`tracedecay-global-db/src/schema_stages.rs:978-1186`): one union schema installed into every registered SQLite file.
- **RegisteredGlobalDb / RegisteredGlobalDbOwnerV1** (`tracedecay-global-db/src/registered.rs:79-735`): the owner and lease wrapper over `runtime_core::db::Database`, plus retention, WAL and compaction entry points.
- **Session-temporal store** (`tracedecay-session-temporal-store/src/*`): generations, occurrences, assertions, refresh operations and receipts, summary nodes and sources, retrieval and hydration, and the Grafeo relation projection.
- **LCM** (`tracedecay-lcm/src/{schema,raw,payload,retention,gc}.rs`): `lcm_raw_messages` with FTS5, external payload files, projection-durable retention, and payload GC.
- **External-source authority** (`tracedecay-session-memory/src/external_source_store.rs:375,549`, SQL in `tracedecay-rusqlite-runtime/src/repository/external_source.rs`): a second per-observation commit ledger with its own pending-projection head queue.
- **Writer idempotency ledger** (`tracedecay-rusqlite-runtime/src/ledger/{schema,prune}.rs`): `td_runtime_writer_idempotency_v2` and `td_runtime_writer_checkpoint_v1` in every registered DB.
- **Retention drivers** (`tracedecay-maintenance/src/retention/{registered_store,cold_store,orphan_stores,live_compaction}.rs`, `tracedecay-maintenance/src/retention.rs`, `tracedecay/src/daemon/maintenance.rs:136,549,840-1000`, `tracedecay/src/daemon/scheduler.rs:1574`).
- **Project registry and store registration** (`tracedecay-global-db/src/{project_registry,store_registration,profile_registry_maintenance}.rs`): `global.db` rows per project, store, alias and branch scope.

## Flow: one Claude transcript line, hook to final rows

### A. Hook arrives (spool path; no transcript bytes)
1. The host hook reaches the daemon MCP handler. It publishes an activity event to the project sessions DB (`requests.rs:503-512`, `tracedecay-session-memory/src/event_lane.rs:1-5`). That event is an `analytics_events` row with provider `tracedecay-observability` and retention class `OptionalLocalDetail30d` (`event_lane.rs:186`).
2. The handler encodes the plan (`hook_events::encode_durable_hook_event_plan`) and calls `broker.admit(source, payload)` (`requests.rs:540-551`). The broker group-commits through `spawn_blocking` under a `Mutex<HostAdmissionRuntime>` (`lib.rs:121-135`, `93-107`).
   - Spool files are `<dir of db>/.<dbname>.host-admission/{records.bin, meta.json, quarantine.bin}` (`runtime.rs:39-53`, `spool.rs:66-67`, `meta.rs META_FILE`).
   - The profile broker is keyed by `user-sessions.db`, giving `~/.tracedecay/.user-sessions.db.host-admission` (`tracedecay/src/daemon/branch_admin.rs:378-391`, `tracedecay-cli/src/commands/storage.rs:42`).
   - Production mounts a project broker "on the runtime's project sessions database", giving `projects/<id>/.sessions.db.host-admission` (`tracedecay/src/mcp/server.rs:651-661` comment). The production open site was not traced (see Open Questions).
   - Bounds: 16 MiB, 4096 records, max wire record size (`tracedecay-sessions/src/admission/bounds.rs:8,14,17`). Quarantine is bounded to about spool bytes plus frame overhead (`spool/bounds.rs:33-36`). Compaction is lazy at 2x waste and 1/16 of the bound (`spool.rs:68-75`).
3. Replay runs inline (`replay_host_admission`, `connection.rs:377`, 64 records per pass): lease, decode, `run_hook_event_plan` (`hook_dispatch.rs:82`), then commit, quarantine or defer.
   - Plans are only `SyncFiles`/`AddBranch*`/`DebouncedIncrementalSync`, which go to the code-index scheduler, or `RecordTerminalReceipt`/`MarkTurnIngested`, which write the automation `dashboard_root` files (`hook_dispatch.rs:88-160`).
   - **No plan captures transcript content.**
4. On commit, the handler wakes `project_session_refresh_wake` and `user_session_refresh_wake` (`requests.rs:563-570`). It also writes route analytics to `ledger_sink()`, which is `accounting_db` or `global_db` (`ledger.rs:99-104,437-464`). That gives one `analytics_events` row per hook in `global.db` (non-observability provider, pruned at 180 days by `prune_global_retention`, `tracedecay-maintenance/src/retention.rs:1-10,210`; `scheduler.rs:1574-1595`).

### B. Refresh worker pulls the transcript (per registered sessions DB)
5. `run_session_temporal_refresh_scheduler` (`session_temporal_refresh_scheduler/worker.rs:225`) takes the dirty and historical-dirty bits. A history pass calls `history.run_pass()` (`worker.rs:734-768`) under a daemon-wide history admission semaphore.
6. `ProjectSessionHistoricalIngestor::run_pass` (`history.rs:156-195`) builds `GlobalDbSessionIngestAuthority` over the project `sessions.db` lease and calls `ingest_project_sources_for_provider_with_cancellation_and_codex_state`. The profile variant (`history.rs:280-315`) uses `user-sessions.db`, plus `global.db` as `registry_authority` to exclude registered project roots (`tracedecay-sessions/src/runtime/ingest/user.rs:39-50`, `hosts/claude.rs:87-118` `for_user_scope` / `retain_scoped_frames`).
7. Claude provider: `run_claude` (`ingest/project_provider.rs:789`) calls `ingest_project_claude_observations` (`:977`), which calls `claude_observation::ingest_source_with_sweep` (`hosts/claude_observation.rs:1400`).
   - The source frontier comes from `parse_offsets` via `get_parse_offset(CLAUDE_SOURCE_FRONTIER_KEY)` (`claude_observation.rs:1287-1350`; `tracedecay-global-db/src/discovery_queue.rs:127-184`, where `parse_offsets` doubles as the discovery queue).
   - The byte budget is 2 MiB per source and 16 MiB per pass (`source/jsonl.rs:61`, `claude_observation.rs:49`).
8. Per JSONL frame (one line), `build_claude_capture_request` (`claude_observation.rs:356`) builds identity = (source path, scope, generation, byte range, `stable_record_id`) plus the expected cursor. `capture_frame`/`capture_frame_window` (`:416,942`) then calls `admission.capture_observation(s)`.
9. `HostAdmissionFacade::capture_observation(s)` (`tracedecay-host-admission/src/lib.rs:646,686`) runs four steps:
   a. `record_source_presence` (`lib.rs:1248`) writes `observation_source_presence` (`tracedecay-global-db/src/registered_source_rewrite.rs`).
   b. `ObservationApplication::capture_observation` sanitizes on `ProcessBackgroundCpuV1` and calls `GlobalDbObservationStore::persist_observations` (`observation_adapter.rs:1497`), which calls `submit_runtime_write` (`:2251`), a `RepositoryWritePayloadV1` sent to the rusqlite runtime writer thread. The writer inserts:
      - `sanitization_receipts`, `retrieval_anchors`, `retrieval_anchor_aliases`, `observation_repository_provenance` and `observation_repository_captures` (`tracedecay-rusqlite-runtime/src/repository/observation/authority.rs`)
      - `observations`, `observation_retrieval_anchors` and `projection_queue` (`.../observation/mod.rs`)
      - `source_cursors` (upsert) and `source_cursor_advances` (insert, then prune superseded) (`.../observation/cursor_authority.rs`, `PRUNE_SUPERSEDED_CURSOR_ADVANCES_SQL`)
      - one `td_runtime_writer_idempotency_v2` row per submission, plus a checkpoint update (`ledger/schema.rs:3-36`)
      - `retrieval_anchor_dispositions` and `reverse_lineage` (`runtime-core/src/db/retrieval_anchor_schema.rs:33,63`)
      - bodies over 4096 bytes are slimmed into `session_canonical_bodies` (content-hash PK, deflate) (`tracedecay-store/src/canonical_body.rs:19,27`; `tracedecay-global-db/src/observation/canonical_bodies.rs`)
   c. `project_captured_outcomes` (`lib.rs:1284`) calls `RuntimeExternalSourceStore::capture_host_observations` (`session-memory/src/external_source_store.rs:375`). It inserts `external_source_commit_receipts_v2`, `_retained_receipts_v1`, `_mutations_v1`, `_objects_v2`, `_lineage_v1`, `_pending_projections_v1` and `_frontiers_v1` (`rusqlite-runtime/src/repository/external_source.rs:686-796,1362`).
   d. `publish_canonical_git_evidence` (`lib.rs:1346`) writes git-correlation evidence rows.
10. After all sources, `drain_projection_queue(admission, scope, …, 256)` runs (`claude_observation.rs:1218,1473`; `MAX_PROJECTIONS_PER_PASS` at `:53`), reaching `HostAdmissionFacade::drain_projection_queue` (`projection_drain.rs:21`). Its steps:
    - `converge_projection_predecessor` (`:45`; `observation_projection/rebuild.rs:409`) runs a bounded projector-version rebuild step. An incomplete rebuild means the drain returns `deferred`.
    - `external_source.drain_host_projection_replay(max)` (`:79`; `external_source_store.rs:549`) is the external head-of-queue projector. **Any error fails the whole drain** as `external_source_projection_unavailable` (`:80-83`).
    - The batched window (`project_queued_observations`, 32 per txn, `projection_drain.rs:13,95-155`; `rebuild.rs:213`) projects strictly the ready head: `MIN(observation_sequence)` with `next_retry_at <= now` and no active rebuild (`rebuild.rs:262-300`, same SQL as runtime read at `rusqlite-runtime/src/repository/observation/mod.rs:327-345`). Any error rolls back the window and falls back to per-item.
    - Per-item (`projection_drain.rs:156-275`) uses `project_observation` (`rebuild.rs:115`). The per-transaction writes are:
      - `sessions` upsert
      - `lcm_raw_messages` insert or update, with FTS5 triggers into `lcm_raw_messages_fts` (`tracedecay-lcm/src/schema.rs:118-130,309-358`)
      - `observation_projection_dispositions`, `_provenance` and `_aliases`
      - `observation_provider_usage`
      - `observation_workflow_facts`
      - `observation_projection_checkpoints` (`last_sequence`)
      - DELETE of the `projection_queue` row (`state.rs:362-370`)
      - external-payload files for data-URI, base64, >256K-char tool output, or quarantined assistant output (`tracedecay-lcm/src/security.rs:7-50`). The file is written *before* the DB transaction (`docs/LCM-PAYLOAD-LIFECYCLE.md` section 2), as `<storage_root>/lcm-payloads/payload_<sha>.payload` plus an `lcm_external_payloads` row, where `storage_root` is the parent dir of the sessions DB (`tracedecay-lcm/src/payload.rs:35-37`).
    - Then project-scope git evidence convergence runs (`projection_drain.rs:300-338`), then `persist_host_provider_coverage`.
11. The worker runs the temporal projection pass (`worker.rs:330-352`, `session_projection_refresh`), using `CanonicalSessionTemporalProjector::project` (`projector.rs:107-136`) and `SessionTemporalAccess::materialize_session_temporal_refresh_batch_result`. Writes go to `session_refresh_operations` / `_progress` / `_batch_bindings` / `_receipts`, `session_temporal_generations`, `session_occurrences` (+ `session_occurrences_fts`), `session_assertions` / `_supersession`, `session_current_entities`, `session_derived_evidence*`, `session_temporal_observation_effects` and `session_temporal_projection_receipts` (`tracedecay-session-temporal-store/src/projection/persist.rs:45-121,342`; schema `tracedecay-global-db/src/session_temporal_schema.rs:38-420`). The relation topology is published into the shard's Grafeo DB: `user-sessions.grafeo` or `projects/<id>/sessions.grafeo` (`tracedecay-store/src/runtime/identity.rs:526,654`; `tracedecay-session-temporal-store/src/relations.rs:1-5`, identities only), with SQLite `session_relation_receipts` / `_effect_journal`.
12. The LCM summary convergence page (`worker.rs:372-460`, `tracedecay-session-runtime/src/lcm_summary_convergence.rs`) writes `session_summary_nodes` (+ fts) and `session_summary_sources`, which are immutable ("retirement is an availability state, never a delete", `session_temporal_schema.rs:31-37`), plus `lcm_lifecycle_state`, `lcm_maintenance_debt`, `lcm_raw_predecessor_ranges`, `lcm_gc_*` and `lcm_summary_convergence_dirty_raw`.
13. Retention, later and mounted stores only, at a 24 h cadence: see Store Inventory.

### C. Which files hold the line
- Profile-scope sessions (any transcript whose cwd is not a registered project root): `~/.tracedecay/user-sessions.db` (+ `-wal`/`-shm`), `~/.tracedecay/lcm-payloads/`, `~/.tracedecay/user-sessions.grafeo` (+ `.wal`), and spool `~/.tracedecay/.user-sessions.db.host-admission/`.
- Project-scope sessions: `~/.tracedecay/projects/<proj_id>/sessions.db` (+ WAL), `projects/<proj_id>/lcm-payloads/`, `projects/<proj_id>/sessions.grafeo` and the project spool (`StoreLayout::new`, `tracedecay-runtime-core/src/storage/paths_and_io.rs:850-870`; `layout.rs:69-76`).
- `~/.tracedecay/global.db`: no session rows from capture. It holds the registry (`code_projects`, `project_aliases`, `store_instances`, `graph_scopes`, `store_artifacts`, `projects`), hook route `analytics_events`, `savings_ledger`, configuration, native integration, git-index transactions, observability rollups and work/handoff. The profile ingest reads it only for `registered_project_roots`.

## Store Inventory (my angle)

Format: engine | path | unit | writer | readers | schema version | growth bound / retention | failure containment.

1. **Registered sessions DB (profile)**: SQLite via rusqlite runtime | `~/.tracedecay/user-sessions.db` | per profile (`StoreShardScopeV1::ProfileSessions`, `tracedecay-store/src/runtime/identity.rs:134-152`) | runtime writer thread (repository submits) plus `Database::begin_write_transaction` for projection, temporal and LCM | observation reads: sync `dispatch_runtime_observation_read` parks the caller (`observation_adapter.rs:1752`, an async fn body with no await; this is the #4085 class); temporal and LCM: async `DatabaseEngineReadSnapshot` | union registered schema (`schema_stages.rs:978-1186`) with per-component versions (LCM 15 `tracedecay-lcm/src/schema.rs:34`; temporal 8 `session-temporal-store/src/schema_constants.rs:15`; git correlation 9 `tracedecay-sessions/src/runtime/git_correlation.rs:29`; projector `claude-session-message-v7` `tracedecay-store/src/projection.rs:24`); mismatch means typed reset-required, never stepped forward (`registered.rs:100-118`, `schema_stages.rs:425-433`) | see the per-table list below | one corrupt queue head blocks all projection for this file.
2. **Registered sessions DB (project)**: same as 1, at `~/.tracedecay/projects/<proj_id>/sessions.db`, one per registered project id (`ProjectSessions`). Spool, lcm-payloads and grafeo are siblings.
3. **global.db (profile authority)**: SQLite | `~/.tracedecay/global.db` | per profile (`StoreShardScopeV1::Profile`) | same writer model. A fresh install receives the *whole* registered schema, including observation, temporal and LCM tables (`schema_stages.rs:419-427`). An existing one never admits or converges session features | registry rows grow per registered root, alias and branch; `analytics_events` (non-observability) pruned at 180 days by `prune_global_retention` (`tracedecay-automation/src/config.rs:16-36`); observability rows at 30/395 days (`registered_analytics.rs:17-20,626-662`).
4. **Host-admission spool**: flat files `records.bin` / `meta.json` / `quarantine.bin` | sibling of each sessions DB | per registered DB | `Mutex<HostAdmissionRuntime>` on the blocking pool (`lib.rs:121-135`) | same | `FORMAT_VERSION` in frames; non-healthy integrity fails the open as `MetadataCorrupted` (`runtime.rs:62-64`) | **bounded** at 16 MiB / 4096 records plus quarantine | an undecodable record is quarantined; unsupported-version or non-committed records block only their source (`connection.rs:396-420`).
5. **LCM payload files**: flat files `lcm-payloads/payload_<sha256>.payload` | sibling of each sessions DB | written before the DB txn (crash leaves an orphan file) | `expand_payload` verifies sha | live while any raw row references them; GC grace 24 h, missing-reap 7 days (`tracedecay-lcm/src/types.rs:424-470`); no age policy by contract (`docs/LCM-PAYLOAD-LIFECYCLE.md` section 1 non-goals) | a missing file yields `PayloadMissing` for that ref only.
6. **Sessions Grafeo**: `user-sessions.grafeo` / `sessions.grafeo` (+ `.wal`) | per sessions shard | daemon-owned graph lease bound to the shard (`tracedecay-global-db/src/registered/session_relation_graph.rs:9-57`) | topology only; `ProjectionReplacement` per generation (`relations.rs:17-21`); Grafeo WAL checkpointing is listed as missing in #4099 (not traced here).
7. **Hook V2 admission ledger**: framed log `~/.tracedecay/hook-v2-profile-admissions/` and per project | bounded by records, age and bytes (`tracedecay-hooks/src/admission_ledger.rs:29-36,85`).
8. **Response handles**: files, 24 h TTL with sweep (`tracedecay-session-memory/src/response_handles.rs:16,276`).

### Per-table growth in a sessions DB (rows per captured line, retention)
- `observations`: 1 per line (`observation_json`, slimmed). **Never released while its anchor is active.** Release happens only 30 days after a `superseded`/`deleted` disposition, and replaces the payload with a tombstone while keeping the row (`tracedecay-global-db/src/observation/retention.rs:1-80,150-205`). The module doc cites one store measured at 1.8 GB here.
- `session_canonical_bodies`: one per distinct body over 4 KiB, shared by observations and LCM. **No production DELETE** (grep shows test-only deletes).
- `retrieval_anchors` (1.6 GB measured, same doc), `retrieval_anchor_dispositions` (append-only ledger with immutability triggers), `_aliases`, `_reverse_lineage`, `_derivative_tombstones`: skeleton rows are kept forever by design.
- `observation_repository_provenance` (1.4 GB measured) and `observation_repository_captures`: same payload-only release rule.
- `sanitization_receipts`, `observation_retrieval_anchors`, `observation_source_presence`, `observation_admission_refusals`: append-only.
- `source_cursors` is upserted. `source_cursor_advances` prunes superseded rows.
- `projection_queue` is consumed per item, but **grows while the drain is blocked**.
- `observation_projection_dispositions`, `_provenance`, `_aliases`, `observation_provider_usage`, `observation_workflow_facts`: about 1 per projected line. Deleted only by a projector-version rebuild, which first stages copies in `observation_projection_rebuild_*` (`rebuild.rs`), a temporary doubling.
- `sessions`: one per session. `lcm_raw_messages` (+ FTS5 shadow): 1 per message.
  - Offload after 30 days externalizes inline content into payload files, moving bytes rather than freeing them.
  - Drop after 180 days applies **only if covered by `session_summary_sources`** (`tracedecay-lcm/src/retention.rs:1-38,60-75,109-126`).
  - Neither pass touches `observations`.
- `external_source_commit_receipts_v2`, `_mutations_v1`, `_lineage_v1`, `_objects_v2`, `_authority_receipts_v1`, `_projection_effects_v2`, `_projection_lineage_v1`, `_projected_objects_v2`: JSON-heavy, about 1+ per captured line, **no DELETE** anywhere. Only `pending_projections`, `retained_receipts`, `projection_publications` and `frontiers` have deletes (`rusqlite-runtime/src/repository/external_source.rs:878-912,1052`).
- `td_runtime_writer_idempotency_v2` (about 0.5 KB per row, one per write submission): pruned only for rows of the *same incarnation* with a lower authority epoch, 256 per commit (`rusqlite-runtime/src/ledger/prune.rs:18-21,41,59-71`). Incarnation = daemon authority epoch per process start (`tracedecay-store-runtime/src/session_registry.rs:3001-3022`). **Rows from every prior daemon incarnation are unreachable but never deleted**, and age-based pruning is explicitly refused. `td_runtime_writer_checkpoint_v1` gains one row per (shard, incarnation).
- Session temporal: `session_occurrences` (+fts), `session_assertions`, `_supersession`, `session_current_entities`, `session_derived_evidence*`, `session_temporal_observation_effects` (1 per observation), `session_temporal_generations` (1 per refresh generation), `session_temporal_projection_receipts` (1 per batch), `session_refresh_operations` / `_progress` / `_batch_bindings` / `_receipts` (per refresh op), `session_summary_nodes` / `_sources` (+fts, immutable).
  - The only deletes in the crate are `session_temporal_resets`, `session_relation_effect_journal` and `lcm_summary_convergence_dirty_raw` (`session-temporal-store/src/{refresh.rs:1998,relation_receipts.rs:241,operations/generation.rs}`).
  - **No retention on any temporal table.**
- `analytics_events` (activity, observability provider): 30 days. `delivery_settlements` / `_source_receipts` / `_fanout_events`, `managed_test_runs`, git-correlation and workflow-index tables: no deletes found in global-db (git correlation not inspected in depth).
- `parse_offsets`: one row per discovered path (discovery queue). No delete seen.

### Retention execution reality
- `run_registered_store_retention` (`tracedecay-maintenance/src/retention/registered_store.rs:65-108`) runs four kernels: LCM retention ("all" providers), observation retention, observability prune, and payload GC. Batch defaults are 500 rows per pass (`tracedecay-lcm/src/retention.rs:109-111,840-850`; `observation/retention.rs:180-182`).
- It is invoked only for `administration.mounted_registered_session_databases()` (`tracedecay/src/daemon/maintenance.rs:866-880`) inside a bounded round-robin window, with per-unit writer admission (`try_with_writer`; a busy store defers).
- The cadence is `retention.interval_hours` with a 24 h default (`tracedecay-configuration/src/config/model.rs:17-19`, `maintenance.rs:549`). A tick reports `Complete` when the kernels succeed (`maintenance.rs:136-180`). I found no feedback from "batch was full" to "run again sooner".
- As read, throughput is therefore at most about 500 drops + 500 offloads + 500x3 observation releases per mounted store per day, against an ingest rate of thousands of lines per day.
- Unmounted project shards get no session retention. `run_cold_store_page` only collects orphans and incident debris (`tracedecay-maintenance/src/retention/cold_store.rs:1-80`).
- Orphan classification (`orphan_stores.rs:136-216`): a store is `Live` if **any** of canonical root, display root, **git_common_dir** or alias roots probes live. A store whose worktree is deleted but whose repository `.git` still exists is never `Orphaned`. GC fires 30 days after proven absence (`orphan_store_gc_days`).
- WAL: `checkpoint_result` truncates a WAL of 32 MiB or more only under the `Maintenance` authority role. The live daemon gets `RequiresExclusiveMaintenance` and the `-wal` file keeps its high-water size (`tracedecay-global-db/src/registered_maintenance.rs:9-20,52-93,110-120`).
- Incremental vacuum `live_compaction` runs for `global.db` and session stores (`maintenance.rs:956-975`). It reclaims only freed pages, which retention barely produces.

## Fail-closed projection drain: what one corrupt head blocks

- The queue is DB-wide, not per provider or session: `projection_queue(observation_id PK, observation_sequence UNIQUE, attempt_count, next_retry_at_micros, last_error)` (`tracedecay-global-db/src/observation/schema.rs:234-241`). The head is `MIN(observation_sequence)` (`rebuild.rs:262-300`). A rejection must satisfy `sequence == checkpoint.last_sequence + 1`, otherwise `Gap` (`rebuild.rs:357-363`). `drain_projection_queue("claude", …)` drains the same head as codex, cursor and the rest.
- Error dispositions (`rebuild.rs:103-105,135-182`; `projection_drain.rs:192-238`; `lib.rs:1059-1101`):
  - `Storage` (non-corrupt): persist a retry with 5 s base and 300 s max (`rebuild.rs:40-41,325-332`). The head stays and blocks the queue until `next_retry_at`.
  - `Contract`, `SanitizationRefused`: durable skip disposition, queue item consumed, the drain continues. **Only these two advance past a bad head.**
  - `Corrupt` (SQLITE_CORRUPT/NOTADB, classified at `tracedecay-store/src/projection.rs:611-662`): no retry row, no skip. The drain returns `terminal_unavailable("projection_store_corrupt")`, and the next wake retries the same head immediately.
  - `Gap`, `NotQueued`, `ObservationNotFound`, `OutputCollision`, `SessionOutputCollision`, `ProvenanceCollision`, `Anchor`, `UnsupportedProvider`, `InvalidRebuildFrontier`: `degraded(...)`, no skip and no retry, so the queue is equally frozen. The comment at `projection_drain.rs:224-226` calls this "fail-closed sequence ordering".
  - The batched window rolls back the whole window on any error (`rebuild.rs:236-245`). The per-item fallback then hits the same head.
- The same pattern sits in front: `converge_projection_predecessor` errors (`projection_drain.rs:46-52`) and the external-source `drain_host_projection_replay` head (`:79-83`, which also walks a head-of-queue, `external_source_store.rs:577-590`) both abort the drain before the observation queue is touched.
- Blast radius of one bad head in `user-sessions.db` or `projects/<id>/sessions.db`:
  - no new `sessions` or `lcm_raw_messages` rows for any session in that file
  - temporal refresh has nothing new to project
  - no summaries, so LCM retention can never mark rows durable
  - git evidence convergence and `persist_host_provider_coverage` are skipped (they run after the drain)
  - the history outcome is `Blocked` (`history.rs:340-353`, `failure.rs:504-511`), logged at warn (`worker.rs:308-315`)
  - **capture keeps going**: sources are processed before the drain in every pass (`claude_observation.rs:1430-1472`), so `observations`, `projection_queue`, `external_source_*` and the idempotency ledger keep growing
- Repair path: none automated. `projection_store_corrupt` is only surfaced as a status and reason code (`tracedecay-sessions/src/runtime/ingest/failure.rs:504-511`, history `:578-595`). A projector-version rebuild (`rebuild_projection`) deletes and replays the queue but needs readable rows.
- `rearm_queued_projection_retries` (`state.rs:236-310`) zeroes retry deadlines (not terminal ones) in batches. Its trigger was not traced.

## Boundaries

- `tracedecay-host-admission` depends on global-db, sessions, session-memory, store, runtime-core and privacy (`Cargo.toml`). It owns the spool, the facade and the drain.
- `tracedecay-sessions` holds host parsers, ingest orchestration and the `HostAdmission` trait. It has no SQLite; it writes only through `HostAdmission`/`SessionIngestAuthority`.
- `tracedecay-global-db` holds `RegisteredGlobalDb`, all registered schema, the observation adapter, projection, retention and registry. It depends on `tracedecay-session-temporal-store` (temporal schema and relation graph types) and on `tracedecay-lcm` (LCM schema install).
- `tracedecay-session-temporal-store` is generic over `SessionTemporalRegisteredDb` (`handle.rs:63-80`) to avoid depending on global-db.
- `tracedecay-session-runtime` holds the refresh scheduler, the historical ingestors, LCM summarization and retrieval services.
- `tracedecay-session-memory` holds the external-source store, event lane, fact store and response handles.
- Daemon composition (`tracedecay/src/daemon/*`, `tracedecay/src/mcp/server/*`, `tracedecay-daemon-service`) owns the cadences, the MCP hook handler and the brokers.
- `tracedecay-store-runtime/src/session_registry/maintenance.rs:671-718` opens each registered DB: `Database::publish_runtime(ReadWrite)`, then `admit_and_attach[_for_daemon]`. Long-lived daemons defer convergence to `registered_schema_convergence.schedule`.

## Non-Obvious Things

- "global-db" is a misnomer. `RegisteredGlobalDb` wraps every registered SQLite file (`global.db`, `user-sessions.db`, `user-memory.db`, each `projects/*/sessions.db`). Every one gets the same union schema of about 100 tables, including Work, Workflow, handoff, git-index, native integration, configuration and stack delivery (`schema_stages.rs:978-1186`).
- The spool is not a transcript path. Hook events wake ingestion. The JSONL file on the host is the real durable source and is re-read by byte cursors. If the host deletes or rotates the file before ingest, the bytes are gone. That is safe for growth but matters for "durability".
- The same line is stored up to four times in one DB:
  - `observations` / `session_canonical_bodies`
  - `lcm_raw_messages.content` (or a payload file after offload)
  - `lcm_raw_messages_fts` index text (up to 64 KiB, generated column)
  - `session_occurrences.index_text` + `session_occurrences_fts`
  - plus JSON copies in the `external_source_mutations` / `objects` and `observation_projection_provenance` rows (exact content of the mutation JSON not verified)
- LCM "offload" moves bytes from SQLite into `lcm-payloads/` and frees no total disk.
- The idempotency ledger comment says records are "deliberately not pruned by age", and pruning is scoped to the current incarnation. Every daemon restart therefore strands the previous incarnation's rows permanently (`prune.rs:41-56`).
- The observation retention doc ("three append-only, forever-growing evidence tables that dominated one observed sessions.db") admits the live-record gap. The fix only covers superseded or deleted evidence.
- Retention is scoped to mounted stores. The 30+ project stores of idle worktrees receive no row retention. Orphan GC protects them through `git_common_dir`.
  - Since repository collapse, linked worktrees should map to the repository's project id (`tracedecay-runtime-core/src/storage/layout.rs:105-127`), so 30+ distinct `proj_*` dirs implies non-linked checkouts, pre-collapse stores, or path-local ids. The cause was not established.
- The observation adapter's reads are synchronous inside `async fn` (`next_queued_observation`, `get_source_cursor`, `committed_source_cursors`; `observation_adapter.rs:1524,2448`). They park the polling native worker per read. The temporal and LCM paths use async snapshots.
- `projection_drain.rs` boxes every future (`Pin<Box<dyn Future>>`) to dodge rust-lang#159228 `Send` proof overflow, and the whole drain is one long async function on the refresh worker.
- `HostAdmissionRuntime::open` refuses any non-Healthy spool integrity as `MetadataCorrupted` (`runtime.rs:62-64`). A corrupt spool therefore makes the broker unavailable for its DB (`spool_runtime_unavailable`). Hook admission then fails, though transcript ingest (pull) is unaffected.
- Profile ingest reads every host transcript file, including ones owned by registered projects, and records cursor-only coverage for excluded frames (`claude.rs:99-118`). Cursor tables grow for all files in both stores.
- `analytics_events` exists in every registered DB. Hook route analytics go to `global.db`. Activity and observability go to the project sessions DB.

## Files Read (substantively)
- tracedecay-host-admission: `lib.rs` (1-140, 565-905, 1051-1140, 1248-1346), `projection_drain.rs` (1-470), `spool.rs` (1-120), `spool/bounds.rs`, `runtime.rs` (36-75), `replay.rs`, `authorities.rs`, `schedule.rs` (head), `session_ingest_authority.rs` (head), `discovery_queue.rs` (head)
- tracedecay-global-db: `lib.rs`, `registered.rs` (1-200, 640-735), `schema_stages.rs` (1-120, 146-240, 370-446, 978-1200), `observation/schema.rs` (122-256), `observation/retention.rs` (1-230), `observation/canonical_bodies.rs` (1-80), `observation_adapter.rs` (1-90, 1472-1600, 1752-1775, 1860-1878, 2251-2273, 2440-2520), `observation_projection.rs`, `observation_projection/rebuild.rs` (60-420), `observation_projection/state.rs` (outline, 236-310), `registered_maintenance.rs`, `registered_analytics.rs` (outline), `session_temporal_schema.rs` (selected DDL), `store_registration.rs` (1-80), `profile_registry_maintenance.rs` (1-60), `registered/session_relation_graph.rs` (1-80), `sqlite_persist.rs` / `transcript.rs` (heads)
- tracedecay-sessions: `runtime/ingest/project_provider.rs` (789-1022), `runtime/ingest/user.rs` (1-80), `runtime/hosts/claude_observation.rs` (356-478, 1218-1520), `runtime/hosts/claude.rs` (87-118)
- tracedecay-session-runtime: `session_temporal_refresh_scheduler/history.rs` (1-400), `worker.rs` (111-380), `projector.rs` (1-140)
- tracedecay-session-temporal-store: `projection/persist.rs` (1-130), `handle.rs` (1-80), `execution.rs` (head), `relation_projection.rs` / `relations.rs` (heads)
- tracedecay-session-memory: `external_source_store.rs` (375-590), `event_lane.rs` (head), `response_handles.rs` (grep)
- tracedecay-lcm: `retention.rs` (1-160, 835-950), `schema.rs` (112-130, 309-360), `payload.rs` (1-60), `security.rs` (1-60), `types.rs` (417-470)
- tracedecay-rusqlite-runtime: `ledger/schema.rs`, `ledger/prune.rs`, `repository/observation/mod.rs` (325-360), `repository/external_source.rs` (DDL and insert or delete list)
- tracedecay-maintenance: `retention.rs` (1-100), `retention/registered_store.rs` (1-140), `retention/cold_store.rs` (1-80), `retention/orphan_stores.rs` (86-216)
- tracedecay (daemon/mcp): `mcp/server/requests.rs` (490-580), `mcp/server/connection.rs` (360-520), `mcp/server/hook_dispatch.rs` (82-230), `mcp/server/ledger.rs` (99-112, 425-465), `daemon/branch_admin.rs` (370-435), `daemon/maintenance.rs` (130-180, 530-600, 840-1000), `daemon/scheduler.rs` (1570-1610)
- tracedecay-store-runtime: `session_registry/maintenance.rs` (640-720), `session_registry.rs` (2990-3030)
- tracedecay-runtime-core: `storage.rs` (1-80), `storage/layout.rs` (1-140), `storage/paths_and_io.rs` (840-880)
- tracedecay-cli: `commands/storage.rs` (1-140)
- tracedecay-configuration: `config/model.rs` (17-110)
- tracedecay-hooks: `admission_ledger.rs` (1-90)
- docs: `LCM-PAYLOAD-LIFECYCLE.md` (1-200), `LCM-PAYLOAD-GC.md` (1-60)

## Open Questions / not traced

- The production open site of the project host-admission broker. It is presumably in project server composition. `daemon/project_server_lifecycle.rs` is off-limits, so this is a gap. The path pattern `projects/<id>/.sessions.db.host-admission` is inferred from `open_for_database` plus the test comment at `mcp/server.rs:651-654`.
- Actual table sizes in the incident: unrecoverable, because the profile was deleted. The only quantitative evidence is the retention doc's 1.8, 1.6 and 1.4 GB per-table figures for one `sessions.db`, and #4099's 229 MB WAL.
- Whether the maintenance tick re-runs session retention sooner when a 500-row batch was full. I saw no has-more feedback (`maintenance.rs:136-180`), but did not read `MaintenanceCadence` / `request_due` fully.
- Exact JSON size of the `external_source_mutations_v1` / `objects_v2` / `commit_receipts_v2` rows per line, and whether they embed the observation body.
- Whether `session_occurrences` per generation is delta-only or copies prior rows. `persist.rs:101-106` suggests delta plus base coverage. Old generations' occurrence rows are never deleted either way.
- Git-correlation tables (`tracedecay-sessions/src/runtime/git_correlation/*`) and Work/Workflow tables: growth not inspected.
- Which queue head was corrupt in the Oct 8 incident (observation `projection_queue` vs external-source pending projection) and in which DB file.
- Doctor `storage.table_growth` "unavailable" lives in `tracedecay-maintenance/src/retention/storage_report.rs`. It was not read (another lane).
- What triggers `rearm_queued_projection_retries`, and whether anything ever sets `next_retry_at_micros = i64::MAX` (terminal) in production.
- Why 30+ `proj_*` stores existed despite repository collapse for linked worktrees.
