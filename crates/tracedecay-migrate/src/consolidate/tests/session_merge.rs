//! Session-store merge verification, divergent-projection preservation,
//! and message-family materialization consolidation tests.

use super::*;
use tracedecay_runtime_core::db::engine::DatabaseAttachmentExecutor;

async fn session_runtime(fixture: &Fixture, project_id: &str) -> HostAdmissionTestRuntimeV1 {
    let runtime =
        open_historical_project_runtime(&fixture.profile, &fixture.project, project_id).await;
    let expected = layout_for_id(&fixture.project, &fixture.profile, project_id)
        .unwrap()
        .sessions_db_path;
    assert_eq!(
        runtime.database_path(HostAdmissionScope::Project).unwrap(),
        expected
    );
    runtime
}

fn session_database(runtime: &HostAdmissionTestRuntimeV1) -> &RegisteredGlobalDb {
    runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap()
}

async fn execute_owned_session_sql(fixture: &Fixture, project_id: &str, sql: &str) {
    let runtime = session_runtime(fixture, project_id).await;
    let database = session_database(&runtime);
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction.execute_batch(sql).await.unwrap();
    transaction.commit().await.unwrap();
    database.checkpoint().await;
}

async fn execute_session_sql_at(path: &Path, sql: &str) {
    let (database, _) = test_open(path).await;
    let transaction = database
        .begin_write_transaction("mutate consolidation session fixture")
        .await
        .unwrap();
    transaction.execute_batch(sql).await.unwrap();
    transaction.commit().await.unwrap();
    database.checkpoint().await.unwrap();
    database.close();
}

async fn set_lcm_schema_version(path: &Path, version: i64) {
    let (database, _) = test_open(path).await;
    let transaction = database
        .begin_write_transaction("set consolidation LCM schema fixture")
        .await
        .unwrap();
    transaction
        .execute(
            "UPDATE session_schema_migrations SET version=?1 WHERE name='lcm'",
            params![version],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    database.checkpoint().await.unwrap();
    database.close();
}

async fn session_table_exists(path: &Path, table: &str) -> bool {
    let (database, _) = test_open_read_only(path).await;
    let snapshot = database
        .begin_engine_read_snapshot("inspect consolidation session fixture")
        .await
        .unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
            params![table],
        )
        .await
        .unwrap();
    let exists = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap() > 0;
    drop(rows);
    drop(snapshot);
    database.close();
    exists
}

#[tokio::test]
async fn planning_rejects_future_source_lcm_schema_before_target_normalization() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id)
        .unwrap()
        .sessions_db_path;
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id)
        .unwrap()
        .sessions_db_path;
    execute_owned_session_sql(
        &fixture,
        &fixture.target_id,
        "DROP TABLE lcm_lifecycle_state",
    )
    .await;
    set_lcm_schema_version(
        &source,
        crate::root_seam::sessions::lcm::LCM_SCHEMA_VERSION.saturating_add(1),
    )
    .await;
    let target_before = file_digest(&target).unwrap();

    let error = sqlite::plan_session_offsets(&target, &source)
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("newer LCM schema version"),
        "{error}"
    );
    assert_eq!(file_digest(&target).unwrap(), target_before);
    assert!(!session_table_exists(&target, "lcm_lifecycle_state").await);
}

#[tokio::test]
async fn planning_rejects_future_target_lcm_schema_without_normalization() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id)
        .unwrap()
        .sessions_db_path;
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id)
        .unwrap()
        .sessions_db_path;
    execute_owned_session_sql(
        &fixture,
        &fixture.target_id,
        &format!(
            "DROP TABLE lcm_lifecycle_state;
             UPDATE session_schema_migrations SET version={}
             WHERE name='lcm'",
            crate::root_seam::sessions::lcm::LCM_SCHEMA_VERSION.saturating_add(1)
        ),
    )
    .await;
    let target_before = file_digest(&target).unwrap();

    let error = sqlite::plan_session_offsets(&target, &source)
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("newer LCM schema version"),
        "{error}"
    );
    assert_eq!(file_digest(&target).unwrap(), target_before);
    assert!(!session_table_exists(&target, "lcm_lifecycle_state").await);
}

