//! Session-temporal (generation, refresh, receipts, forward-migration)
//! consolidation merge and rollback tests.

use super::*;

struct TemporalOwnedSessionFixture {
    runtime: HostAdmissionTestRuntimeV1,
    path: PathBuf,
    project_id: ProjectId,
    project_path: PathBuf,
}

impl TemporalOwnedSessionFixture {
    async fn new(root: &Path, name: &str) -> Self {
        let profile = root.join(format!("{name}-profile"));
        let project = root.join(format!("{name}-project"));
        fs::create_dir_all(&project).unwrap();
        let project_id = ProjectId::new(format!("project.temporal.{name}")).unwrap();
        let runtime = HostAdmissionTestRuntimeV1::project(&profile, &project, project_id.clone())
            .await
            .unwrap();
        let path = runtime
            .database_path(HostAdmissionScope::Project)
            .unwrap()
            .to_path_buf();
        Self {
            runtime,
            path,
            project_id,
            project_path: project,
        }
    }

    fn database(&self) -> &RegisteredGlobalDb {
        self.runtime
            .registered_database(HostAdmissionScope::Project)
            .unwrap()
    }

    async fn upsert_session(&self, session: &SessionRecord) -> bool {
        let mut session = session.clone();
        session.project_key = self.project_id.as_str().to_owned();
        session.project_path = self.project_path.to_string_lossy().into_owned();
        self.runtime
            .upsert_session_for_test(HostAdmissionScope::Project, &session)
            .await
            .unwrap()
    }

    async fn checkpoint(&self) {
        self.database().checkpoint().await;
    }
}

async fn temporal_database(
    path: &Path,
    mode: tracedecay_runtime_core::db::TestDatabaseRuntimeMode,
) -> Database {
    register_test_schema_installer();
    let runtime_mode = if mode == tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize {
        let seed = tracedecay_runtime_core::db::engine::TestConnection::open(path);
        crate::root_seam::global_db::ensure_registered_schema(&seed)
            .await
            .expect("initialize temporal fixture through production migrations");
        drop(seed);
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Existing
    } else {
        mode
    };
    let authority =
        DatabaseAuthority::acquire_test(path, "temporal consolidation fixture").unwrap();
    Database::publish_test_runtime(path, &authority, runtime_mode)
        .await
        .unwrap()
        .0
}

async fn temporal_execute_batch(path: &Path, sql: &str) {
    let db = temporal_database(
        path,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    db.execute_write_batch("seed temporal consolidation fixture", sql)
        .await
        .unwrap();
    db.checkpoint().await.unwrap();
    db.close();
}

async fn temporal_initialize(path: &Path) {
    temporal_database(
        path,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .close();
}

async fn temporal_scalar(path: &Path, sql: &str) -> i64 {
    let scratch_root = path.parent().unwrap();
    let snapshot = tracedecay_runtime_core::sqlite_read_snapshot::open_in(path, scratch_root)
        .await
        .unwrap();
    let mut rows = snapshot.connection().query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

async fn temporal_registered_scalar(db: &RegisteredGlobalDb, sql: &str) -> i64 {
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

#[tokio::test]
async fn temporal_key_rotation_merges_as_an_idempotent_prefix_union() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    temporal_execute_batch(
        &target,
        "INSERT INTO session_query_cursor_keys(
             key_id, key_version, key_material, created_at, retired_at
         ) VALUES (
             'key-v1', 1,
             X'0101010101010101010101010101010101010101010101010101010101010101',
             10, NULL
         );",
    )
    .await;
    temporal_execute_batch(
        &source,
        "INSERT INTO session_query_cursor_keys(
             key_id, key_version, key_material, created_at, retired_at
         ) VALUES (
             'key-v1', 1,
             X'0101010101010101010101010101010101010101010101010101010101010101',
             10, NULL
         );
         INSERT INTO session_query_cursor_keys(
             key_id, key_version, key_material, created_at, retired_at
         ) VALUES (
             'key-v2', 2,
             X'0202020202020202020202020202020202020202020202020202020202020202',
             20, NULL
         );",
    )
    .await;

    sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap();
    sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap();

    assert_eq!(
        temporal_scalar(&target, "SELECT COUNT(*) FROM session_query_cursor_keys").await,
        2
    );
    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT retired_at FROM session_query_cursor_keys WHERE key_version = 1"
        )
        .await,
        20
    );
}

