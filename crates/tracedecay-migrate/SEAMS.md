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
| `storage::BRANCH_META_FILENAME` | `hermes.rs:1779`, `hermes.rs:1840`, `inventory/project.rs:11` |
| `storage::EnrollmentMarker` | `consolidate/mod.rs:50`, `hermes.rs:913`, `hermes/pipeline.rs:207`, `manifest/runtime.rs:13` |
| `storage::PrivateStoreIo` | `consolidate/mod.rs:50`, `manifest/runtime.rs:13` |
| `storage::PrivateStoreIo::create_dir_all` | `hermes.rs:448`, `hermes.rs:1425` |
| `storage::ProjectStorageLocation` | `registry.rs:13` |
| `storage::SESSIONS_DB_FILENAME` | `consolidate/sqlite/verify.rs:28`, `hermes.rs:1768`, `hermes.rs:1778`, `hermes.rs:1829`, `hermes.rs:1839`, `hermes/candidates.rs:79`, `hermes/candidates.rs:115`, `inventory/project.rs:11` |
| `storage::STORE_MANIFEST_FILENAME` | `hermes.rs:1782`, `hermes.rs:1843`, `hermes/candidates.rs:105`, `inventory/project.rs:11`, `manifest/runtime.rs:13`, `profile_backup.rs:453`, `profile_backup.rs:460`, `profile_backup.rs:588`, `profile_backup.rs:1017`, `profile_backup.rs:1035`, `profile_backup.rs:1046`, `profile_backup.rs:1131`, `profile_backup.rs:1195`, `registry.rs:13` |
| `storage::STORE_MANIFEST_SCHEMA_VERSION` | `hermes.rs:1771`, `hermes.rs:1832`, `profile_backup.rs:508`, `profile_backup.rs:1006`, `profile_backup.rs:1133`, `registry.rs:13` |
| `storage::StorageMode` | `consolidate/mod.rs:50`, `manifest/runtime.rs:13`, `registry.rs:13` |
| `storage::StorageMode::ProfileSharded` | `hermes.rs:915`, `hermes.rs:1774`, `hermes.rs:1835`, `hermes/pipeline.rs:209`, `inventory/project.rs:129`, `profile_backup.rs:511`, `profile_backup.rs:1009`, `profile_backup.rs:1136` |
| `storage::StoreKind` | `consolidate/mod.rs:50`, `manifest/runtime.rs:13`, `registry.rs:13` |
| `storage::StoreKind::CodeProject` | `hermes.rs:1773`, `hermes.rs:1834`, `profile_backup.rs:510`, `profile_backup.rs:1008`, `profile_backup.rs:1135` |
| `storage::StoreLayout` | `consolidate/mod.rs:50`, `hermes/pipeline.rs:164` |
| `storage::StoreManifest` | `consolidate/mod.rs:50`, `hermes.rs:1770`, `hermes.rs:1831`, `profile_backup.rs:503`, `profile_backup.rs:507`, `profile_backup.rs:1005`, `profile_backup.rs:1132`, `registry.rs:1476` |
| `storage::classify_registry_storage` | `inventory/scan.rs:161` |
| `storage::default_profile_project_id` | `hermes.rs:906` |
| `storage::default_profile_root` | `hermes.rs:75`, `hermes.rs:125`, `hermes.rs:971`, `hermes/pipeline.rs:214`, `inventory/scan.rs:19` |
| `storage::default_profile_sharded_layout` | `hermes.rs:1656` |
| `storage::has_sqlite_database_header` | `manifest/runtime.rs:13` |
| `storage::profile_sharded_data_root` | `manifest/runtime.rs:13` |
| `storage::profile_sharded_layout` | `hermes/pipeline.rs:204`, `manifest/runtime.rs:13` |
| `storage::read_enrollment_marker` | `inventory/project.rs:127`, `manifest/runtime.rs:13`, `registry.rs:13` |
| `storage::read_repository_identity_marker` | `registry.rs:13` |
| `storage::read_store_manifest` | `hermes/candidates.rs:106`, `manifest/runtime.rs:13`, `profile_backup.rs:476`, `profile_backup.rs:490`, `profile_backup.rs:557`, `profile_backup.rs:565`, `profile_backup.rs:1034`, `profile_backup.rs:1045`, `registry.rs:13` |
| `storage::resolve_layout` | `hermes.rs:818`, `hermes.rs:1060`, `hermes.rs:1129`, `hermes.rs:1225`, `hermes.rs:1426`, `hermes.rs:1578`, `hermes.rs:1801`, `hermes.rs:2084`, `hermes.rs:2107`, `hermes.rs:2131`, `hermes.rs:2158`, `hermes/pipeline.rs:219`, `inventory/scan.rs:165` |
| `storage::resolve_persisted_layout` | `hermes/pipeline.rs:191` |
| `storage::self` | `consolidate/mod.rs:50` |
| `storage::try_acquire_sidecar_lock` | `inventory/project.rs:290` |
| `storage::try_classify_project_storage_with_registry` | `registry.rs:13` |
| `storage::validate_project_id` | `manifest/runtime.rs:13`, `registry.rs:13` |
| `storage::write_enrollment_marker` | `hermes.rs:911` |
| `storage::write_store_manifest` | `manifest/runtime.rs:13` |
| `storage::write_store_manifest_to_path` | `profile_backup.rs:488`, `profile_backup.rs:1016`, `profile_backup.rs:1130` |