#[tokio::test]
async fn consolidation_discards_legacy_dashboard_token_count_cache() {
    let fixture = fixture().await;
    execute_owned_session_sql(
        &fixture,
        &fixture.source_id,
        "CREATE TABLE dashboard_token_counts (
             store TEXT NOT NULL,
             provider TEXT NOT NULL,
             message_id TEXT NOT NULL,
             text_len INTEGER NOT NULL,
             encoder TEXT NOT NULL,
             token_count INTEGER NOT NULL,
             computed_at INTEGER NOT NULL,
             PRIMARY KEY (store, provider, message_id)
         );
         INSERT INTO dashboard_token_counts(
             store, provider, message_id, text_len, encoder, token_count, computed_at
         ) VALUES ('legacy.db', 'codex', 'message-current-session', 12, 'o200k_base', 3, 1);",
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let destination = layout_for_id(
        &fixture.project,
        &fixture.profile,
        &applied.destination_project_id,
    )
    .unwrap()
    .sessions_db_path;

    assert!(!session_table_exists(&destination, "dashboard_token_counts").await);
}

#[tokio::test]
async fn merge_rejects_future_target_input_lcm_schema_before_destination_mutation() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id)
        .unwrap()
        .sessions_db_path;
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id)
        .unwrap()
        .sessions_db_path;
    let target_input = fixture.profile.join("future-target-input-sessions.db");
    let offsets = sqlite::plan_session_offsets(&target, &source)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target, &target_input).unwrap();
    set_lcm_schema_version(
        &target_input,
        crate::root_seam::sessions::lcm::LCM_SCHEMA_VERSION.saturating_add(1),
    )
    .await;
    let target_before = file_digest(&target).unwrap();

    let error = sqlite::merge_sessions(
        &target,
        &source,
        &target_input,
        &fixture.source_id,
        &offsets,
    )
    .await
    .unwrap_err();

    assert!(
        error.to_string().contains("newer LCM schema version"),
        "{error}"
    );
    assert_eq!(file_digest(&target).unwrap(), target_before);
}

#[tokio::test]
async fn verification_rejects_a_missing_unique_row_when_target_is_larger() {
    let fixture = fixture().await;
    for suffix in ["one", "two"] {
        add_fact_to_shard(
            &fixture,
            &fixture.target_id,
            &format!("extra target fact {suffix}"),
            "target-extra",
            json!({"suffix": suffix}),
            None,
        )
        .await;
    }
    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DatabasesMerged),
    )
    .await
    .unwrap_err();

    let graph_path = report
        .destination_data_root
        .join(tracedecay_runtime_core::config::DB_FILENAME);
    // Deliberately corrupt the frozen result outside the product writer. The
    // writer must continue rejecting the foreign-key bypass this fixture needs.
    let fixture_db = rusqlite::Connection::open(&graph_path).unwrap();
    fixture_db
        .execute(
            "DELETE FROM memory_facts WHERE content = 'legacy durable fact'",
            (),
        )
        .unwrap();
    drop(fixture_db);
    assert_eq!(
        sqlite::count_rows(&graph_path, "memory_facts")
            .await
            .unwrap(),
        report.target.facts,
        "the old max(input) count check would have accepted this loss"
    );

    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination fact logical union differs"),
        "{error}"
    );
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id
    );
}

#[tokio::test]
async fn verification_checks_session_bounds_and_immutable_message_payloads() {
    let fixture = fixture().await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DatabasesMerged),
    )
    .await
    .unwrap_err();
    let sessions = report
        .destination_data_root
        .join(storage::SESSIONS_DB_FILENAME);
    execute_session_sql_at(
        &sessions,
        "UPDATE sessions SET ended_at=42 WHERE session_id='legacy-session'",
    )
    .await;
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination session logical union differs"),
        "{error}"
    );

    execute_session_sql_at(
        &sessions,
        "UPDATE sessions SET ended_at=1800000001 WHERE session_id='legacy-session';
         UPDATE session_messages SET text='corrupted text'
         WHERE message_id='message-legacy-session';",
    )
    .await;
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination session message logical union differs"),
        "{error}"
    );

    execute_session_sql_at(
        &sessions,
        "UPDATE session_messages SET text='message from legacy-session'
         WHERE message_id='message-legacy-session';
         UPDATE lcm_raw_messages SET content_hash='corrupted-hash'
         WHERE session_id='legacy-session';",
    )
    .await;
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("destination LCM raw message logical union differs"),
        "{error}"
    );
}

