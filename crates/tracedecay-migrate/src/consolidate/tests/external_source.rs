//! External-source reducer-state consolidation and rollback tests.

use std::collections::BTreeMap;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalSourceStateRow {
    owner_id: String,
    state_json: String,
}

async fn insert_external_source_state(path: &Path, binding_id: &str, owner_id: &str, marker: &str) {
    let (database, _) = test_open(path).await;
    let transaction = database
        .begin_write_transaction("insert external-source consolidation fixture")
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO external_source_states_v1 (
                binding_id, source_id, owner_kind, owner_id,
                definition_digest, binding_digest, frontier_digest,
                receipt_idempotency_key, receipt_request_digest, state_json
             ) VALUES (?1, ?2, 'project', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                binding_id,
                format!("source.{marker}"),
                owner_id,
                format!("definition.{marker}"),
                format!("binding.{marker}"),
                format!("frontier.{marker}"),
                format!("idempotency.{marker}"),
                format!("request.{marker}"),
                format!(r#"{{"marker":"{marker}"}}"#),
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    database.checkpoint().await.unwrap();
    database.close();
}

async fn external_source_states(path: &Path) -> BTreeMap<String, ExternalSourceStateRow> {
    let (database, _) = test_open_read_only(path).await;
    let snapshot = database
        .begin_engine_read_snapshot("read external-source consolidation fixture")
        .await
        .unwrap();
    let mut rows = snapshot
        .query(
            "SELECT binding_id, owner_id, state_json
             FROM external_source_states_v1
             ORDER BY binding_id",
            (),
        )
        .await
        .unwrap();
    let mut states = BTreeMap::new();
    while let Some(row) = rows.next().await.unwrap() {
        states.insert(
            row.get::<String>(0).unwrap(),
            ExternalSourceStateRow {
                owner_id: row.get(1).unwrap(),
                state_json: row.get(2).unwrap(),
            },
        );
    }
    drop(rows);
    drop(snapshot);
    database.close();
    states
}

async fn delete_external_source_state(path: &Path, binding_id: &str) {
    let (database, _) = test_open(path).await;
    let transaction = database
        .begin_write_transaction("delete external-source consolidation fixture")
        .await
        .unwrap();
    transaction
        .execute(
            "DELETE FROM external_source_states_v1 WHERE binding_id=?1",
            params![binding_id],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    database.checkpoint().await.unwrap();
    database.close();
}

async fn insert_union_fixture(
    target: &Path,
    source: &Path,
    target_owner: &str,
    source_owner: &str,
    prefix: &str,
) {
    insert_external_source_state(
        target,
        &format!("binding.{prefix}.target"),
        target_owner,
        &format!("{prefix}.target"),
    )
    .await;
    insert_external_source_state(
        source,
        &format!("binding.{prefix}.source"),
        source_owner,
        &format!("{prefix}.source"),
    )
    .await;
    insert_external_source_state(
        target,
        &format!("binding.{prefix}.shared"),
        target_owner,
        &format!("{prefix}.shared"),
    )
    .await;
    insert_external_source_state(
        source,
        &format!("binding.{prefix}.shared"),
        target_owner,
        &format!("{prefix}.shared"),
    )
    .await;
}

pub(super) async fn assert_executable_union_witness() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    insert_union_fixture(
        &target.graph_db_path,
        &source.graph_db_path,
        &fixture.target_id,
        &fixture.source_id,
        "graph",
    )
    .await;
    insert_union_fixture(
        &target.sessions_db_path,
        &source.sessions_db_path,
        &fixture.target_id,
        &fixture.source_id,
        "session",
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();

    for (path, prefix) in [
        (
            applied
                .destination_data_root
                .join(tracedecay_runtime_core::config::DB_FILENAME),
            "graph",
        ),
        (
            applied
                .destination_data_root
                .join(storage::SESSIONS_DB_FILENAME),
            "session",
        ),
    ] {
        let states = external_source_states(&path).await;
        assert_eq!(states.len(), 3);
        assert_eq!(
            states[&format!("binding.{prefix}.source")].owner_id,
            fixture.source_id
        );
        assert_eq!(
            states[&format!("binding.{prefix}.source")].state_json,
            format!(r#"{{"marker":"{prefix}.source"}}"#)
        );
        assert_eq!(
            states[&format!("binding.{prefix}.target")].owner_id,
            fixture.target_id
        );
        assert_eq!(
            states[&format!("binding.{prefix}.shared")].state_json,
            format!(r#"{{"marker":"{prefix}.shared"}}"#)
        );
    }
}

#[tokio::test]
async fn consolidation_unions_external_source_states_in_graph_and_session_stores() {
    assert_executable_union_witness().await;
}

#[tokio::test]
async fn divergent_external_source_state_rolls_back_the_session_merge() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id)
        .unwrap()
        .sessions_db_path;
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id)
        .unwrap()
        .sessions_db_path;
    let target_input = fixture
        .profile
        .join("external-source-target-input-sessions.db");
    insert_external_source_state(
        &target,
        "binding.session.conflict",
        &fixture.target_id,
        "session.target",
    )
    .await;
    insert_external_source_state(
        &source,
        "binding.session.conflict",
        &fixture.target_id,
        "session.source",
    )
    .await;
    let before = external_source_states(&target).await;
    let offsets = sqlite::plan_session_offsets(&target, &source)
        .await
        .unwrap();
    copy_sqlite_family_exact(&target, &target_input).unwrap();

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
        error
            .to_string()
            .contains("external source state collision"),
        "{error}"
    );
    assert_eq!(external_source_states(&target).await, before);
}

#[tokio::test]
async fn divergent_external_source_state_aborts_apply_without_retiring_inputs() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    let target = layout_for_id(&fixture.project, &fixture.profile, &fixture.target_id).unwrap();
    insert_external_source_state(
        &target.graph_db_path,
        "binding.graph.conflict",
        &fixture.target_id,
        "graph.target",
    )
    .await;
    insert_external_source_state(
        &source.graph_db_path,
        "binding.graph.conflict",
        &fixture.target_id,
        "graph.source",
    )
    .await;
    let source_before = external_source_states(&source.graph_db_path).await;
    let target_before = external_source_states(&target.graph_db_path).await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();

    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("external source state collision"),
        "{error}"
    );
    assert_eq!(
        external_source_states(&source.graph_db_path).await,
        source_before
    );
    assert_eq!(
        external_source_states(&target.graph_db_path).await,
        target_before
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
async fn final_verification_rejects_missing_external_source_state() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    insert_external_source_state(
        &source.graph_db_path,
        "binding.graph.verification",
        &fixture.source_id,
        "graph.verification",
    )
    .await;
    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    apply_with_stop(
        &options,
        &report.confirmation_token,
        Some(ConsolidationState::DatabasesMerged),
    )
    .await
    .unwrap_err();
    let destination_graph = report
        .destination_data_root
        .join(tracedecay_runtime_core::config::DB_FILENAME);
    delete_external_source_state(&destination_graph, "binding.graph.verification").await;

    let error = apply(&options, &report.confirmation_token)
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("destination external source state union differs"),
        "{error}"
    );
}