### `root_seam::db` — 84 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `db::Database` | `consolidate/runtime.rs:19`, `consolidate/sqlite.rs:7`, `consolidate/tests.rs:30`, `hermes/memory.rs:13`, `memory_cutover.rs:961` |
| `db::DatabaseAccessMode` | `consolidate/runtime.rs:19` |
| `db::DatabaseAuthority` | `consolidate/runtime.rs:19`, `consolidate/tests.rs:30`, `manifest/runtime.rs:431`, `manifest/runtime.rs:857`, `manifest/runtime.rs:1159`, `manifest/runtime.rs:1399`, `memory_cutover.rs:961` |
| `db::DatabaseAuthority::for_runtime` | `consolidate/sqlite.rs:1571`, `inventory/sqlite.rs:20`, `inventory/sqlite.rs:154`, `manifest/runtime.rs:409`, `manifest/runtime.rs:881`, `manifest/runtime.rs:1002` |
| `db::DatabaseAuthority::replace_file_atomically` | `manifest/runtime.rs:890`, `manifest/runtime.rs:992` |
| `db::DatabaseAuthorityRole::Test` | `consolidate/sqlite.rs:1575` |
| `db::DatabaseDeletionFence::acquire` | `memory_cutover.rs:1068` |
| `db::MaintenanceDatabaseScope` | `consolidate/mod.rs:1956`, `consolidate/runtime.rs:19` |
| `db::MemoryV2ArchiveDatabase` | `consolidate/sqlite/memory_v2.rs:2`, `memory_cutover.rs:23` |
| `db::TestDatabaseRuntimeMode` | `consolidate/tests/temporal.rs:55`, `memory_cutover.rs:961` |
| `db::TestDatabaseRuntimeMode::Existing` | `consolidate/sqlite.rs:1579`, `consolidate/tests.rs:64`, `consolidate/tests/temporal.rs:62` |
| `db::TestDatabaseRuntimeMode::Initialize` | `consolidate/tests.rs:53`, `consolidate/tests/temporal.rs:56`, `consolidate/tests/temporal.rs:75`, `consolidate/tests/temporal.rs:84` |
| `db::TestDatabaseRuntimeMode::ReadOnly` | `consolidate/tests.rs:75` |
| `db::engine` | `consolidate/sqlite.rs:8`, `consolidate/sqlite/inspect.rs:10`, `consolidate/sqlite/observation.rs:4`, `consolidate/sqlite/temporal.rs:6`, `consolidate/tests.rs:29`, `hermes.rs:401`, `hermes/copy.rs:10`, `hermes/fingerprint.rs:9`, `hermes/memory.rs:14`, `hermes/pipeline.rs:14`, `hermes/resolution.rs:7`, `inventory/sqlite.rs:3`, `registry.rs:8` |
| `db::engine::Connection` | `hermes/memory.rs:751` |
| `db::engine::DatabaseAttachmentExecutor` | `consolidate/tests/session_merge.rs:5` |
| `db::engine::Executor` | `consolidate/sqlite/external_source.rs:8`, `consolidate/sqlite/memory_v2.rs:1`, `consolidate/sqlite/projection.rs:1`, `consolidate/sqlite/verify.rs:3` |
| `db::engine::QueryExecutor` | `memory_cutover.rs:22` |
| `db::engine::TestConnection::open` | `consolidate/tests/temporal.rs:57` |
| `db::engine::TransactionBehavior::Immediate` | `hermes.rs:656`, `hermes/memory.rs:761` |
| `db::engine::Value` | `hermes/session_merge.rs:6` |
| `db::engine::Value::Text` | `registry.rs:1166`, `registry.rs:1181` |
| `db::engine::params` | `consolidate/mod.rs:28`, `memory_cutover.rs:919`, `memory_cutover.rs:1181`, `registry/tests.rs:12` |
| `db::enter_maintenance_database_scope` | `consolidate/mod.rs:208`, `consolidate/mod.rs:251`, `hermes.rs:207`, `inventory/scan.rs:24`, `manifest/runtime.rs:395`, `manifest/runtime.rs:605`, `manifest/runtime.rs:676`, `memory_cutover.rs:178` |
| `db::export_memory_v2_owner_archive` | `consolidate/sqlite/memory_v2.rs:2`, `memory_cutover.rs:23` |
| `db::import_memory_v2_owner_archive` | `consolidate/sqlite/memory_v2.rs:2` |
| `db::is_lock_contended` | `inventory/project.rs:293` |
| `db::list_memory_v2_archive_owners` | `consolidate/sqlite/memory_v2.rs:2`, `memory_cutover.rs:23` |
| `db::migrations::migrate_connection` | `hermes.rs:454` |
| `db::plan_memory_v2_owner_archive_import` | `consolidate/sqlite/memory_v2.rs:2` |
| `db::sqlite_generation_identity` | `consolidate/runtime.rs:943` |