#[tokio::test]
async fn divergent_session_message_projections_preserve_a_source_variant() {
    let fixture = fixture().await;
    execute_owned_session_sql(
        &fixture,
        &fixture.source_id,
        "INSERT INTO session_messages(
             provider, message_id, session_id, role, timestamp, ordinal, text, kind, model
         ) VALUES
             ('codex', 'message-current-session', 'legacy-session', 'assistant',
              1800000000, 1, 'source divergent projection', 'message', 'source-model'),
             ('codex', 'source-only-message', 'legacy-session', 'user',
              1800000001, 2, 'source-only projection', 'message', NULL);",
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    assert_eq!(report.collisions.message_overlaps, 1);
    assert_eq!(report.collisions.divergent_lcm_messages, 0);

    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let runtime = session_runtime(&fixture, &applied.destination_project_id).await;
    let sessions = session_database(&runtime);
    let selected = sessions
        .get_session_message("codex", "message-current-session")
        .await
        .unwrap();
    assert_eq!(selected.session_id, "current-session");
    assert_eq!(selected.text, "message from current-session");
    assert_eq!(selected.role, "user");
    let variant_id = format!("consolidated/{}/message-current-session", fixture.source_id);
    let variant = sessions
        .get_session_message("codex", &variant_id)
        .await
        .unwrap();
    assert_eq!(variant.session_id, "legacy-session");
    assert_eq!(variant.text, "source divergent projection");
    assert_eq!(variant.role, "assistant");
    let source_only = sessions
        .get_session_message("codex", "source-only-message")
        .await
        .unwrap();
    assert_eq!(source_only.session_id, "legacy-session");
    assert_eq!(source_only.text, "source-only projection");
}

#[tokio::test]
async fn lcm_representation_drift_uses_the_selected_target_row() {
    let fixture = fixture().await;
    let target_runtime = session_runtime(&fixture, &fixture.target_id).await;
    let target_raw = session_database(&target_runtime)
        .lcm_load_raw_message("codex", "message-current-session")
        .await
        .unwrap();
    drop(target_runtime);
    let source_runtime = session_runtime(&fixture, &fixture.source_id).await;
    let source = session_database(&source_runtime);
    let transaction = source.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "UPDATE lcm_raw_messages
             SET message_id=?1, content=?2, content_hash=?3,
                 storage_kind='external', payload_ref=NULL
             WHERE provider='codex' AND message_id='message-legacy-session'",
            params![
                target_raw.message_id.clone(),
                target_raw.content.clone(),
                target_raw.content_hash.clone()
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    source.checkpoint().await;
    drop(source_runtime);

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    assert_eq!(report.collisions.lcm_message_overlaps, 1);
    assert_eq!(report.collisions.divergent_lcm_messages, 1);
    assert_eq!(report.collisions.divergent_lcm_session_ids, 1);
    assert_eq!(report.collisions.divergent_lcm_content_hashes, 0);
    assert_eq!(report.collisions.divergent_lcm_storage_kinds, 1);
    assert_eq!(report.collisions.divergent_lcm_payload_refs, 0);

    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let destination_runtime = session_runtime(&fixture, &applied.destination_project_id).await;
    let destination = session_database(&destination_runtime);
    assert_eq!(
        destination
            .lcm_load_raw_message("codex", "message-current-session")
            .await
            .unwrap(),
        target_raw
    );
}

#[tokio::test]
async fn session_only_divergence_does_not_duplicate_identical_external_raw_family() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    let payload = b"shared payload";
    let payload_ref = "shared.payload";
    let content_hash = tracedecay_sessions::compatibility::projected_content_hash("shared payload");
    for layout in [&source, &target] {
        fs::create_dir_all(layout.data_root.join("lcm-payloads")).unwrap();
        fs::write(
            layout.data_root.join("lcm-payloads").join(payload_ref),
            payload,
        )
        .unwrap();
    }
    execute_owned_session_sql(
        &fixture,
        &fixture.source_id,
        &format!(
            "UPDATE session_messages
             SET message_id='message-current-session', text='source projection'
             WHERE message_id='message-legacy-session';
             UPDATE lcm_raw_messages
             SET message_id='message-current-session', content=NULL,
                 content_hash='{content_hash}', storage_kind='external',
                 payload_ref='{payload_ref}'
             WHERE message_id='message-legacy-session';
             INSERT INTO lcm_external_payloads(
                 payload_ref, provider, session_id, message_id, kind, content_hash,
                 byte_count, char_count
             ) VALUES('{payload_ref}', 'codex', 'legacy-session',
                      'message-current-session', 'message', '{content_hash}', 14, 14);"
        ),
    )
    .await;
    execute_owned_session_sql(
        &fixture,
        &fixture.target_id,
        &format!(
            "UPDATE lcm_raw_messages
             SET content=NULL, content_hash='{content_hash}', storage_kind='external',
                 payload_ref='{payload_ref}'
             WHERE message_id='message-current-session';
             INSERT INTO lcm_external_payloads(
                 payload_ref, provider, session_id, message_id, kind, content_hash,
                 byte_count, char_count
             ) VALUES('{payload_ref}', 'codex', 'current-session',
                      'message-current-session', 'message', '{content_hash}', 14, 14);"
        ),
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let variant_id = format!("consolidated/{}/message-current-session", fixture.source_id);
    let runtime = session_runtime(&fixture, &applied.destination_project_id).await;
    let sessions = session_database(&runtime);
    assert!(
        sessions
            .get_session_message("codex", &variant_id)
            .await
            .is_some()
    );
    assert!(
        sessions
            .lcm_load_raw_message("codex", &variant_id)
            .await
            .is_none()
    );
    let raw = sessions
        .lcm_load_raw_message("codex", "message-current-session")
        .await
        .unwrap();
    assert_eq!(raw.content_hash, content_hash);
    assert_eq!(raw.payload_ref.as_deref(), Some(payload_ref));
    let snapshot = sessions.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT message_id FROM lcm_external_payloads WHERE payload_ref=?1",
            [payload_ref],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "message-current-session"
    );
    drop(rows);
    drop(snapshot);
}