#[tokio::test]
async fn temporal_key_collision_rolls_back_without_reporting_key_material() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    temporal_execute_batch(
        &target,
        "INSERT INTO session_query_cursor_keys(
             key_id, key_version, key_material, created_at, retired_at
         ) VALUES (
             'key-v1', 1,
             X'DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF',
             10, NULL
         );",
    )
    .await;
    temporal_execute_batch(
        &source,
        "INSERT INTO session_query_cursor_keys(
             key_id, key_version, key_material, created_at, retired_at
         ) VALUES (
             'key-v1', 1,
             X'CAFEBABECAFEBABECAFEBABECAFEBABECAFEBABECAFEBABECAFEBABECAFEBABE',
             10, NULL
         );",
    )
    .await;

    let error = sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap_err();
    let report = error.to_string();
    assert!(report.contains("cursor key"));
    assert!(!report.contains("DEADBEEF"));
    assert!(!report.contains("CAFEBABE"));
    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM session_query_cursor_keys
             WHERE key_material =
               X'DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_generation_replays_only_legal_state_transitions() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    let building = "INSERT INTO session_temporal_generations(
             session_id, generation, state, frozen_watermarks_json, created_at,
             ready_at, activated_at, completed_at
         ) VALUES ('session-a', 1, 'building', '{}', 10, NULL, NULL, NULL);";
    temporal_execute_batch(&target, building).await;
    temporal_execute_batch(
        &source,
        &format!(
            "{building}
             UPDATE session_temporal_generations
             SET state = 'ready', ready_at = 20
             WHERE session_id = 'session-a' AND generation = 1;
             UPDATE session_temporal_generations
             SET state = 'active', activated_at = 30
             WHERE session_id = 'session-a' AND generation = 1;"
        ),
    )
    .await;

    sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap();
    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM session_temporal_generations
             WHERE session_id = 'session-a' AND generation = 1
               AND state = 'active' AND ready_at = 20 AND activated_at = 30"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_generation_branch_conflict_rolls_back() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    let setup = |terminal: &str| {
        format!(
            "INSERT INTO session_temporal_generations(
                 session_id, generation, state, frozen_watermarks_json, created_at,
                 ready_at, activated_at, completed_at
             ) VALUES ('session-conflict', 1, 'building', '{{}}', 10, NULL, NULL, NULL);
             UPDATE session_temporal_generations
             SET state = '{terminal}', completed_at = 20
             WHERE session_id = 'session-conflict' AND generation = 1;"
        )
    };
    temporal_execute_batch(&target, &setup("failed")).await;
    temporal_execute_batch(&source, &setup("cancelled")).await;

    let error = sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("generation lifecycle conflict"));
    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM session_temporal_generations
             WHERE session_id = 'session-conflict' AND state = 'failed'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_refresh_replays_running_to_terminal() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    let digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    let config = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    let running = format!(
        "INSERT INTO session_refresh_operations(
             session_id, operation_id, request_digest, target_frontier_json,
             state, created_at, updated_at, terminal_at, failure_code
         ) VALUES (
             'session-refresh', 'operation-1', '{digest}',
             '{{\"observed_through\":1,\"committed_through\":0}}',
             'running', 10, 10, NULL, NULL
         );
         INSERT INTO session_temporal_generations(
             session_id, generation, state, frozen_watermarks_json, created_at,
             ready_at, activated_at, completed_at
         ) VALUES ('session-refresh', 1, 'building', '{{}}', 10, NULL, NULL, NULL);
         INSERT INTO session_refresh_bindings(
             session_id, operation_id, scope_kind, source_frontier, target_frontier,
             projector_version, config_digest, generation, frozen_watermarks_json,
             binding_digest, created_at
         ) VALUES (
             'session-refresh', 'operation-1', 'session_store', 0, 1,
             'session-temporal-projector.v1', '{config}', 1, '{{}}', '{digest}', 10
         );"
    );
    temporal_execute_batch(&target, &running).await;
    temporal_execute_batch(
        &source,
        &format!(
            "{running}
             INSERT INTO session_refresh_progress(
                 session_id, operation_id, progress_ordinal, frontier_json,
                 coverage_json, committed_batches, committed_records, recorded_at
             ) VALUES (
                 'session-refresh', 'operation-1', 0,
                 '{{\"observed_through\":1,\"committed_through\":0}}',
                 '{{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}}',
                 0, 0, 15
             );
             UPDATE session_temporal_generations
             SET state = 'cancelled', completed_at = 20
             WHERE session_id = 'session-refresh' AND generation = 1;
             UPDATE session_refresh_operations
             SET state = 'cancelled', updated_at = 20, terminal_at = 20
             WHERE session_id = 'session-refresh' AND operation_id = 'operation-1';
             INSERT INTO session_refresh_receipts(
                 session_id, operation_id, terminal_state, frontier_json,
                 coverage_json, failure_code, terminal_at
             ) VALUES (
                 'session-refresh', 'operation-1', 'cancelled',
                 '{{\"observed_through\":1,\"committed_through\":0}}',
                 '{{\"visible\":0,\"hidden\":0,\"unknown\":0,\"redacted\":0}}',
                 NULL, 20
             );"
        ),
    )
    .await;

    sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap();
    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM session_refresh_operations
             WHERE session_id = 'session-refresh' AND operation_id = 'operation-1'
               AND state = 'cancelled' AND terminal_at = 20"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_refresh_running_conflict_rolls_back() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    let setup = |operation: &str, generation: i64, digest_byte: char| {
        let digest = format!("sha256:{}", digest_byte.to_string().repeat(64));
        format!(
            "INSERT INTO session_refresh_operations(
                 session_id, operation_id, request_digest, target_frontier_json,
                 state, created_at, updated_at, terminal_at, failure_code
             ) VALUES (
                 'session-refresh', '{operation}', '{digest}',
                 '{{\"observed_through\":1,\"committed_through\":0}}',
                 'running', 10, 10, NULL, NULL
             );
             INSERT INTO session_temporal_generations(
                 session_id, generation, state, frozen_watermarks_json, created_at,
                 ready_at, activated_at, completed_at
             ) VALUES (
                 'session-refresh', {generation}, 'building', '{{}}',
                 10, NULL, NULL, NULL
             );
             INSERT INTO session_refresh_bindings(
                 session_id, operation_id, scope_kind, source_frontier, target_frontier,
                 projector_version, config_digest, generation, frozen_watermarks_json,
                 binding_digest, created_at
             ) VALUES (
                 'session-refresh', '{operation}', 'session_store', 0, 1,
                 'session-temporal-projector.v1',
                 'sha256:2222222222222222222222222222222222222222222222222222222222222222',
                 {generation}, '{{}}', '{digest}', 10
             );"
        )
    };
    temporal_execute_batch(&target, &setup("operation-target", 1, '3')).await;
    temporal_execute_batch(&source, &setup("operation-source", 2, '4')).await;

    let error = sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("refresh operation running-state conflict")
    );
    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM session_refresh_operations
             WHERE session_id = 'session-refresh'
               AND operation_id = 'operation-target' AND state = 'running'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_projection_receipt_digest_collision_rolls_back() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    let setup = |digest: &str| {
        format!(
            "INSERT INTO session_temporal_generations(
                 session_id, generation, state, frozen_watermarks_json, created_at,
                 ready_at, activated_at, completed_at
             ) VALUES ('session-r', 1, 'building', '{{}}', 10, NULL, NULL, NULL);
             INSERT INTO session_temporal_projection_receipts(
                 session_id, generation, batch_ordinal, batch_digest,
                 frozen_watermarks_json, source_through, projection_through,
                 occurrence_count, occurrence_digest, dimension_count, dimension_digest,
                 copy_count, copy_digest, assertion_count, assertion_digest,
                 supersession_count, supersession_digest, current_count, current_digest,
                 fts_count, fts_digest, committed_at
             ) VALUES (
                 'session-r', 1, 0, '{digest}', '{{}}', 1, 1,
                 0, 'occ', 0, 'dim', 0, 'copy', 0, 'assert',
                 0, 'super', 0, 'current', 0, 'fts', 20
             );"
        )
    };
    temporal_execute_batch(&target, &setup("digest-target")).await;
    temporal_execute_batch(&source, &setup("digest-source")).await;

    let error = sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("projection receipt"));
    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts
             WHERE batch_digest = 'digest-target'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_summary_collision_rolls_back_without_reporting_text() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    let setup = |summary_text: &str| {
        format!(
            "INSERT INTO retrieval_anchors(
                 anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES ('summary-anchor', '{{}}', '{{}}', 'v1');
             INSERT INTO session_summary_nodes(
                 summary_id, session_id, summary_anchor_id, summary_text, index_text,
                 source_horizon_json, publication_json, created_at
             ) VALUES (
                 'summary-shared', 'session-summary', 'summary-anchor',
                 '{summary_text}', 'index', '{{}}', NULL, 10
             );"
        )
    };
    temporal_execute_batch(&target, &setup("private target summary")).await;
    temporal_execute_batch(&source, &setup("private source summary")).await;

    let error = sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap_err();
    let report = error.to_string();
    assert!(report.contains("summary node"));
    assert!(!report.contains("private target summary"));
    assert!(!report.contains("private source summary"));
    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM session_summary_nodes
             WHERE summary_text = 'private target summary'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_owner_bound_alias_collision_rolls_back_without_anchor_text() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    let setup = |anchor_id: &str, text: &str| {
        format!(
            "INSERT INTO retrieval_anchors(
                 anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES ('{anchor_id}', '{{\"text\":\"{text}\"}}', '{{\"owner\":\"same\"}}', 'v1');
             INSERT INTO retrieval_anchor_aliases(
                 owner_json, alias_kind, locator_digest, anchor_id
             ) VALUES ('{{\"owner\":\"same\"}}', '\"legacy\"', '\"digest\"', '{anchor_id}');"
        )
    };
    temporal_execute_batch(&target, &setup("anchor-target", "private-target")).await;
    temporal_execute_batch(&source, &setup("anchor-source", "private-source")).await;

    let error = sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap_err();
    let report = error.to_string();
    assert!(report.contains("retrieval anchor alias"));
    assert!(!report.contains("private-target"));
    assert!(!report.contains("private-source"));
    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM retrieval_anchor_aliases
             WHERE anchor_id = 'anchor-target'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_observation_effect_rebinds_to_destination_sequence() {
    let temp = TempDir::new().unwrap();
    let target = TemporalOwnedSessionFixture::new(temp.path(), "effect-target").await;
    let source = TemporalOwnedSessionFixture::new(temp.path(), "effect-source").await;
    let target_path = target.path.clone();
    let source_path = source.path.clone();
    let target_input_path = temp.path().join("target-input.db");
    let target_observation = migration_observation_for_scope(
        "session.temporal.target",
        ObservationScopeV1::Project {
            project_id: target.project_id.clone(),
        },
        "receipt.temporal.target",
        "message-temporal-target",
        "target observation",
    );
    let source_observation = migration_observation_for_scope(
        "session.temporal.source",
        ObservationScopeV1::Project {
            project_id: source.project_id.clone(),
        },
        "receipt.temporal.source",
        "message-temporal-source",
        "source observation",
    );
    let source_observation_id = source_observation.observation_id().as_str().to_owned();
    let source_receipt_id = source_observation
        .receipt()
        .receipt()
        .receipt_id()
        .as_str()
        .to_owned();

    Box::pin(persist_migration_observation(
        target.database(),
        target_observation,
        None,
    ))
    .await;
    target.checkpoint().await;
    Box::pin(persist_migration_observation(
        source.database(),
        source_observation,
        None,
    ))
    .await;
    source
        .database()
        .writer_connection()
        .unwrap()
        .execute(
            "INSERT INTO session_temporal_observation_effects(
                 observation_id, observation_sequence, session_id, receipt_id,
                 effect_digest, output_count, recorded_at
             ) SELECT observation_id, sequence, 'session-temporal',
                      ?2, 'effect-digest', 1, 30
               FROM observations WHERE observation_id = ?1",
            params![source_observation_id, source_receipt_id],
        )
        .await
        .unwrap();
    source.checkpoint().await;

    let offsets = sqlite::plan_session_offsets(&target_path, &source_path)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target_path, &target_input_path).unwrap();
    sqlite::merge_sessions(
        &target_path,
        &source_path,
        &target_input_path,
        "proj_source",
        &offsets,
    )
    .await
    .unwrap();

    assert_eq!(
        temporal_registered_scalar(
            target.database(),
            "SELECT COUNT(*)
             FROM session_temporal_observation_effects AS effect
             JOIN observations AS observation USING(observation_id)
             WHERE effect.observation_sequence = observation.sequence
               AND effect.observation_sequence = 2"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_migration_receipt_content_collision_rolls_back() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    let setup = |digest: &str| {
        format!(
            "INSERT INTO session_temporal_generations(
                 session_id, generation, state, frozen_watermarks_json, created_at,
                 ready_at, activated_at, completed_at
             ) VALUES ('session-mig', 1, 'building', '{{}}', 10, NULL, NULL, NULL);
             INSERT INTO session_temporal_migration_receipts(
                 session_id, generation, batch_ordinal, source_digest,
                 frozen_watermarks_json, imported_items, committed_at
             ) VALUES (
                 'session-mig', 1, 0, '{digest}', '{{}}', 1, 20
             );"
        )
    };
    temporal_execute_batch(
        &target,
        &setup("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
    )
    .await;
    temporal_execute_batch(
        &source,
        &setup("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    )
    .await;

    let error = sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("migration receipt"));
    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM session_temporal_migration_receipts
             WHERE source_digest =
               'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_migration_receipts_merge_idempotently_by_batch_ordinal() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    let shared = "INSERT INTO session_temporal_generations(
             session_id, generation, state, frozen_watermarks_json, created_at,
             ready_at, activated_at, completed_at
         ) VALUES ('session-mig', 1, 'building', '{}', 10, NULL, NULL, NULL);
         INSERT INTO session_temporal_migration_receipts(
             session_id, generation, batch_ordinal, source_digest,
             frozen_watermarks_json, imported_items, committed_at
         ) VALUES (
             'session-mig', 1, 0,
             'sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
             '{}', 1, 20
         );";
    temporal_execute_batch(&target, shared).await;
    temporal_execute_batch(
        &source,
        &format!(
            "{shared}
             INSERT INTO session_temporal_migration_receipts(
                 session_id, generation, batch_ordinal, source_digest,
                 frozen_watermarks_json, imported_items, committed_at
             ) VALUES (
                 'session-mig', 1, 1,
                 'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                 '{{}}', 2, 30
             );"
        ),
    )
    .await;

    sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap();
    sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap();

    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM session_temporal_migration_receipts
             WHERE session_id = 'session-mig'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn temporal_generation_merge_is_idempotent_on_full_remerge() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("target.db");
    let source = temp.path().join("source.db");
    // Receipts must be inserted while the generation is still building; the
    // insert-guard rejects checkpoints once the generation leaves building.
    let seed = |advance: bool| {
        let mut sql = String::from(
            "INSERT INTO session_temporal_generations(
                 session_id, generation, state, frozen_watermarks_json, created_at,
                 ready_at, activated_at, completed_at
             ) VALUES ('session-full', 1, 'building', '{}', 10, NULL, NULL, NULL);
             INSERT INTO session_temporal_projection_receipts(
                 session_id, generation, batch_ordinal, batch_digest,
                 frozen_watermarks_json, source_through, projection_through,
                 occurrence_count, occurrence_digest, dimension_count, dimension_digest,
                 copy_count, copy_digest, assertion_count, assertion_digest,
                 supersession_count, supersession_digest, current_count, current_digest,
                 fts_count, fts_digest, committed_at
             ) VALUES (
                 'session-full', 1, 0, 'digest-full', '{}', 1, 1,
                 0, 'occ', 0, 'dim', 0, 'copy', 0, 'assert',
                 0, 'super', 0, 'current', 0, 'fts', 40
             );",
        );
        if advance {
            sql.push_str(
                "UPDATE session_temporal_generations
                 SET state = 'ready', ready_at = 20
                 WHERE session_id = 'session-full' AND generation = 1;
                 UPDATE session_temporal_generations
                 SET state = 'active', activated_at = 30
                 WHERE session_id = 'session-full' AND generation = 1;",
            );
        }
        sql
    };
    temporal_execute_batch(&target, &seed(false)).await;
    temporal_execute_batch(&source, &seed(true)).await;

    sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap();
    sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap();

    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM session_temporal_generations
             WHERE session_id = 'session-full' AND state = 'active'"
        )
        .await,
        1
    );
    assert_eq!(
        temporal_scalar(
            &target,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts
             WHERE session_id = 'session-full'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_forward_migrates_eligible_legacy_sources_with_receipts() {
    let temp = TempDir::new().unwrap();
    let target_runtime = TemporalOwnedSessionFixture::new(temp.path(), "eligible-target").await;
    let target = target_runtime.path.clone();
    let empty = temp.path().join("empty.db");
    let observation = migration_observation_for_scope(
        "session.legacy.forward",
        ObservationScopeV1::Project {
            project_id: target_runtime.project_id.clone(),
        },
        "receipt.legacy.forward",
        "message.legacy.forward",
        "eligible legacy body",
    );
    let observation_id = observation.observation_id().as_str().to_owned();

    assert!(
        target_runtime
            .upsert_session(&SessionRecord {
                provider: "claude".to_string(),
                session_id: "session.legacy.forward".to_string(),
                project_key: "fixture".to_string(),
                project_path: "/fixture".to_string(),
                title: None,
                started_at: Some(1),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    let db = target_runtime.database();
    Box::pin(persist_migration_observation(db, observation, None)).await;
    assert_eq!(project_all_migration_observations(db).await, 1);

    // Projection materializes lcm_raw_messages + provenance; forward-migrate
    // binds those projected outputs into temporal sinks with receipts.
    let output_message_id = {
        let snapshot = db.read_snapshot().await.unwrap();
        let mut rows = QueryExecutor::query(
            &snapshot,
            "SELECT provenance.output_message_id
                 FROM observation_projection_provenance AS provenance
                 JOIN lcm_raw_messages AS raw
                   ON raw.provider = provenance.output_provider
                  AND raw.message_id = provenance.output_message_id
                 WHERE provenance.observation_id = ?1
                   AND raw.session_id = 'session.legacy.forward'
                   AND COALESCE(raw.legacy_source, 0) = 0
                   AND COALESCE(raw.legacy_truncated, 0) = 0
                 ORDER BY provenance.output_ordinal
                 LIMIT 1",
            params![observation_id],
        )
        .await
        .unwrap();
        let row = rows
            .next()
            .await
            .unwrap()
            .expect("projected observation must bind provenance to eligible lcm_raw_messages");
        row.get::<String>(0).unwrap()
    };
    target_runtime.checkpoint().await;

    temporal_initialize(&empty).await;

    sqlite::merge_temporal_for_test(&target, &empty)
        .await
        .unwrap();
    sqlite::merge_temporal_for_test(&target, &empty)
        .await
        .unwrap();

    let occurrence_sql = format!(
        "SELECT COUNT(*) FROM session_occurrences
         WHERE session_id = 'session.legacy.forward'
           AND message_id = '{}'",
        output_message_id.replace('\'', "''")
    );
    assert_eq!(
        temporal_registered_scalar(target_runtime.database(), &occurrence_sql).await,
        1
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_receipts
             WHERE session_id = 'session.legacy.forward' AND imported_items = 1"
        )
        .await,
        1
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_dispositions
             WHERE session_id = 'session.legacy.forward'
               AND disposition = 'eligible'"
        )
        .await,
        1
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_current_entities
             WHERE session_id = 'session.legacy.forward'
               AND entity_kind = 'occurrence_anchor'"
        )
        .await,
        1
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*)
             FROM session_occurrences AS occurrence
             JOIN session_occurrences_fts AS fts ON fts.rowid = occurrence.rowid
             WHERE occurrence.session_id = 'session.legacy.forward'
               AND fts.index_text = occurrence.index_text
               AND fts.snippet_text = occurrence.snippet_text"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_forward_migrate_skips_quarantined_legacy_sources() {
    let temp = TempDir::new().unwrap();
    let target_runtime = TemporalOwnedSessionFixture::new(temp.path(), "quarantined-target").await;
    let target = target_runtime.path.clone();
    let empty = temp.path().join("empty.db");
    assert!(
        target_runtime
            .upsert_session(&SessionRecord {
                provider: "claude".to_string(),
                session_id: "session.quarantined".to_string(),
                project_key: "fixture".to_string(),
                project_path: "/fixture".to_string(),
                title: None,
                started_at: Some(1),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    target_runtime
        .database()
        .writer_connection()
        .unwrap()
        .execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref,
                snippet_text, index_text, metadata_json, legacy_source, legacy_truncated
             ) VALUES (
                'claude', 'message.quarantined', 'session.quarantined',
                'user', 1, 1, 'secret-canary', 'deadbeef', 'inline', NULL,
                'secret-canary', 'secret-canary',
                '{\"payload_access\":\"quarantined\"}', 0, 0
             )",
            (),
        )
        .await
        .unwrap();
    target_runtime.checkpoint().await;

    temporal_initialize(&empty).await;

    sqlite::merge_temporal_for_test(&target, &empty)
        .await
        .unwrap();

    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_occurrences
             WHERE session_id = 'session.quarantined'"
        )
        .await,
        0
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_dispositions
             WHERE session_id = 'session.quarantined'
               AND disposition = 'quarantined'
               AND reason = 'payload_access'"
        )
        .await,
        1
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_receipts
             WHERE session_id = 'session.quarantined' AND imported_items = 0"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_forward_migrate_recovers_after_partial_failure_rematch() {
    let temp = TempDir::new().unwrap();
    let target_runtime = TemporalOwnedSessionFixture::new(temp.path(), "recover-target").await;
    let target = target_runtime.path.clone();
    let empty = temp.path().join("empty.db");
    let observation = migration_observation_for_scope(
        "session.legacy.recover",
        ObservationScopeV1::Project {
            project_id: target_runtime.project_id.clone(),
        },
        "receipt.legacy.recover",
        "message.legacy.recover",
        "recoverable legacy body",
    );
    let observation_id = observation.observation_id().as_str().to_owned();

    assert!(
        target_runtime
            .upsert_session(&SessionRecord {
                provider: "claude".to_string(),
                session_id: "session.legacy.recover".to_string(),
                project_key: "fixture".to_string(),
                project_path: "/fixture".to_string(),
                title: None,
                started_at: Some(1),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    let db = target_runtime.database();
    Box::pin(persist_migration_observation(db, observation, None)).await;
    assert_eq!(project_all_migration_observations(db).await, 1);
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = QueryExecutor::query(
        &snapshot,
        "SELECT 1
             FROM observation_projection_provenance AS provenance
             JOIN lcm_raw_messages AS raw
               ON raw.provider = provenance.output_provider
              AND raw.message_id = provenance.output_message_id
             WHERE provenance.observation_id = ?1
               AND raw.session_id = 'session.legacy.recover'",
        params![observation_id],
    )
    .await
    .unwrap();
    assert!(rows.next().await.unwrap().is_some());
    drop(rows);
    drop(snapshot);
    target_runtime.checkpoint().await;

    temporal_initialize(&empty).await;

    // Abort after the first temporal import write; outer TX must roll back.
    sqlite::set_forward_migrate_fault_after_import(true);
    let error = sqlite::merge_temporal_for_test(&target, &empty)
        .await
        .unwrap_err();
    sqlite::set_forward_migrate_fault_after_import(false);
    assert!(
        error
            .to_string()
            .contains("injected forward-migrate fault after import"),
        "{error}"
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_occurrences
             WHERE session_id = 'session.legacy.recover'"
        )
        .await,
        0
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_receipts
             WHERE session_id = 'session.legacy.recover'"
        )
        .await,
        0
    );

    // Rematch without the fault completes and is idempotent.
    sqlite::merge_temporal_for_test(&target, &empty)
        .await
        .unwrap();
    sqlite::merge_temporal_for_test(&target, &empty)
        .await
        .unwrap();
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_occurrences
             WHERE session_id = 'session.legacy.recover'"
        )
        .await,
        1
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_receipts
             WHERE session_id = 'session.legacy.recover' AND imported_items = 1"
        )
        .await,
        1
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_dispositions
             WHERE session_id = 'session.legacy.recover'
               AND disposition = 'eligible'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn temporal_forward_migrate_emits_typed_skip_dispositions() {
    let temp = TempDir::new().unwrap();
    let target_runtime = TemporalOwnedSessionFixture::new(temp.path(), "dispositions-target").await;
    let target = target_runtime.path.clone();
    let empty = temp.path().join("empty.db");
    assert!(
        target_runtime
            .upsert_session(&SessionRecord {
                provider: "claude".to_string(),
                session_id: "session.dispositions".to_string(),
                project_key: "fixture".to_string(),
                project_path: "/fixture".to_string(),
                title: None,
                started_at: Some(1),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    for (message_id, metadata, legacy_source, legacy_truncated) in [
        ("message.policy", "{}", 1_i64, 0_i64),
        ("message.unbound", "{}", 0, 0),
        (
            "message.ineligible",
            "{\"migration_origin\":\"temporal_compatibility\"}",
            0,
            0,
        ),
    ] {
        target_runtime
            .database()
            .writer_connection()
            .unwrap()
            .execute(
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref,
                    snippet_text, index_text, metadata_json, legacy_source, legacy_truncated
                 ) VALUES (
                    'claude', ?1, 'session.dispositions',
                    'user', 1, 1, 'body', 'deadbeef', 'inline', NULL,
                    'body', 'body', ?2, ?3, ?4
                 )",
                params![message_id, metadata, legacy_source, legacy_truncated],
            )
            .await
            .unwrap();
    }
    target_runtime.checkpoint().await;

    temporal_initialize(&empty).await;

    sqlite::merge_temporal_for_test(&target, &empty)
        .await
        .unwrap();

    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_dispositions
             WHERE session_id = 'session.dispositions' AND disposition = 'policy_excluded'"
        )
        .await,
        1
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_dispositions
             WHERE session_id = 'session.dispositions' AND disposition = 'unbound'"
        )
        .await,
        1
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_dispositions
             WHERE session_id = 'session.dispositions' AND disposition = 'ineligible'"
        )
        .await,
        1
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_occurrences
             WHERE session_id = 'session.dispositions'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn temporal_forward_migrate_preserves_multi_output_ordinals() {
    let temp = TempDir::new().unwrap();
    let target_runtime = TemporalOwnedSessionFixture::new(temp.path(), "multi-output-target").await;
    let target = target_runtime.path.clone();
    let empty = temp.path().join("empty.db");
    let observation_scope = ObservationScopeV1::Project {
        project_id: target_runtime.project_id.clone(),
    };
    let first = migration_observation_for_scope(
        "session.multi.output",
        observation_scope.clone(),
        "receipt.multi.output.a",
        "message.multi.a",
        "first multi-output body",
    );
    let second = migration_observation_range_for_scope(
        "session.multi.output",
        observation_scope.clone(),
        10,
        20,
        "receipt.multi.output.b",
        "message.multi.b",
        "second multi-output body",
    );
    let first_id = first.observation_id().as_str().to_owned();
    let second_id = second.observation_id().as_str().to_owned();

    assert!(
        target_runtime
            .upsert_session(&SessionRecord {
                provider: "claude".to_string(),
                session_id: "session.multi.output".to_string(),
                project_key: "fixture".to_string(),
                project_path: "/fixture".to_string(),
                title: None,
                started_at: Some(1),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    let db = target_runtime.database();
    Box::pin(persist_migration_observation(db, first, None)).await;
    assert_eq!(project_all_migration_observations(db).await, 1);
    Box::pin(persist_migration_observation(
        db,
        second,
        Some(migration_cursor_for_scope(
            "session.multi.output",
            10,
            observation_scope,
        )),
    ))
    .await;
    assert_eq!(project_all_migration_observations(db).await, 1);

    // Distinct projected outputs remain distinct after forward-migrate, and each
    // occurrence preserves the provenance output ordinal rather than collapsing
    // to a hard-coded zero.
    for (observation_id, expected_ordinal) in [(&first_id, 0_i64), (&second_id, 0_i64)] {
        let snapshot = db.read_snapshot().await.unwrap();
        let mut rows = QueryExecutor::query(
            &snapshot,
            "SELECT output_ordinal
                 FROM observation_projection_provenance
                 WHERE observation_id = ?1",
            params![observation_id.clone()],
        )
        .await
        .unwrap();
        let row = rows
            .next()
            .await
            .unwrap()
            .expect("projected provenance ordinal");
        assert_eq!(row.get::<i64>(0).unwrap(), expected_ordinal);
    }
    target_runtime.checkpoint().await;

    temporal_initialize(&empty).await;

    sqlite::merge_temporal_for_test(&target, &empty)
        .await
        .unwrap();

    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_occurrences
             WHERE session_id = 'session.multi.output'"
        )
        .await,
        2
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_occurrences AS occurrence
             JOIN observation_projection_provenance AS provenance
               ON provenance.observation_id = occurrence.source_observation_id
              AND provenance.output_ordinal = occurrence.projection_output_ordinal
             WHERE occurrence.session_id = 'session.multi.output'"
        )
        .await,
        2
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_dispositions
             WHERE session_id = 'session.multi.output'
               AND disposition = 'eligible'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn temporal_merge_rolls_back_across_supersession_and_fts_phases() {
    let temp = TempDir::new().unwrap();
    let target_runtime = TemporalOwnedSessionFixture::new(temp.path(), "phase-target").await;
    let target = target_runtime.path.clone();
    let source = temp.path().join("source.db");
    let empty = temp.path().join("empty.db");
    let watermarks = r#"{"active_generation":1,"cursor_key":null,"projection_frontier":0,"source_frontier":0,"summary_frontier":0}"#;
    let setup = |session_id: &str| {
        format!(
            "INSERT INTO session_temporal_generations(
                 session_id, generation, state, frozen_watermarks_json, created_at,
                 ready_at, activated_at, completed_at
             ) VALUES (
                 '{session_id}', 1, 'building', '{watermarks}', 1, NULL, NULL, NULL
             );
             UPDATE session_temporal_generations
             SET state = 'ready', ready_at = 2
             WHERE session_id = '{session_id}' AND generation = 1;
             UPDATE session_temporal_generations
             SET state = 'active', activated_at = 3
             WHERE session_id = '{session_id}' AND generation = 1;",
        )
    };

    target_runtime
        .database()
        .writer_connection()
        .unwrap()
        .execute_batch(&setup("session.phase.target"))
        .await
        .unwrap();
    temporal_execute_batch(&source, &setup("session.phase.source")).await;
    temporal_initialize(&empty).await;

    sqlite::set_temporal_merge_fault_phase("after_supersession_merge");
    let error = sqlite::merge_temporal_for_test(&target, &source)
        .await
        .unwrap_err();
    sqlite::set_temporal_merge_fault_phase("");
    assert!(
        error
            .to_string()
            .contains("injected temporal merge fault after supersession"),
        "{error}"
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_generations
             WHERE session_id = 'session.phase.source'"
        )
        .await,
        0
    );

    let observation = migration_observation_for_scope(
        "session.phase.fts",
        ObservationScopeV1::Project {
            project_id: target_runtime.project_id.clone(),
        },
        "receipt.phase.fts",
        "message.phase.fts",
        "fts phase rollback body",
    );
    assert!(
        target_runtime
            .upsert_session(&SessionRecord {
                provider: "claude".to_string(),
                session_id: "session.phase.fts".to_string(),
                project_key: "fixture".to_string(),
                project_path: "/fixture".to_string(),
                title: None,
                started_at: Some(1),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    Box::pin(persist_migration_observation(
        target_runtime.database(),
        observation,
        None,
    ))
    .await;
    assert_eq!(
        project_all_migration_observations(target_runtime.database()).await,
        1
    );
    target_runtime.checkpoint().await;

    sqlite::set_temporal_merge_fault_phase("after_fts_parity");
    let error = sqlite::merge_temporal_for_test(&target, &empty)
        .await
        .unwrap_err();
    sqlite::set_temporal_merge_fault_phase("");
    assert!(
        error
            .to_string()
            .contains("injected temporal merge fault after fts parity"),
        "{error}"
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_occurrences
             WHERE session_id = 'session.phase.fts'"
        )
        .await,
        0
    );
    assert_eq!(
        temporal_registered_scalar(
            target_runtime.database(),
            "SELECT COUNT(*) FROM session_temporal_migration_receipts
             WHERE session_id = 'session.phase.fts'"
        )
        .await,
        0
    );
}