### `root_seam::sqlite_read_snapshot` — 49 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `sqlite_read_snapshot::SnapshotAttachToken` | `consolidate/sqlite.rs:1506` |
| `sqlite_read_snapshot::SnapshotConnection` | `hermes/pipeline.rs:17`, `hermes/session_merge.rs:8` |
| `sqlite_read_snapshot::SnapshotDatabase` | `consolidate/sqlite.rs:223`, `consolidate/sqlite/inspect.rs:436`, `hermes/pipeline.rs:17` |
| `sqlite_read_snapshot::SnapshotSet` | `consolidate/evidence.rs:6`, `consolidate/mod.rs:850`, `consolidate/sqlite/inspect.rs:245`, `consolidate/sqlite/inspect.rs:266`, `consolidate/sqlite/inspect.rs:302`, `consolidate/sqlite/inspect.rs:336`, `consolidate/sqlite/inspect.rs:373`, `consolidate/sqlite/inspect.rs:434`, `consolidate/sqlite/verify.rs:20`, `consolidate/sqlite/verify.rs:23` |
| `sqlite_read_snapshot::SnapshotSet::capture` | `consolidate/sqlite/inspect.rs:238` |
| `sqlite_read_snapshot::SnapshotSet::capture_in` | `consolidate/evidence.rs:144`, `consolidate/finalize.rs:75`, `consolidate/finalize.rs:85` |
| `sqlite_read_snapshot::SourceGeneration` | `consolidate/evidence.rs:13` |
| `sqlite_read_snapshot::checkpointed_database_has_any_rows` | `memory_cutover.rs:678`, `memory_cutover.rs:691` |
| `sqlite_read_snapshot::family_fingerprint` | `consolidate/evidence.rs:60`, `consolidate/evidence.rs:109`, `consolidate/evidence.rs:151`, `consolidate/evidence.rs:183` |
| `sqlite_read_snapshot::open` | `hermes.rs:650`, `hermes.rs:771`, `hermes.rs:778`, `hermes/pipeline.rs:625` |
| `sqlite_read_snapshot::open_in` | `consolidate/evidence.rs:173`, `consolidate/finalize.rs:26`, `consolidate/sqlite.rs:833`, `consolidate/sqlite.rs:880`, `consolidate/sqlite.rs:1563`, `consolidate/tests/memory.rs:60`, `consolidate/tests/temporal.rs:91`, `hermes/pipeline.rs:399`, `hermes/pipeline.rs:589`, `inventory/sqlite.rs:32`, `inventory/sqlite.rs:167`, `manifest/runtime.rs:868`, `manifest/runtime.rs:1404`, `memory_cutover.rs:123`, `memory_cutover.rs:233`, `memory_cutover.rs:440`, `memory_cutover.rs:476`, `memory_cutover.rs:1196` |