#[tokio::test]
async fn distinct_external_content_variant_preserves_owner_expansion_and_retry() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    let mut source_ref = None;
    for (layout, project_id, session_id, old_message_id, content) in [
        (
            &source,
            fixture.source_id.as_str(),
            "legacy-session",
            "message-legacy-session",
            "source external body",
        ),
        (
            &target,
            fixture.target_id.as_str(),
            "current-session",
            "message-current-session",
            "target external body",
        ),
    ] {
        let payload = crate::root_seam::sessions::lcm::payload::write_external_payload(
            &layout.data_root,
            "codex",
            session_id,
            "message-current-session",
            "message",
            content,
            None,
        )
        .unwrap();
        if session_id == "legacy-session" {
            source_ref = Some(payload.payload_ref.clone());
        }
        let runtime = session_runtime(&fixture, project_id).await;
        let db = session_database(&runtime);
        let writer = db.begin_write_transaction().await.unwrap();
        crate::root_seam::sessions::lcm::payload::upsert_payload_metadata(&writer, &payload)
            .await
            .unwrap();
        writer
            .execute(
                "UPDATE session_messages
                 SET message_id='message-current-session', text=?1
                 WHERE provider='codex' AND message_id=?2",
                params![content, old_message_id],
            )
            .await
            .unwrap();
        writer
            .execute(
                "UPDATE lcm_raw_messages
                 SET message_id='message-current-session', content=NULL,
                     content_hash=?1, storage_kind='external', payload_ref=?2
                 WHERE provider='codex' AND message_id=?3",
                params![payload.content_hash, payload.payload_ref, old_message_id],
            )
            .await
            .unwrap();
        writer.commit().await.unwrap();
        db.checkpoint().await;
        drop(runtime);
    }

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let variant_id = format!("consolidated/{}/message-current-session", fixture.source_id);
    let source_ref = source_ref.unwrap();
    let runtime = session_runtime(&fixture, &applied.destination_project_id).await;
    let sessions = session_database(&runtime);
    let raw = sessions
        .lcm_load_raw_message("codex", &variant_id)
        .await
        .unwrap();
    assert_eq!(raw.payload_ref.as_deref(), Some(source_ref.as_str()));
    let snapshot = sessions.read_snapshot().await.unwrap();
    let mut owners = snapshot
        .query(
            "SELECT message_id FROM lcm_external_payloads WHERE payload_ref=?1",
            [source_ref.as_str()],
        )
        .await
        .unwrap();
    assert_eq!(
        owners
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        variant_id
    );
    drop(owners);
    let expanded = crate::root_seam::sessions::lcm::payload::expand_payload(
        &snapshot,
        &applied.destination_data_root,
        "codex",
        "legacy-session",
        &source_ref,
        0,
        100,
    )
    .await
    .unwrap();
    assert_eq!(expanded.content, "source external body");
    drop(snapshot);
    drop(runtime);

    let retried = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(retried.state, ConsolidationState::Applied);
}

