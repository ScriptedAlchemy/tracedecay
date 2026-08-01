# Migration crate seams

`crates/tracedecay-migrate` now owns the whole migration subsystem, but the
runtime kernel it stands on had not been extracted when the move landed.
Every reference to a root-owned module was rewritten to
`crate::root_seam::<module>` (see `src/root_seam.rs`), which is deliberately
empty — so `cargo check -p tracedecay-migrate` reports exactly this surface
and nothing else. Once the kernel movers land, the lead repoints
`root_seam` at the crates named below and the seam closes with no edits to
the moved code.

Total seam references: **513** across **18** root modules.

## Where each seam module lands

| Seam module | Refs | Distinct items | Target crate |
| --- | ---: | ---: | --- |
| `storage` | 120 | 34 | tracedecay-runtime-core |
| `db` | 84 | 31 | tracedecay-runtime-core |
| `sqlite_read_snapshot` | 49 | 11 | tracedecay-runtime-core |
| `global_db` | 47 | 20 | tracedecay-global-db |
| `errors` | 43 | 4 | tracedecay-runtime-core |
| `sessions` | 31 | 13 | tracedecay-sessions |
| `lifecycle_lease` | 30 | 3 | tracedecay-runtime-core |
| `config` | 26 | 8 | ROOT KEEPS `config` — see the note below |
| `daemon` | 21 | 8 | tracedecay-daemon |
| `branch` | 13 | 9 | tracedecay-runtime-core (unassigned in the plan's kernel list) |
| `agents` | 8 | 6 | tracedecay-agent-hosts |
| `tracedecay` | 8 | 4 | unassigned (root facade `src/tracedecay.rs`) |
| `branch_meta` | 7 | 5 | tracedecay-runtime-core |
| `memory` | 7 | 3 | tracedecay-runtime-core |
| `open_store_holders` | 7 | 4 | tracedecay-runtime-core |
| `application` | 6 | 4 | tracedecay-application |
| `worktree` | 5 | 2 | tracedecay-runtime-core |
| `git` | 1 | 1 | tracedecay-runtime-core |

> **`config` is the one seam that is not a pure re-point.** The split plan
> keeps `config` in the root crate, so the eight constants and helpers below
> (`DB_FILENAME`, `TRACEDECAY_DIR`, `db_filename`, …) have to move *down* into
> the kernel rather than be re-exported upward. `branch` and `tracedecay` are
> not named in the plan's kernel composition either; the verifier report that
> refines `tracedecay-runtime-core` should adjudicate them.

## Catalog

Paths are relative to `crates/tracedecay-migrate/src/`. Items are the
referenced kernel item, i.e. what `root_seam::<module>` must resolve.

### `root_seam::storage` — 120 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `storage::(whole module)` | `consolidate/files.rs:11`, `memory_cutover.rs:27` |
| `storage::BRANCH_META_FILENAME` | `hermes.rs:1792`, `hermes.rs:1854`, `inventory/project.rs:15` |
| `storage::EnrollmentMarker` | `consolidate/mod.rs:50`, `hermes.rs:924`, `hermes/pipeline.rs:210`, `manifest/runtime.rs:11` |
| `storage::PrivateStoreIo` | `consolidate/mod.rs:50`, `manifest/runtime.rs:11` |
| `storage::PrivateStoreIo::create_dir_all` | `hermes.rs:453`, `hermes.rs:1436` |
| `storage::ProjectStorageLocation` | `registry.rs:13` |
| `storage::SESSIONS_DB_FILENAME` | `consolidate/sqlite/verify.rs:28`, `hermes.rs:1781`, `hermes.rs:1791`, `hermes.rs:1843`, `hermes.rs:1853`, `hermes/candidates.rs:79`, `hermes/candidates.rs:116`, `inventory/project.rs:15` |
| `storage::STORE_MANIFEST_FILENAME` | `hermes.rs:1795`, `hermes.rs:1857`, `hermes/candidates.rs:105`, `inventory/project.rs:15`, `manifest/runtime.rs:11`, `profile_backup.rs:453`, `profile_backup.rs:460`, `profile_backup.rs:589`, `profile_backup.rs:1021`, `profile_backup.rs:1042`, `profile_backup.rs:1053`, `profile_backup.rs:1144`, `profile_backup.rs:1211`, `registry.rs:13` |
| `storage::STORE_MANIFEST_SCHEMA_VERSION` | `hermes.rs:1784`, `hermes.rs:1846`, `profile_backup.rs:509`, `profile_backup.rs:1010`, `profile_backup.rs:1146`, `registry.rs:13` |
| `storage::StorageMode` | `consolidate/mod.rs:50`, `manifest/runtime.rs:11`, `registry.rs:13` |
| `storage::StorageMode::ProfileSharded` | `hermes.rs:926`, `hermes.rs:1787`, `hermes.rs:1849`, `hermes/pipeline.rs:212`, `inventory/project.rs:131`, `profile_backup.rs:512`, `profile_backup.rs:1013`, `profile_backup.rs:1149` |
| `storage::StoreKind` | `consolidate/mod.rs:50`, `manifest/runtime.rs:11`, `registry.rs:13` |
| `storage::StoreKind::CodeProject` | `hermes.rs:1786`, `hermes.rs:1848`, `profile_backup.rs:511`, `profile_backup.rs:1012`, `profile_backup.rs:1148` |
| `storage::StoreLayout` | `consolidate/mod.rs:50`, `hermes/pipeline.rs:164` |
| `storage::StoreManifest` | `consolidate/mod.rs:50`, `hermes.rs:1783`, `hermes.rs:1845`, `profile_backup.rs:504`, `profile_backup.rs:508`, `profile_backup.rs:1009`, `profile_backup.rs:1145`, `registry.rs:1482` |
| `storage::classify_registry_storage` | `inventory/scan.rs:161` |
| `storage::default_profile_project_id` | `hermes.rs:917` |
| `storage::default_profile_root` | `hermes.rs:75`, `hermes.rs:126`, `hermes.rs:982`, `hermes/pipeline.rs:217`, `inventory/scan.rs:19` |
| `storage::default_profile_sharded_layout` | `hermes.rs:1666` |
| `storage::has_sqlite_database_header` | `manifest/runtime.rs:11` |
| `storage::profile_sharded_data_root` | `manifest/runtime.rs:11` |
| `storage::profile_sharded_layout` | `hermes/pipeline.rs:207`, `manifest/runtime.rs:11` |
| `storage::read_enrollment_marker` | `inventory/project.rs:129`, `manifest/runtime.rs:11`, `registry.rs:13` |
| `storage::read_repository_identity_marker` | `registry.rs:13` |
| `storage::read_store_manifest` | `hermes/candidates.rs:106`, `manifest/runtime.rs:11`, `profile_backup.rs:476`, `profile_backup.rs:490`, `profile_backup.rs:557`, `profile_backup.rs:566`, `profile_backup.rs:1041`, `profile_backup.rs:1052`, `registry.rs:13` |
| `storage::resolve_layout` | `hermes.rs:829`, `hermes.rs:1071`, `hermes.rs:1140`, `hermes.rs:1236`, `hermes.rs:1437`, `hermes.rs:1589`, `hermes.rs:1815`, `hermes.rs:2098`, `hermes.rs:2122`, `hermes.rs:2146`, `hermes.rs:2173`, `hermes/pipeline.rs:225`, `inventory/scan.rs:169` |
| `storage::resolve_persisted_layout` | `hermes/pipeline.rs:192` |
| `storage::self` | `consolidate/mod.rs:50` |
| `storage::try_acquire_sidecar_lock` | `inventory/project.rs:292` |
| `storage::try_classify_project_storage_with_registry` | `registry.rs:13` |
| `storage::validate_project_id` | `manifest/runtime.rs:11`, `registry.rs:13` |
| `storage::write_enrollment_marker` | `hermes.rs:922` |
| `storage::write_store_manifest` | `manifest/runtime.rs:11` |
| `storage::write_store_manifest_to_path` | `profile_backup.rs:488`, `profile_backup.rs:1020`, `profile_backup.rs:1143` |

### `root_seam::db` — 84 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `db::Database` | `consolidate/runtime.rs:19`, `consolidate/sqlite.rs:7`, `consolidate/tests.rs:32`, `hermes/memory.rs:13`, `memory_cutover.rs:960` |
| `db::DatabaseAccessMode` | `consolidate/runtime.rs:19` |
| `db::DatabaseAuthority` | `consolidate/runtime.rs:19`, `consolidate/tests.rs:32`, `manifest/runtime.rs:428`, `manifest/runtime.rs:856`, `manifest/runtime.rs:1159`, `manifest/runtime.rs:1399`, `memory_cutover.rs:960` |
| `db::DatabaseAuthority::for_runtime` | `consolidate/sqlite.rs:1571`, `inventory/sqlite.rs:17`, `inventory/sqlite.rs:154`, `manifest/runtime.rs:406`, `manifest/runtime.rs:881`, `manifest/runtime.rs:1002` |
| `db::DatabaseAuthority::replace_file_atomically` | `manifest/runtime.rs:890`, `manifest/runtime.rs:992` |
| `db::DatabaseAuthorityRole::Test` | `consolidate/sqlite.rs:1575` |
| `db::DatabaseDeletionFence::acquire` | `memory_cutover.rs:1067` |
| `db::MaintenanceDatabaseScope` | `consolidate/mod.rs:1961`, `consolidate/runtime.rs:19` |
| `db::MemoryV2ArchiveDatabase` | `consolidate/sqlite/memory_v2.rs:2`, `memory_cutover.rs:23` |
| `db::TestDatabaseRuntimeMode` | `consolidate/tests/temporal.rs:57`, `memory_cutover.rs:960` |
| `db::TestDatabaseRuntimeMode::Existing` | `consolidate/sqlite.rs:1579`, `consolidate/tests.rs:66`, `consolidate/tests/temporal.rs:65` |
| `db::TestDatabaseRuntimeMode::Initialize` | `consolidate/tests.rs:55`, `consolidate/tests/temporal.rs:59`, `consolidate/tests/temporal.rs:80`, `consolidate/tests/temporal.rs:93` |
| `db::TestDatabaseRuntimeMode::ReadOnly` | `consolidate/tests.rs:77` |
| `db::engine` | `consolidate/sqlite.rs:8`, `consolidate/sqlite/inspect.rs:10`, `consolidate/sqlite/observation.rs:4`, `consolidate/sqlite/temporal.rs:6`, `consolidate/tests.rs:31`, `hermes.rs:404`, `hermes/copy.rs:10`, `hermes/fingerprint.rs:9`, `hermes/memory.rs:14`, `hermes/pipeline.rs:15`, `hermes/resolution.rs:7`, `inventory/sqlite.rs:4`, `registry.rs:8` |
| `db::engine::Connection` | `hermes/memory.rs:751` |
| `db::engine::DatabaseAttachmentExecutor` | `consolidate/tests/session_merge.rs:5` |
| `db::engine::Executor` | `consolidate/sqlite/external_source.rs:8`, `consolidate/sqlite/memory_v2.rs:1`, `consolidate/sqlite/projection.rs:1`, `consolidate/sqlite/verify.rs:3` |
| `db::engine::QueryExecutor` | `memory_cutover.rs:22` |
| `db::engine::TestConnection::open` | `consolidate/tests/temporal.rs:60` |
| `db::engine::TransactionBehavior::Immediate` | `hermes.rs:662`, `hermes/memory.rs:761` |
| `db::engine::Value` | `hermes/session_merge.rs:6` |
| `db::engine::Value::Text` | `registry.rs:1170`, `registry.rs:1185` |
| `db::engine::params` | `consolidate/mod.rs:28`, `memory_cutover.rs:918`, `memory_cutover.rs:1182`, `registry/tests.rs:12` |
| `db::enter_maintenance_database_scope` | `consolidate/mod.rs:208`, `consolidate/mod.rs:251`, `hermes.rs:208`, `inventory/scan.rs:24`, `manifest/runtime.rs:392`, `manifest/runtime.rs:604`, `manifest/runtime.rs:675`, `memory_cutover.rs:178` |
| `db::export_memory_v2_owner_archive` | `consolidate/sqlite/memory_v2.rs:2`, `memory_cutover.rs:23` |
| `db::import_memory_v2_owner_archive` | `consolidate/sqlite/memory_v2.rs:2` |
| `db::is_lock_contended` | `inventory/project.rs:295` |
| `db::list_memory_v2_archive_owners` | `consolidate/sqlite/memory_v2.rs:2`, `memory_cutover.rs:23` |
| `db::migrations::migrate_connection` | `hermes.rs:459` |
| `db::plan_memory_v2_owner_archive_import` | `consolidate/sqlite/memory_v2.rs:2` |
| `db::sqlite_generation_identity` | `consolidate/runtime.rs:945` |

### `root_seam::sqlite_read_snapshot` — 49 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `sqlite_read_snapshot::SnapshotAttachToken` | `consolidate/sqlite.rs:1506` |
| `sqlite_read_snapshot::SnapshotConnection` | `hermes/pipeline.rs:17`, `hermes/session_merge.rs:8` |
| `sqlite_read_snapshot::SnapshotDatabase` | `consolidate/sqlite.rs:223`, `consolidate/sqlite/inspect.rs:437`, `hermes/pipeline.rs:17` |
| `sqlite_read_snapshot::SnapshotSet` | `consolidate/evidence.rs:6`, `consolidate/mod.rs:851`, `consolidate/sqlite/inspect.rs:246`, `consolidate/sqlite/inspect.rs:267`, `consolidate/sqlite/inspect.rs:303`, `consolidate/sqlite/inspect.rs:337`, `consolidate/sqlite/inspect.rs:374`, `consolidate/sqlite/inspect.rs:435`, `consolidate/sqlite/verify.rs:20`, `consolidate/sqlite/verify.rs:23` |
| `sqlite_read_snapshot::SnapshotSet::capture` | `consolidate/sqlite/inspect.rs:239` |
| `sqlite_read_snapshot::SnapshotSet::capture_in` | `consolidate/evidence.rs:143`, `consolidate/finalize.rs:76`, `consolidate/finalize.rs:86` |
| `sqlite_read_snapshot::SourceGeneration` | `consolidate/evidence.rs:13` |
| `sqlite_read_snapshot::checkpointed_database_has_any_rows` | `memory_cutover.rs:677`, `memory_cutover.rs:690` |
| `sqlite_read_snapshot::family_fingerprint` | `consolidate/evidence.rs:59`, `consolidate/evidence.rs:108`, `consolidate/evidence.rs:153`, `consolidate/evidence.rs:185` |
| `sqlite_read_snapshot::open` | `hermes.rs:655`, `hermes.rs:778`, `hermes.rs:787`, `hermes/pipeline.rs:631` |
| `sqlite_read_snapshot::open_in` | `consolidate/evidence.rs:175`, `consolidate/finalize.rs:27`, `consolidate/sqlite.rs:833`, `consolidate/sqlite.rs:880`, `consolidate/sqlite.rs:1563`, `consolidate/tests/memory.rs:60`, `consolidate/tests/temporal.rs:101`, `hermes/pipeline.rs:405`, `hermes/pipeline.rs:595`, `inventory/sqlite.rs:32`, `inventory/sqlite.rs:167`, `manifest/runtime.rs:868`, `manifest/runtime.rs:1404`, `memory_cutover.rs:123`, `memory_cutover.rs:233`, `memory_cutover.rs:439`, `memory_cutover.rs:475`, `memory_cutover.rs:1197` |

### `root_seam::global_db` — 47 refs → tracedecay-global-db

| Referenced item | Sites (file:line) |
| --- | --- |
| `global_db::(whole module)` | `inventory/scan.rs:11` |
| `global_db::CodeProjectRecord` | `registry.rs:9`, `registry/tests.rs:224` |
| `global_db::GraphScopeUpsert` | `consolidate/mod.rs:47`, `registry.rs:9`, `registry/tests.rs:14` |
| `global_db::ProjectAliasRecord` | `registry/tests.rs:224` |
| `global_db::ProjectRegistryContext` | `registry.rs:9`, `registry/tests.rs:224` |
| `global_db::ProjectStoreContext` | `registry/tests.rs:224` |
| `global_db::RegisteredGlobalDb` | `consolidate/mod.rs:47`, `consolidate/sqlite.rs:35`, `consolidate/tests.rs:33`, `consolidate/tests/observation.rs:8`, `hermes.rs:12`, `hermes/pipeline.rs:16`, `hermes/resolution.rs:8`, `hermes/session_merge.rs:7`, `inventory/sqlite.rs:5`, `registry.rs:9`, `registry/tests.rs:14` |
| `global_db::RegisteredGlobalDbWriteTransaction` | `hermes/session_merge.rs:7`, `registry.rs:1156` |
| `global_db::StoreArtifactUpsert` | `consolidate/mod.rs:47`, `registry.rs:9` |
| `global_db::StoreInstanceRecord` | `registry/tests.rs:224` |
| `global_db::StoreInstanceUpsert` | `consolidate/mod.rs:47`, `registry.rs:9` |
| `global_db::ensure_registered_schema` | `consolidate/tests/temporal.rs:61`, `hermes.rs:462` |
| `global_db::observation::OBSERVATION_ANCHOR_SCHEMA_MIGRATION` | `consolidate/sqlite/observation.rs:93`, `consolidate/sqlite/observation.rs:114`, `consolidate/tests/observation.rs:113`, `consolidate/tests/observation.rs:144` |
| `global_db::observation::OBSERVATION_PROVENANCE_SCHEMA_MIGRATION` | `consolidate/sqlite/observation.rs:94`, `consolidate/sqlite/observation.rs:115`, `consolidate/tests/observation.rs:121`, `consolidate/tests/observation.rs:145` |
| `global_db::repair_session_temporal_store` | `registry.rs:251` |
| `global_db::schema_stages::begin_observation_authority_canonical_repair` | `consolidate/sqlite/observation.rs:606`, `consolidate/tests/observation.rs:784` |
| `global_db::schema_stages::finish_observation_authority_canonical_repair` | `consolidate/sqlite/observation.rs:626`, `consolidate/tests/observation.rs:816` |
| `global_db::schema_stages::validate_observation_authority_connection` | `consolidate/sqlite/observation.rs:135`, `consolidate/tests/observation.rs:1340` |
| `global_db::self` | `inventory/sqlite.rs:5` |
| `global_db::tests::harness::RegisteredGlobalDbHarness` | `registry/tests.rs:13` |

### `root_seam::errors` — 43 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `errors::Result` | `consolidate/files.rs:10`, `consolidate/mod.rs:46`, `consolidate/runtime.rs:22`, `consolidate/sqlite.rs:9`, `consolidate/sqlite/external_source.rs:11`, `consolidate/sqlite/inspect.rs:11`, `consolidate/sqlite/memory_v2.rs:7`, `consolidate/sqlite/observation.rs:11`, `consolidate/sqlite/projection.rs:4`, `consolidate/sqlite/temporal.rs:12`, `consolidate/sqlite/verify.rs:10`, `hermes/pipeline.rs:165`, `hermes/pipeline.rs:181`, `inventory/hermes.rs:14`, `inventory/project.rs:14`, `inventory/scan.rs:10`, `memory_cutover.rs:26`, `registry.rs:126`, `registry.rs:156`, `registry.rs:170`, `registry.rs:180`, `registry.rs:196`, `registry.rs:223`, `registry.rs:236`, `registry.rs:1049`, `registry.rs:1115`, `registry.rs:1159` |
| `errors::TraceDecayError` | `consolidate/mod.rs:46`, `consolidate/runtime.rs:22`, `consolidate/sqlite.rs:9`, `consolidate/sqlite/inspect.rs:222`, `memory_cutover.rs:26` |
| `errors::TraceDecayError::Config` | `hermes/pipeline.rs:167`, `hermes/pipeline.rs:197`, `inventory/scan.rs:59`, `registry.rs:238`, `registry.rs:247`, `registry.rs:1120`, `registry.rs:1130` |
| `errors::TraceDecayError::Database` | `consolidate/tests/observation.rs:1345`, `registry.rs:128`, `registry.rs:136`, `registry.rs:145` |

### `root_seam::sessions` — 31 refs → tracedecay-sessions

| Referenced item | Sites (file:line) |
| --- | --- |
| `sessions::SessionMessageRecord` | `consolidate/tests.rs:38`, `hermes.rs:409` |
| `sessions::SessionRecord` | `consolidate/tests.rs:38`, `hermes.rs:409` |
| `sessions::git_correlation::ensure_git_correlation_schema_in_transaction` | `consolidate/sqlite.rs:804` |
| `sessions::hermes::ingest_legacy_pinned_profile` | `hermes/pipeline.rs:561` |
| `sessions::lcm::LCM_SCHEMA_VERSION` | `consolidate/sqlite.rs:865`, `consolidate/sqlite.rs:870`, `consolidate/tests/session_merge.rs:102`, `consolidate/tests/session_merge.rs:135`, `consolidate/tests/session_merge.rs:205`, `hermes.rs:2138`, `hermes/pipeline.rs:343`, `hermes/pipeline.rs:346` |
| `sessions::lcm::payload::expand_payload` | `consolidate/tests/session_merge.rs:651` |
| `sessions::lcm::payload::upsert_payload_metadata` | `consolidate/tests/session_merge.rs:593` |
| `sessions::lcm::payload::validate_payload_ref` | `consolidate/sqlite/verify.rs:656`, `hermes/copy.rs:587` |
| `sessions::lcm::payload::write_external_payload` | `consolidate/tests/session_merge.rs:577` |
| `sessions::lcm::schema::ensure_lcm_schema_in_transaction` | `consolidate/sqlite.rs:801` |
| `sessions::lcm::schema::rebuild_raw_fts` | `consolidate/sqlite.rs:736`, `consolidate/sqlite.rs:777` |
| `sessions::user_sessions_db_path` | `hermes.rs:1874`, `hermes.rs:1995`, `hermes.rs:2016`, `hermes.rs:2032`, `hermes.rs:2057`, `hermes.rs:2074`, `hermes.rs:2120`, `hermes/pipeline.rs:184` |
| `sessions::workflow_index::ensure_workflow_index_schema` | `consolidate/sqlite.rs:809` |

### `root_seam::lifecycle_lease` — 30 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `lifecycle_lease::LifecycleLease` | `consolidate/mod.rs:1960`, `consolidate/runtime.rs:216`, `consolidate/runtime.rs:581`, `hermes.rs:124`, `hermes.rs:205`, `manifest/runtime.rs:322`, `manifest/runtime.rs:329`, `manifest/runtime.rs:597`, `profile_backup.rs:116` |
| `lifecycle_lease::acquire_exclusive_for_profile` | `consolidate/mod.rs:204`, `consolidate/mod.rs:247`, `hermes.rs:182`, `hermes.rs:938`, `hermes.rs:983`, `inventory/scan.rs:20`, `manifest/runtime.rs:380`, `manifest/runtime.rs:582`, `manifest/runtime.rs:666`, `memory_cutover.rs:174`, `profile_backup.rs:957`, `profile_backup.rs:1029`, `profile_backup.rs:1084`, `profile_backup.rs:1105`, `profile_backup.rs:1158` |
| `lifecycle_lease::canonical_or_original` | `consolidate/mod.rs:940`, `consolidate/mod.rs:1195`, `consolidate/mod.rs:1216`, `consolidate/mod.rs:2301`, `consolidate/mod.rs:2302`, `inventory/project.rs:406` |

### `root_seam::config` — 26 refs → ROOT KEEPS `config` — see the note below

| Referenced item | Sites (file:line) |
| --- | --- |
| `config::DB_FILENAME` | `consolidate/files.rs:199`, `consolidate/finalize.rs:8`, `consolidate/finalize.rs:161`, `consolidate/mod.rs:2249`, `consolidate/tests/external_source.rs:160`, `consolidate/tests/external_source.rs:320`, `consolidate/tests/lifecycle.rs:353`, `consolidate/tests/lifecycle.rs:355`, `consolidate/tests/lifecycle.rs:402`, `consolidate/tests/lifecycle.rs:555`, `consolidate/tests/memory.rs:140`, `consolidate/tests/schema.rs:15`, `consolidate/tests/session_merge.rs:253`, `memory_cutover.rs:1167` |
| `config::PinnedUserDataDir::new` | `hermes.rs:971` |
| `config::TRACEDECAY_DIR` | `inventory/hermes.rs:13`, `inventory/project.rs:13`, `inventory/scan.rs:9` |
| `config::db_filename` | `hermes/candidates.rs:80`, `hermes/candidates.rs:117`, `inventory/project.rs:13`, `memory_cutover.rs:438` |
| `config::has_project_database` | `hermes/resolution.rs:63` |
| `config::is_generated_dir_segment` | `inventory/project.rs:413` |
| `config::self` | `inventory/project.rs:13` |
| `config::user_data_dir` | `consolidate/preflight.rs:21` |

### `root_seam::daemon` — 21 refs → tracedecay-daemon

| Referenced item | Sites (file:line) |
| --- | --- |
| `daemon::QuiescedDaemonLifecycle::acquire` | `hermes.rs:88` |
| `daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve` | `consolidate/mod.rs:1699`, `consolidate/mod.rs:1799` |
| `daemon::daemon_reachable` | `consolidate/preflight.rs:23` |
| `daemon::profile_identity::load_or_create` | `consolidate/mod.rs:276`, `consolidate/mod.rs:391`, `consolidate/mod.rs:1697`, `consolidate/mod.rs:1797`, `consolidate/mod.rs:1968`, `hermes.rs:219`, `inventory/scan.rs:29`, `registry.rs:157` |
| `daemon::store_runtime::registry` | `consolidate/runtime.rs:12` |
| `daemon::store_runtime::resolver::canonical_store_locator_digest` | `consolidate/runtime.rs:892` |
| `daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1` | `consolidate/mod.rs:388`, `hermes/pipeline.rs:14`, `registry.rs:115` |
| `daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open` | `consolidate/mod.rs:393`, `hermes.rs:229`, `inventory/scan.rs:31`, `registry.rs:159` |

### `root_seam::branch` — 13 refs → tracedecay-runtime-core (unassigned in the plan's kernel list)

| Referenced item | Sites (file:line) |
| --- | --- |
| `branch::BranchAdminAction` | `memory_cutover.rs:957` |
| `branch::BranchAdminOutcome` | `memory_cutover.rs:957` |
| `branch::BranchAdminReport` | `memory_cutover.rs:957` |
| `branch::current_branch` | `inventory/project.rs:360` |
| `branch::detect_default_branch` | `consolidate/mod.rs:773`, `consolidate/tests/lifecycle.rs:148`, `consolidate/tests/lifecycle.rs:201`, `consolidate/tests/lifecycle.rs:224` |
| `branch::gc_dead_branch_stores` | `consolidate/tests/lifecycle.rs:1115` |
| `branch::prepare_branch_admin_mutation` | `memory_cutover.rs:957` |
| `branch::resolve_branch_db_path` | `inventory/project.rs:362` |
| `branch::sanitize_branch_name` | `consolidate/mod.rs:823`, `consolidate/prepare.rs:199` |

### `root_seam::agents` — 8 refs → tracedecay-agent-hosts

| Referenced item | Sites (file:line) |
| --- | --- |
| `agents::AgentIntegration` | `hermes.rs:400` |
| `agents::InstallContext` | `hermes.rs:400` |
| `agents::UpdatePluginOutcome` | `hermes.rs:400` |
| `agents::expected_tool_perms` | `hermes.rs:1355` |
| `agents::hermes::HermesIntegration` | `hermes.rs:399` |
| `agents::hermes::read_config_pinned_project_root` | `hermes.rs:154`, `hermes.rs:313`, `hermes/resolution.rs:194` |

### `root_seam::tracedecay` — 8 refs → unassigned (root facade `src/tracedecay.rs`)

| Referenced item | Sites (file:line) |
| --- | --- |
| `tracedecay::TraceDecay` | `memory_cutover.rs:28` |
| `tracedecay::TraceDecay::resolve_store_layout_for_identity` | `hermes/pipeline.rs:220` |
| `tracedecay::TraceDecayOpenOptions` | `consolidate/tests.rs:39`, `memory_cutover.rs:28` |
| `tracedecay::current_timestamp` | `consolidate/finalize.rs:124`, `manifest/runtime.rs:198`, `memory_cutover.rs:853`, `registry.rs:777` |

### `root_seam::branch_meta` — 7 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `branch_meta::(whole module)` | `memory_cutover.rs:21`, `registry.rs:7` |
| `branch_meta::BranchEntry` | `consolidate/mod.rs:45`, `consolidate/prepare.rs:10` |
| `branch_meta::BranchMeta` | `consolidate/mod.rs:45` |
| `branch_meta::load_branch_meta` | `inventory/project.rs:361` |
| `branch_meta::self` | `consolidate/mod.rs:45` |

### `root_seam::memory` — 7 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `memory::store::MemoryStore` | `consolidate/sqlite.rs:10`, `consolidate/tests.rs:34`, `hermes.rs:405`, `hermes/memory.rs:15` |
| `memory::types` | `consolidate/tests.rs:35`, `hermes.rs:406` |
| `memory::user::user_memory_db_path` | `hermes.rs:1953` |

### `root_seam::open_store_holders` — 7 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `open_store_holders::OpenStoreHolderScan` | `consolidate/preflight.rs:50`, `consolidate/preflight.rs:201` |
| `open_store_holders::OpenStoreHolderScan::Supported` | `consolidate/preflight.rs:45`, `consolidate/preflight.rs:53`, `consolidate/preflight.rs:58` |
| `open_store_holders::OpenStoreHolderScan::Unsupported` | `consolidate/preflight.rs:86` |
| `open_store_holders::scan` | `consolidate/preflight.rs:35` |

### `root_seam::application` — 6 refs → tracedecay-application

| Referenced item | Sites (file:line) |
| --- | --- |
| `application::host_admission` | `consolidate/tests.rs:28`, `consolidate/tests/observation.rs:5`, `hermes.rs:401` |
| `application::host_admission::HostAdmissionAuthorities::for_project` | `hermes/pipeline.rs:554` |
| `application::host_admission::HostAdmissionFacade::new` | `hermes/pipeline.rs:553` |
| `application::session::compatibility::projected_content_hash` | `consolidate/tests/session_merge.rs:462` |

### `root_seam::worktree` — 5 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `worktree::git_common_dir` | `consolidate/mod.rs:436`, `consolidate/mod.rs:685`, `consolidate/tests.rs:880`, `consolidate/tests.rs:935` |
| `worktree::git_worktree_root` | `hermes/resolution.rs:60` |

### `root_seam::git` — 1 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `git::git_program` | `consolidate/tests.rs:1367` |