### `root_seam::global_db` — 47 refs → tracedecay-global-db

| Referenced item | Sites (file:line) |
| --- | --- |
| `global_db::(whole module)` | `inventory/scan.rs:11` |
| `global_db::CodeProjectRecord` | `registry.rs:9`, `registry/tests.rs:220` |
| `global_db::GraphScopeUpsert` | `consolidate/mod.rs:47`, `registry.rs:9`, `registry/tests.rs:14` |
| `global_db::ProjectAliasRecord` | `registry/tests.rs:220` |
| `global_db::ProjectRegistryContext` | `registry.rs:9`, `registry/tests.rs:220` |
| `global_db::ProjectStoreContext` | `registry/tests.rs:220` |
| `global_db::RegisteredGlobalDb` | `consolidate/mod.rs:47`, `consolidate/sqlite.rs:35`, `consolidate/tests.rs:31`, `consolidate/tests/observation.rs:6`, `hermes.rs:12`, `hermes/pipeline.rs:15`, `hermes/resolution.rs:8`, `hermes/session_merge.rs:7`, `inventory/sqlite.rs:4`, `registry.rs:9`, `registry/tests.rs:14` |
| `global_db::RegisteredGlobalDbWriteTransaction` | `hermes/session_merge.rs:7`, `registry.rs:1152` |
| `global_db::StoreArtifactUpsert` | `consolidate/mod.rs:47`, `registry.rs:9` |
| `global_db::StoreInstanceRecord` | `registry/tests.rs:220` |
| `global_db::StoreInstanceUpsert` | `consolidate/mod.rs:47`, `registry.rs:9` |
| `global_db::ensure_registered_schema` | `consolidate/tests/temporal.rs:58`, `hermes.rs:457` |
| `global_db::observation::OBSERVATION_ANCHOR_SCHEMA_MIGRATION` | `consolidate/sqlite/observation.rs:93`, `consolidate/sqlite/observation.rs:114`, `consolidate/tests/observation.rs:111`, `consolidate/tests/observation.rs:142` |
| `global_db::observation::OBSERVATION_PROVENANCE_SCHEMA_MIGRATION` | `consolidate/sqlite/observation.rs:94`, `consolidate/sqlite/observation.rs:115`, `consolidate/tests/observation.rs:119`, `consolidate/tests/observation.rs:143` |
| `global_db::repair_session_temporal_store` | `registry.rs:247` |
| `global_db::schema_stages::begin_observation_authority_canonical_repair` | `consolidate/sqlite/observation.rs:605`, `consolidate/tests/observation.rs:782` |
| `global_db::schema_stages::finish_observation_authority_canonical_repair` | `consolidate/sqlite/observation.rs:624`, `consolidate/tests/observation.rs:812` |
| `global_db::schema_stages::validate_observation_authority_connection` | `consolidate/sqlite/observation.rs:135`, `consolidate/tests/observation.rs:1334` |
| `global_db::self` | `inventory/sqlite.rs:4` |
| `global_db::tests::harness::RegisteredGlobalDbHarness` | `registry/tests.rs:13` |