#[tokio::test]
async fn divergent_projection_and_raw_content_preserve_a_linked_source_variant() {
    let fixture = fixture().await;
    execute_owned_session_sql(
        &fixture,
        &fixture.source_id,
        "UPDATE session_messages
         SET message_id='message-current-session', text='source divergent projection',
             metadata_json='{\"parent_message_id\":\"message-current-session\"}'
         WHERE provider='codex' AND message_id='message-legacy-session';
         UPDATE lcm_raw_messages
         SET message_id='message-current-session',
             metadata_json='{\"parent_message_id\":\"message-current-session\"}'
         WHERE provider='codex' AND message_id='message-legacy-session';
         INSERT INTO turns(
             message_id, project_hash, session_id, model, timestamp, input_tokens,
             output_tokens, cost_usd, category
         ) VALUES(
             'message-current-session', 'project', 'legacy-session', 'model',
             1800000000, 1, 1, 0.0, 'task'
         );
         INSERT INTO session_messages(
             provider, message_id, session_id, role, timestamp, ordinal, text,
             kind, metadata_json
         ) VALUES(
             'codex', 'message-current-session:thinking', 'legacy-session',
             'assistant', 1800000001, 2, 'source thinking', 'reasoning',
             '{\"parent_message_id\":\"message-current-session\"}'
         );
         INSERT INTO lcm_raw_messages(
             provider, message_id, session_id, role, ordinal, timestamp, content,
             content_hash, storage_kind, snippet_text, index_text, metadata_json
         ) VALUES(
             'codex', 'message-current-session:thinking', 'legacy-session',
             'assistant', 2, 1800000001, 'source thinking', 'thinking-hash',
             'inline', 'source thinking', 'source thinking',
             '{\"parent_message_id\":\"message-current-session\"}'
         );
         INSERT INTO lcm_summary_nodes(
             node_id, provider, conversation_id, session_id, depth, summary_text,
             summary_hash, summary_token_count, source_token_count, created_at
         ) VALUES(
             'variant-summary', 'codex', 'source-conversation', 'legacy-session', 1,
             'variant summary', 'variant-summary-hash', 1, 1, 1800000002
         );
         INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
         SELECT 'variant-summary', 'raw_message', CAST(store_id AS TEXT), 0
         FROM lcm_raw_messages WHERE message_id='message-current-session';",
    )
    .await;
    execute_owned_session_sql(
        &fixture,
        &fixture.target_id,
        "INSERT INTO session_messages(
             provider, message_id, session_id, role, timestamp, ordinal, text,
             kind, metadata_json
         ) VALUES(
             'codex', 'message-current-session:thinking', 'current-session',
             'assistant', 1800000001, 2, 'source thinking', 'reasoning',
             '{\"parent_message_id\":\"message-current-session\"}'
         );
         INSERT INTO lcm_raw_messages(
             provider, message_id, session_id, role, ordinal, timestamp, content,
             content_hash, storage_kind, snippet_text, index_text, metadata_json
         ) VALUES(
             'codex', 'message-current-session:thinking', 'current-session',
             'assistant', 2, 1800000001, 'source thinking', 'thinking-hash',
             'inline', 'source thinking', 'source thinking',
             '{\"parent_message_id\":\"message-current-session\"}'
         );",
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    assert_eq!(report.collisions.divergent_lcm_messages, 2);
    assert_eq!(report.collisions.divergent_lcm_session_ids, 2);
    assert_eq!(report.collisions.divergent_lcm_content_hashes, 1);
    assert_eq!(report.collisions.divergent_lcm_storage_kinds, 0);
    assert_eq!(report.collisions.divergent_lcm_payload_refs, 0);

    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let variant_id = format!("consolidated/{}/message-current-session", fixture.source_id);
    let runtime = session_runtime(&fixture, &applied.destination_project_id).await;
    let sessions = session_database(&runtime);
    assert_eq!(
        sessions
            .get_session_message("codex", "message-current-session")
            .await
            .unwrap()
            .text,
        "message from current-session"
    );
    let variant = sessions
        .get_session_message("codex", &variant_id)
        .await
        .unwrap();
    assert_eq!(variant.text, "source divergent projection");
    let metadata: serde_json::Value =
        serde_json::from_str(variant.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["parent_message_id"], variant_id);
    assert_eq!(
        metadata["consolidation_original_parent_message_id"],
        "message-current-session"
    );
    let raw_variant = sessions
        .lcm_load_raw_message("codex", &variant_id)
        .await
        .unwrap();
    assert_eq!(raw_variant.content, "message from legacy-session");
    let raw_metadata: serde_json::Value =
        serde_json::from_str(raw_variant.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(raw_metadata["parent_message_id"], variant_id);
    assert_eq!(
        raw_metadata["consolidation_original_parent_message_id"],
        "message-current-session"
    );
    let thinking_variant_id = format!("{variant_id}:thinking");
    let thinking = sessions
        .get_session_message("codex", &thinking_variant_id)
        .await
        .unwrap();
    let thinking_metadata: serde_json::Value =
        serde_json::from_str(thinking.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(thinking_metadata["parent_message_id"], variant_id);
    assert_eq!(
        thinking_metadata["consolidation_original_parent_message_id"],
        "message-current-session"
    );
    assert!(
        sessions
            .lcm_load_raw_message("codex", &thinking_variant_id)
            .await
            .is_none()
    );
    let thinking_raw = sessions
        .lcm_load_raw_message("codex", "message-current-session:thinking")
        .await
        .unwrap();
    let thinking_raw_metadata: serde_json::Value =
        serde_json::from_str(thinking_raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        thinking_raw_metadata["parent_message_id"],
        "message-current-session"
    );
    let snapshot = sessions.read_snapshot().await.unwrap();
    let mut turn_rows = snapshot
        .query(
            "SELECT message_id FROM turns WHERE session_id='legacy-session'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        turn_rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        variant_id
    );
    drop(turn_rows);
    let mut rows = snapshot
        .query(
            "SELECT r.message_id
             FROM lcm_summary_sources s
             JOIN lcm_raw_messages r ON r.store_id=CAST(s.source_id AS INTEGER)
             WHERE s.node_id='variant-summary'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        variant_id
    );
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);
    drop(snapshot);
    drop(runtime);

    let retried = apply(&options, &report.confirmation_token).await.unwrap();
    assert_eq!(retried.state, ConsolidationState::Applied);
    let runtime = session_runtime(&fixture, &retried.destination_project_id).await;
    let sessions = session_database(&runtime);
    assert_eq!(sessions.session_message_count().await.unwrap(), 4);
}

#[tokio::test]
async fn indexed_message_family_materialization_handles_deep_and_wide_graph() {
    const DEPTH: usize = 128;
    const WIDTH: usize = 256;

    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    execute_owned_session_sql(
        &fixture,
        &fixture.source_id,
        "UPDATE session_messages
         SET message_id='message-current-session', text='source divergent projection'
         WHERE provider='codex' AND message_id='message-legacy-session';",
    )
    .await;

    let mut family_sql = String::from(
        "INSERT OR IGNORE INTO sessions(provider, session_id, project_key, project_path)
         VALUES('codex', 'family-session', 'project', '/repo');",
    );
    let mut parent = "message-current-session".to_string();
    for depth in 0..DEPTH {
        let child = format!("family-depth-{depth}");
        let _ = write!(
            family_sql,
            "INSERT INTO session_messages(
                 provider, message_id, session_id, role, ordinal, text, kind, metadata_json
             ) VALUES(
                 'codex', '{child}', 'family-session', 'assistant', {ordinal},
                 'depth {depth}', 'message', '{{\"parent_message_id\":\"{parent}\"}}'
             );",
            ordinal = depth + 2,
        );
        parent = child;
    }
    for width in 0..WIDTH {
        let child = format!("family-wide-{width}");
        let _ = write!(
            family_sql,
            "INSERT INTO session_messages(
                 provider, message_id, session_id, role, ordinal, text, kind, metadata_json
             ) VALUES(
                 'codex', '{child}', 'family-session', 'assistant', {ordinal},
                 'wide {width}', 'message',
                 '{{\"parent_message_id\":\"message-current-session\"}}'
             );",
            ordinal = DEPTH + width + 2,
        );
    }
    execute_owned_session_sql(&fixture, &fixture.source_id, &family_sql).await;
    execute_owned_session_sql(&fixture, &fixture.target_id, &family_sql).await;
    let staging = fixture.profile.join("session-map-staging");
    fs::create_dir_all(&staging).unwrap();
    let staged_source = staging.join("source.db");
    let staged_target = staging.join("target.db");
    fs::copy(&source.sessions_db_path, &staged_source).unwrap();
    fs::copy(&target.sessions_db_path, &staged_target).unwrap();
    sqlite::plan_session_offsets(&staged_target, &staged_source)
        .await
        .unwrap();

    let (target_db, _) = test_open(&staged_target).await;
    let writer = target_db
        .begin_write_transaction("attach consolidation test input")
        .await
        .unwrap();
    writer
        .attach_database(&staged_source, "source_input")
        .await
        .unwrap();
    sqlite::build_consolidation_message_map(&writer, "source_input", "main", &fixture.source_id)
        .await
        .unwrap();

    let mut rows = writer
        .query("SELECT COUNT(*) FROM consolidation_message_map", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        (1 + DEPTH + WIDTH) as i64
    );
    drop(rows);
    for original_id in [
        format!("family-depth-{}", DEPTH - 1),
        format!("family-wide-{}", WIDTH - 1),
    ] {
        let mut rows = writer
            .query(
                "SELECT mapped_id FROM consolidation_message_map
                 WHERE provider='codex' AND original_id=?1",
                [original_id.as_str()],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            format!("consolidated/{}/{original_id}", fixture.source_id)
        );
    }

    let family_plan_sql = format!(
        "{} SELECT COUNT(*) FROM variant_family",
        sqlite::session_variant_family_cte()
    );
    let family_plan = explain_query_plan(&writer, &family_plan_sql).await;
    assert!(
        family_plan
            .iter()
            .any(|detail| detail.contains("SEARCH edge USING")),
        "recursive family lookup must use the parent-edge primary key: {family_plan:?}"
    );
    assert!(
        family_plan
            .iter()
            .all(|detail| !detail.contains("session_messages")),
        "recursive family step must not rescan source session messages: {family_plan:?}"
    );

    let reserved_plan = explain_query_plan(&writer, sqlite::reserved_message_collision_sql()).await;
    assert!(
        reserved_plan
            .iter()
            .any(|detail| detail.starts_with("SEARCH r ") && detail.contains("USING PRIMARY KEY")),
        "reserved-reference lookup must use its primary key: {reserved_plan:?}"
    );

    let turn_lookup = sqlite::mapped_turn_message_id("s");
    let turn_plan_sql = format!(
        "SELECT {turn_lookup}
         FROM (SELECT 'message-current-session' AS message_id,
                      'legacy-session' AS session_id) s"
    );
    let turn_plan = explain_query_plan(&writer, &turn_plan_sql).await;
    assert!(
        turn_plan
            .iter()
            .any(|detail| detail.contains("SEARCH m USING")),
        "turn-owner lookup must use its primary key: {turn_plan:?}"
    );

    writer.commit().await.unwrap();
    target_db.close();
}