### `root_seam::errors` — 43 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `errors::Result` | `consolidate/files.rs:10`, `consolidate/mod.rs:46`, `consolidate/runtime.rs:20`, `consolidate/sqlite.rs:9`, `consolidate/sqlite/external_source.rs:11`, `consolidate/sqlite/inspect.rs:11`, `consolidate/sqlite/memory_v2.rs:7`, `consolidate/sqlite/observation.rs:11`, `consolidate/sqlite/projection.rs:4`, `consolidate/sqlite/temporal.rs:12`, `consolidate/sqlite/verify.rs:10`, `hermes/pipeline.rs:165`, `hermes/pipeline.rs:181`, `inventory/hermes.rs:10`, `inventory/project.rs:10`, `inventory/scan.rs:10`, `memory_cutover.rs:26`, `registry.rs:123`, `registry.rs:152`, `registry.rs:166`, `registry.rs:176`, `registry.rs:192`, `registry.rs:219`, `registry.rs:232`, `registry.rs:1045`, `registry.rs:1111`, `registry.rs:1155` |
| `errors::TraceDecayError` | `consolidate/mod.rs:46`, `consolidate/runtime.rs:20`, `consolidate/sqlite.rs:9`, `consolidate/sqlite/inspect.rs:222`, `memory_cutover.rs:26` |
| `errors::TraceDecayError::Config` | `hermes/pipeline.rs:167`, `hermes/pipeline.rs:194`, `inventory/scan.rs:59`, `registry.rs:234`, `registry.rs:243`, `registry.rs:1116`, `registry.rs:1126` |
| `errors::TraceDecayError::Database` | `consolidate/tests/observation.rs:1337`, `registry.rs:126`, `registry.rs:134`, `registry.rs:142` |

### `root_seam::sessions` — 31 refs → tracedecay-sessions

| Referenced item | Sites (file:line) |
| --- | --- |
| `sessions::SessionMessageRecord` | `consolidate/tests.rs:36`, `hermes.rs:404` |
| `sessions::SessionRecord` | `consolidate/tests.rs:36`, `hermes.rs:404` |
| `sessions::git_correlation::ensure_git_correlation_schema_in_transaction` | `consolidate/sqlite.rs:806` |
| `sessions::hermes::ingest_legacy_pinned_profile` | `hermes/pipeline.rs:555` |
| `sessions::lcm::LCM_SCHEMA_VERSION` | `consolidate/sqlite.rs:865`, `consolidate/sqlite.rs:870`, `consolidate/tests/session_merge.rs:102`, `consolidate/tests/session_merge.rs:135`, `consolidate/tests/session_merge.rs:205`, `hermes.rs:2123`, `hermes/pipeline.rs:337`, `hermes/pipeline.rs:340` |
| `sessions::lcm::payload::expand_payload` | `consolidate/tests/session_merge.rs:649` |
| `sessions::lcm::payload::upsert_payload_metadata` | `consolidate/tests/session_merge.rs:591` |
| `sessions::lcm::payload::validate_payload_ref` | `consolidate/sqlite/verify.rs:656`, `hermes/copy.rs:587` |
| `sessions::lcm::payload::write_external_payload` | `consolidate/tests/session_merge.rs:575` |
| `sessions::lcm::schema::ensure_lcm_schema_in_transaction` | `consolidate/sqlite.rs:803` |
| `sessions::lcm::schema::rebuild_raw_fts` | `consolidate/sqlite.rs:738`, `consolidate/sqlite.rs:779` |
| `sessions::user_sessions_db_path` | `hermes.rs:1860`, `hermes.rs:1981`, `hermes.rs:2002`, `hermes.rs:2018`, `hermes.rs:2043`, `hermes.rs:2060`, `hermes.rs:2106`, `hermes/pipeline.rs:184` |
| `sessions::workflow_index::ensure_workflow_index_schema` | `consolidate/sqlite.rs:809` |

### `root_seam::lifecycle_lease` — 30 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `lifecycle_lease::LifecycleLease` | `consolidate/mod.rs:1955`, `consolidate/runtime.rs:214`, `consolidate/runtime.rs:579`, `hermes.rs:123`, `hermes.rs:204`, `manifest/runtime.rs:325`, `manifest/runtime.rs:332`, `manifest/runtime.rs:598`, `profile_backup.rs:116` |
| `lifecycle_lease::acquire_exclusive_for_profile` | `consolidate/mod.rs:204`, `consolidate/mod.rs:247`, `hermes.rs:181`, `hermes.rs:927`, `hermes.rs:972`, `inventory/scan.rs:20`, `manifest/runtime.rs:383`, `manifest/runtime.rs:586`, `manifest/runtime.rs:667`, `memory_cutover.rs:174`, `profile_backup.rs:957`, `profile_backup.rs:1026`, `profile_backup.rs:1078`, `profile_backup.rs:1096`, `profile_backup.rs:1146` |
| `lifecycle_lease::canonical_or_original` | `consolidate/mod.rs:939`, `consolidate/mod.rs:1193`, `consolidate/mod.rs:1213`, `consolidate/mod.rs:2296`, `consolidate/mod.rs:2297`, `inventory/project.rs:404` |

### `root_seam::config` — 26 refs → ROOT KEEPS `config` — see the note below

| Referenced item | Sites (file:line) |
| --- | --- |
| `config::DB_FILENAME` | `consolidate/files.rs:199`, `consolidate/finalize.rs:8`, `consolidate/finalize.rs:160`, `consolidate/mod.rs:2244`, `consolidate/tests/external_source.rs:160`, `consolidate/tests/external_source.rs:320`, `consolidate/tests/lifecycle.rs:353`, `consolidate/tests/lifecycle.rs:355`, `consolidate/tests/lifecycle.rs:402`, `consolidate/tests/lifecycle.rs:555`, `consolidate/tests/memory.rs:140`, `consolidate/tests/schema.rs:15`, `consolidate/tests/session_merge.rs:253`, `memory_cutover.rs:1166` |
| `config::PinnedUserDataDir::new` | `hermes.rs:960` |
| `config::TRACEDECAY_DIR` | `inventory/hermes.rs:9`, `inventory/project.rs:9`, `inventory/scan.rs:9` |
| `config::db_filename` | `hermes/candidates.rs:80`, `hermes/candidates.rs:116`, `inventory/project.rs:9`, `memory_cutover.rs:439` |
| `config::has_project_database` | `hermes/resolution.rs:63` |
| `config::is_generated_dir_segment` | `inventory/project.rs:411` |
| `config::self` | `inventory/project.rs:9` |
| `config::user_data_dir` | `consolidate/preflight.rs:21` |

### `root_seam::daemon` — 21 refs → tracedecay-daemon

| Referenced item | Sites (file:line) |
| --- | --- |
| `daemon::QuiescedDaemonLifecycle::acquire` | `hermes.rs:89` |
| `daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve` | `consolidate/mod.rs:1695`, `consolidate/mod.rs:1794` |
| `daemon::daemon_reachable` | `consolidate/preflight.rs:22` |
| `daemon::profile_identity::load_or_create` | `consolidate/mod.rs:275`, `consolidate/mod.rs:390`, `consolidate/mod.rs:1694`, `consolidate/mod.rs:1793`, `consolidate/mod.rs:1963`, `hermes.rs:218`, `inventory/scan.rs:29`, `registry.rs:153` |
| `daemon::store_runtime::registry` | `consolidate/runtime.rs:12` |
| `daemon::store_runtime::resolver::canonical_store_locator_digest` | `consolidate/runtime.rs:890` |
| `daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1` | `consolidate/mod.rs:387`, `hermes/pipeline.rs:13`, `registry.rs:114` |
| `daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open` | `consolidate/mod.rs:392`, `hermes.rs:228`, `inventory/scan.rs:31`, `registry.rs:155` |

### `root_seam::branch` — 13 refs → tracedecay-runtime-core (unassigned in the plan's kernel list)

| Referenced item | Sites (file:line) |
| --- | --- |
| `branch::BranchAdminAction` | `memory_cutover.rs:958` |
| `branch::BranchAdminOutcome` | `memory_cutover.rs:958` |
| `branch::BranchAdminReport` | `memory_cutover.rs:958` |
| `branch::current_branch` | `inventory/project.rs:358` |
| `branch::detect_default_branch` | `consolidate/mod.rs:772`, `consolidate/tests/lifecycle.rs:148`, `consolidate/tests/lifecycle.rs:201`, `consolidate/tests/lifecycle.rs:224` |
| `branch::gc_dead_branch_stores` | `consolidate/tests/lifecycle.rs:1115` |
| `branch::prepare_branch_admin_mutation` | `memory_cutover.rs:958` |
| `branch::resolve_branch_db_path` | `inventory/project.rs:360` |
| `branch::sanitize_branch_name` | `consolidate/mod.rs:822`, `consolidate/prepare.rs:199` |