#[tokio::test]
async fn numeric_and_boolean_parent_ids_expand_the_variant_family() {
    let fixture = fixture().await;
    execute_owned_session_sql(
        &fixture,
        &fixture.source_id,
        "UPDATE session_messages
         SET message_id='7', text='source divergent projection'
         WHERE provider='codex' AND message_id='message-legacy-session';",
    )
    .await;
    execute_owned_session_sql(
        &fixture,
        &fixture.target_id,
        "UPDATE session_messages SET message_id='7'
         WHERE provider='codex' AND message_id='message-current-session';",
    )
    .await;
    let family_sql =
        "INSERT OR IGNORE INTO sessions(provider, session_id, project_key, project_path)
         VALUES('codex', 'scalar-family', 'project', '/repo');
         INSERT INTO session_messages(
             provider, message_id, session_id, role, ordinal, text, kind, metadata_json
         ) VALUES(
             'codex', '1', 'scalar-family', 'assistant', 1, 'numeric child', 'message',
             '{\"parent_message_id\":7}'
         );
         INSERT INTO session_messages(
             provider, message_id, session_id, role, ordinal, text, kind, metadata_json
         ) VALUES(
             'codex', 'boolean-child', 'scalar-family', 'assistant', 2,
             'boolean child', 'message', '{\"parent_message_id\":true}'
         );";
    execute_owned_session_sql(&fixture, &fixture.source_id, family_sql).await;
    execute_owned_session_sql(&fixture, &fixture.target_id, family_sql).await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let runtime = session_runtime(&fixture, &applied.destination_project_id).await;
    let sessions = session_database(&runtime);
    let numeric_id = format!("consolidated/{}/1", fixture.source_id);
    let boolean_id = format!("consolidated/{}/boolean-child", fixture.source_id);
    let numeric = sessions
        .get_session_message("codex", &numeric_id)
        .await
        .unwrap();
    let boolean = sessions
        .get_session_message("codex", &boolean_id)
        .await
        .unwrap();
    let numeric_metadata: serde_json::Value =
        serde_json::from_str(numeric.metadata_json.as_deref().unwrap()).unwrap();
    let boolean_metadata: serde_json::Value =
        serde_json::from_str(boolean.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        numeric_metadata["parent_message_id"],
        format!("consolidated/{}/7", fixture.source_id)
    );
    assert_eq!(boolean_metadata["parent_message_id"], numeric_id);
}

#[tokio::test]
async fn synthetic_message_key_parent_reference_collision_fails_before_merge() {
    let fixture = fixture().await;
    let synthetic = format!("consolidated/{}/message-current-session", fixture.source_id);
    let source_runtime = session_runtime(&fixture, &fixture.source_id).await;
    let source_db = session_database(&source_runtime);
    let transaction = source_db.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "UPDATE session_messages
             SET message_id='message-current-session', text='source divergent projection',
                 metadata_json=?1
             WHERE provider='codex' AND message_id='message-legacy-session'",
            [format!("{{\"parent_message_id\":\"{synthetic}\"}}")],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    source_db.checkpoint().await;
    drop(source_runtime);

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("synthetic consolidation message key collision"),
        "{error}"
    );
}

#[tokio::test]
async fn synthetic_message_key_collision_fails_before_merge() {
    let fixture = fixture().await;
    let synthetic = format!("consolidated/{}/message-current-session", fixture.source_id);
    let source_runtime = session_runtime(&fixture, &fixture.source_id).await;
    let source = session_database(&source_runtime);
    let transaction = source.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "UPDATE session_messages
             SET message_id='message-current-session', text='source divergent projection'
             WHERE provider='codex' AND message_id='message-legacy-session'",
            (),
        )
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO session_messages(
                 provider, message_id, session_id, role, ordinal, text, kind
             ) VALUES('codex', ?1, 'legacy-session', 'user', 2, 'native collision', 'message')",
            [synthetic],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    source.checkpoint().await;
    drop(source_runtime);

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("synthetic consolidation message key collision"),
        "{error}"
    );
    assert_eq!(
        storage::read_repository_identity_marker(&fixture.project)
            .unwrap()
            .unwrap()
            .project_id,
        fixture.target_id
    );
}