### `root_seam::agents` — 8 refs → tracedecay-agent-hosts

| Referenced item | Sites (file:line) |
| --- | --- |
| `agents::AgentIntegration` | `hermes.rs:399` |
| `agents::InstallContext` | `hermes.rs:399` |
| `agents::UpdatePluginOutcome` | `hermes.rs:399` |
| `agents::expected_tool_perms` | `hermes.rs:1344` |
| `agents::hermes::HermesIntegration` | `hermes.rs:398` |
| `agents::hermes::read_config_pinned_project_root` | `hermes.rs:153`, `hermes.rs:312`, `hermes/resolution.rs:193` |

### `root_seam::tracedecay` — 8 refs → unassigned (root facade `src/tracedecay.rs`)

| Referenced item | Sites (file:line) |
| --- | --- |
| `tracedecay::TraceDecay` | `memory_cutover.rs:28` |
| `tracedecay::TraceDecay::resolve_store_layout_for_identity` | `hermes/pipeline.rs:217` |
| `tracedecay::TraceDecayOpenOptions` | `consolidate/tests.rs:37`, `memory_cutover.rs:28` |
| `tracedecay::current_timestamp` | `consolidate/finalize.rs:123`, `manifest/runtime.rs:201`, `memory_cutover.rs:854`, `registry.rs:773` |

### `root_seam::branch_meta` — 7 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `branch_meta::(whole module)` | `memory_cutover.rs:21`, `registry.rs:7` |
| `branch_meta::BranchEntry` | `consolidate/mod.rs:45`, `consolidate/prepare.rs:10` |
| `branch_meta::BranchMeta` | `consolidate/mod.rs:45` |
| `branch_meta::load_branch_meta` | `inventory/project.rs:359` |
| `branch_meta::self` | `consolidate/mod.rs:45` |

### `root_seam::memory` — 7 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `memory::store::MemoryStore` | `consolidate/sqlite.rs:10`, `consolidate/tests.rs:32`, `hermes.rs:402`, `hermes/memory.rs:15` |
| `memory::types` | `consolidate/tests.rs:33`, `hermes.rs:403` |
| `memory::user::user_memory_db_path` | `hermes.rs:1939` |

### `root_seam::open_store_holders` — 7 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `open_store_holders::OpenStoreHolderScan` | `consolidate/preflight.rs:46`, `consolidate/preflight.rs:196` |
| `open_store_holders::OpenStoreHolderScan::Supported` | `consolidate/preflight.rs:41`, `consolidate/preflight.rs:48`, `consolidate/preflight.rs:53` |
| `open_store_holders::OpenStoreHolderScan::Unsupported` | `consolidate/preflight.rs:81` |
| `open_store_holders::scan` | `consolidate/preflight.rs:33` |

### `root_seam::application` — 6 refs → tracedecay-application

| Referenced item | Sites (file:line) |
| --- | --- |
| `application::host_admission` | `consolidate/tests.rs:28`, `consolidate/tests/observation.rs:5`, `hermes.rs:400` |
| `application::host_admission::HostAdmissionAuthorities::for_project` | `hermes/pipeline.rs:548` |
| `application::host_admission::HostAdmissionFacade::new` | `hermes/pipeline.rs:547` |
| `application::session::compatibility::projected_content_hash` | `consolidate/tests/session_merge.rs:462` |

### `root_seam::worktree` — 5 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `worktree::git_common_dir` | `consolidate/mod.rs:435`, `consolidate/mod.rs:684`, `consolidate/tests.rs:878`, `consolidate/tests.rs:933` |
| `worktree::git_worktree_root` | `hermes/resolution.rs:60` |

### `root_seam::git` — 1 refs → tracedecay-runtime-core

| Referenced item | Sites (file:line) |
| --- | --- |
| `git::git_program` | `consolidate/tests.rs:1365` |