#[tokio::test]
async fn ambiguous_cross_provider_turn_mapping_fails_closed() {
    let fixture = fixture().await;
    execute_owned_session_sql(
        &fixture,
        &fixture.source_id,
        "UPDATE session_messages
         SET message_id='shared-message', text='source codex'
         WHERE provider='codex' AND message_id='message-legacy-session';
         INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES('claude', 'legacy-session', 'project', '/repo');
         INSERT INTO session_messages(
             provider, message_id, session_id, role, ordinal, text, kind
         ) VALUES('claude', 'shared-message', 'legacy-session', 'assistant', 1,
                  'source claude', 'message');
         INSERT INTO turns(
             message_id, project_hash, session_id, model, timestamp, input_tokens,
             output_tokens, cost_usd, category
         ) VALUES('shared-message', 'project', 'legacy-session', 'model',
                  1800000000, 1, 1, 0.0, 'task');",
    )
    .await;
    execute_owned_session_sql(
        &fixture,
        &fixture.target_id,
        "UPDATE session_messages SET message_id='shared-message'
         WHERE provider='codex' AND message_id='message-current-session';
         INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES('claude', 'current-session', 'project', '/repo');
         INSERT INTO session_messages(
             provider, message_id, session_id, role, ordinal, text, kind
         ) VALUES('claude', 'shared-message', 'current-session', 'user', 1,
                  'target claude', 'message');",
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("source turn message mapping ambiguity collision"),
        "{error}"
    );
}

#[tokio::test]
async fn divergent_external_payload_identity_remains_a_hard_error() {
    let fixture = fixture().await;
    execute_owned_session_sql(
        &fixture,
        &fixture.source_id,
        "INSERT INTO lcm_external_payloads(
             payload_ref, provider, session_id, message_id, kind, content_hash,
             byte_count, char_count, metadata_json
         ) VALUES('shared-ref', 'codex', 'legacy-session', 'message-legacy-session',
                  'tool', 'source-hash', 10, 10, NULL);",
    )
    .await;
    execute_owned_session_sql(
        &fixture,
        &fixture.target_id,
        "INSERT INTO lcm_external_payloads(
             payload_ref, provider, session_id, message_id, kind, content_hash,
             byte_count, char_count, metadata_json
         ) VALUES('shared-ref', 'codex', 'current-session', 'message-current-session',
                  'tool', 'target-hash', 11, 11, NULL);",
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("divergent LCM external payload collision"),
        "{error}"
    );
}

#[tokio::test]
async fn divergent_summary_node_identity_remains_a_hard_error() {
    let fixture = fixture().await;
    for (project_id, session, text, hash) in [
        (
            fixture.source_id.as_str(),
            "legacy-session",
            "source summary",
            "source-hash",
        ),
        (
            fixture.target_id.as_str(),
            "current-session",
            "target summary",
            "target-hash",
        ),
    ] {
        let runtime = session_runtime(&fixture, project_id).await;
        let db = session_database(&runtime);
        let transaction = db.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "INSERT INTO lcm_summary_nodes(
                     node_id, provider, conversation_id, session_id, depth, summary_text,
                     summary_hash, summary_token_count, source_token_count, created_at
                 ) VALUES('shared-summary', 'codex', 'conversation', ?1, 1, ?2, ?3, 1, 1, 1800000002)",
                params![session, text, hash],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        db.checkpoint().await;
        drop(runtime);
    }

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("divergent LCM summary node collision"),
        "{error}"
    );
}
