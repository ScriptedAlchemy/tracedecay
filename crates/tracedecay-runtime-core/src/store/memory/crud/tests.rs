use std::sync::Arc;

use crate::db::engine::params;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use crate::store::memory::{DatabaseFactStore, FactWriteControl};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use tracedecay_domain::{
    Confidence, DomainError, FactCategoryV1, FactEventId, FactOwnerV1, LocatorDigest, ProvenanceId,
};
use tracedecay_store::{
    FactReadControl, FactStoreError, ProjectMemoryFactAddCommandV1,
    ProjectMemoryFactAddDispositionV1, ProjectMemoryFactAddMaterialV1,
    ProjectMemoryFactAddOutcomeV1, ProjectMemoryFactContentDigestQueryV1,
    ProjectMemoryFactFeedbackActionV1, ProjectMemoryFactFeedbackCommandV1,
    ProjectMemoryFactFeedbackDetailsAvailabilityV1, ProjectMemoryFactFeedbackHistoryQueryV1,
    ProjectMemoryFactHistoryQueryV1, ProjectMemoryFactIdV1, ProjectMemoryFactListQueryV1,
    ProjectMemoryFactProjectionV1, ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactStore,
    ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdatePatchV1, ProjectMemoryFactV1,
};

use super::feedback::inspect_project_memory_fact_controlled_tx;
use super::project::{
    find_project_memory_fact_by_content_digest_controlled_tx,
    get_project_memory_fact_controlled_tx, list_project_memory_facts_controlled_tx,
    project_memory_fact_history_controlled_tx,
};

async fn database() -> (TempDir, Database) {
    let directory = tempdir().expect("create canonical memory fixture directory");
    let path = directory.path().join("canonical-update-replay.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "canonical update replay test authority")
            .expect("acquire canonical memory test authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("publish canonical memory test runtime");
    (directory, database)
}

fn write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

fn read_control() -> FactReadControl {
    FactReadControl::new(Arc::new(|| false))
}

fn cancelled_read_control() -> FactReadControl {
    FactReadControl::new(Arc::new(|| true))
}

fn accepted_add_command(
    owner: FactOwnerV1,
    operation_id: &str,
    content: &str,
    source_label: Option<&str>,
) -> ProjectMemoryFactAddCommandV1 {
    let mut material = json!({
        "content": content,
        "category": "project",
        "tags": ["idempotency"],
        "entities": ["TraceDecay"],
        "metadata": {"fixture": "canonical-update-replay"},
    });
    if let (Value::Object(material), Some(source_label)) = (&mut material, source_label) {
        material.insert(
            "source_label".to_owned(),
            Value::String(source_label.to_owned()),
        );
    }
    let receipt = match sanitize_memory_fact_payload(material.clone())
        .expect("sanitize canonical add fixture")
    {
        MemoryFactSanitizationV1::Durable { payload, receipt } => {
            assert_eq!(payload, material);
            receipt
        }
        MemoryFactSanitizationV1::Quarantined => {
            panic!("canonical add fixture must not be quarantined")
        }
    };
    ProjectMemoryFactAddMaterialV1::new(
        owner,
        content.to_owned(),
        FactCategoryV1::Project,
        source_label.map(ToOwned::to_owned),
        vec!["idempotency".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({"fixture": "canonical-update-replay"}),
        receipt,
        None,
        Confidence::new(0.5).expect("canonical default trust"),
        None,
    )
    .and_then(|material| {
        material.into_command(
            ProvenanceId::new(operation_id.to_owned()).expect("canonical add operation identity"),
        )
    })
    .expect("canonical add command")
}

async fn added_target(
    store: &DatabaseFactStore<'_>,
    control: &FactWriteControl,
) -> ProjectMemoryFactIdV1 {
    let owner = FactOwnerV1::Profile;
    let added = store
        .add_project_memory_fact(
            accepted_add_command(
                owner.clone(),
                "operation.crud-update-replay-add",
                "The canonical memory authority preserves update receipts.",
                None,
            ),
            control,
        )
        .await
        .expect("add canonical update fixture");
    assert_eq!(
        added.disposition(),
        ProjectMemoryFactAddDispositionV1::Added
    );
    let fact_id = added.fact().fact_id().clone();
    ProjectMemoryFactIdV1::new(owner, fact_id).expect("owner-bound canonical fact identity")
}

fn update_command(
    target: ProjectMemoryFactIdV1,
    operation_id: &str,
    content: &str,
    trust: f64,
) -> ProjectMemoryFactUpdateCommandV1 {
    ProjectMemoryFactUpdateCommandV1::new(
        target,
        ProvenanceId::new(operation_id.to_owned()).expect("canonical update operation identity"),
        None,
        ProjectMemoryFactUpdatePatchV1::new(
            Some(content.to_owned()),
            None,
            None,
            None,
            None,
            None,
            Some(Confidence::new(trust).expect("canonical update trust")),
        )
        .expect("canonical update patch"),
        None,
    )
    .expect("canonical update command")
}

#[tokio::test]
async fn exact_update_replay_returns_the_same_fact_delta_and_commit_receipt() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let target = added_target(&store, &control).await;
    let command = update_command(
        target,
        "operation.crud-update-exact-replay",
        "The canonical update is exactly replayable.",
        0.8,
    );

    let committed = store
        .update_project_memory_fact(command.clone(), &control)
        .await
        .expect("commit canonical update");
    let replayed = store
        .update_project_memory_fact(command, &control)
        .await
        .expect("replay canonical update");

    assert!(!committed.commit_replayed());
    assert!(replayed.commit_replayed());
    assert_eq!(committed.fact(), replayed.fact());
    assert_eq!(committed.trust_delta_millionths(), 300_000);
    assert_eq!(
        committed.trust_delta_millionths(),
        replayed.trust_delta_millionths()
    );
    assert_eq!(committed.commit_receipt(), replayed.commit_receipt());
}

#[tokio::test]
async fn reused_update_operation_with_changed_patch_conflicts_without_mutation() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let target = added_target(&store, &control).await;
    let operation_id = "operation.crud-update-input-conflict";
    let original = update_command(
        target.clone(),
        operation_id,
        "The original canonical update remains authoritative.",
        0.8,
    );
    let changed = update_command(
        target.clone(),
        operation_id,
        "A changed request cannot reuse the committed operation identity.",
        0.7,
    );

    let committed = store
        .update_project_memory_fact(original.clone(), &control)
        .await
        .expect("commit original canonical update");
    let conflict = store
        .update_project_memory_fact(changed, &control)
        .await
        .expect_err("changed input must conflict with the committed operation identity");
    assert!(matches!(conflict, FactStoreError::OperationConflict));

    let persisted = store
        .get_project_memory_fact(target, &read_control())
        .await
        .expect("read fact after changed-input conflict")
        .expect("canonical fact remains after changed-input conflict");
    let replayed = store
        .update_project_memory_fact(original, &control)
        .await
        .expect("replay original canonical update after conflict");

    assert_eq!(&persisted, committed.fact());
    assert_eq!(replayed.fact(), committed.fact());
    assert_eq!(replayed.trust_delta_millionths(), 300_000);
    assert_eq!(replayed.commit_receipt(), committed.commit_receipt());
    assert!(replayed.commit_replayed());
}

fn available_fact(projection: &ProjectMemoryFactProjectionV1) -> &ProjectMemoryFactV1 {
    match projection {
        ProjectMemoryFactProjectionV1::Available(fact) => fact,
        ProjectMemoryFactProjectionV1::Unavailable(_) => {
            panic!("fixture fact must remain available")
        }
    }
}

async fn add_fact(
    store: &DatabaseFactStore<'_>,
    control: &FactWriteControl,
    operation_id: &str,
    content: &str,
    source_label: Option<&str>,
) -> ProjectMemoryFactAddOutcomeV1 {
    store
        .add_project_memory_fact(
            accepted_add_command(FactOwnerV1::Profile, operation_id, content, source_label),
            control,
        )
        .await
        .expect("commit add fixture")
}

fn source_update_command(
    target: ProjectMemoryFactIdV1,
    operation_id: &str,
    source_label: Option<&str>,
) -> ProjectMemoryFactUpdateCommandV1 {
    ProjectMemoryFactUpdateCommandV1::new(
        target,
        ProvenanceId::new(operation_id.to_owned()).expect("source update operation id"),
        None,
        ProjectMemoryFactUpdatePatchV1::new(
            None,
            None,
            Some(source_label.map(ToOwned::to_owned)),
            None,
            None,
            None,
            None,
        )
        .expect("source update patch"),
        None,
    )
    .expect("source update command")
}

fn feedback_command(
    target: ProjectMemoryFactIdV1,
    operation_id: &str,
    source_label: Option<&str>,
    reason: Option<&str>,
) -> ProjectMemoryFactFeedbackCommandV1 {
    ProjectMemoryFactFeedbackCommandV1::new(
        target,
        ProvenanceId::new(operation_id.to_owned()).expect("feedback operation id"),
        None,
        ProjectMemoryFactFeedbackActionV1::Helpful,
        None,
        source_label.map(ToOwned::to_owned),
        reason.map(ToOwned::to_owned),
    )
    .expect("feedback command")
}

#[tokio::test]
async fn add_source_label_none_and_some_are_canonical_and_exactly_replayable() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let without_source = accepted_add_command(
        FactOwnerV1::Profile,
        "operation.add-source-none",
        "Canonical payload material omits an absent source label.",
        None,
    );
    let with_source = accepted_add_command(
        FactOwnerV1::Profile,
        "operation.add-source-some",
        "Canonical payload material includes a present source label.",
        Some("mcp"),
    );

    let none_committed = store
        .add_project_memory_fact(without_source.clone(), &control)
        .await
        .expect("commit source-less add");
    let none_replayed = store
        .add_project_memory_fact(without_source, &control)
        .await
        .expect("replay source-less add");
    let some_committed = store
        .add_project_memory_fact(with_source.clone(), &control)
        .await
        .expect("commit sourced add");
    let some_replayed = store
        .add_project_memory_fact(with_source, &control)
        .await
        .expect("replay sourced add");

    assert_eq!(available_fact(none_committed.fact()).source_label(), None);
    assert_eq!(
        available_fact(some_committed.fact()).source_label(),
        Some("mcp")
    );
    assert_eq!(
        none_committed.commit_receipt(),
        none_replayed.commit_receipt()
    );
    assert_eq!(
        some_committed.commit_receipt(),
        some_replayed.commit_receipt()
    );
    assert!(none_replayed.commit_replayed());
    assert!(some_replayed.commit_replayed());
}

#[test]
fn add_source_label_tamper_is_rejected_by_the_sanitization_receipt() {
    let content = "A source label is part of canonical payload material.";
    let material = json!({
        "content": content,
        "category": "project",
        "tags": ["idempotency"],
        "entities": ["TraceDecay"],
        "metadata": {"fixture": "canonical-update-replay"},
    });
    let receipt = match sanitize_memory_fact_payload(material).expect("sanitize add material") {
        MemoryFactSanitizationV1::Durable { receipt, .. } => receipt,
        MemoryFactSanitizationV1::Quarantined => panic!("fixture must be durable"),
    };
    let error = ProjectMemoryFactAddMaterialV1::new(
        FactOwnerV1::Profile,
        content.to_owned(),
        FactCategoryV1::Project,
        Some("tampered-source".to_owned()),
        vec!["idempotency".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({"fixture": "canonical-update-replay"}),
        receipt,
        None,
        Confidence::new(0.5).expect("trust"),
        None,
    )
    .expect_err("source-label tamper must invalidate the receipt");
    assert!(matches!(
        error,
        FactStoreError::Contract(DomainError::SnapshotMismatch { .. })
    ));
}

#[tokio::test]
async fn update_source_label_replays_and_changed_input_conflicts() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let target = added_target(&store, &control).await;
    let original = source_update_command(
        target.clone(),
        "operation.update-source-replay",
        Some("mcp"),
    );
    let changed = source_update_command(
        target.clone(),
        "operation.update-source-replay",
        Some("cli"),
    );

    let committed = store
        .update_project_memory_fact(original.clone(), &control)
        .await
        .expect("commit source update");
    let replayed = store
        .update_project_memory_fact(original, &control)
        .await
        .expect("replay source update");
    let conflict = store
        .update_project_memory_fact(changed, &control)
        .await
        .expect_err("changed source label must conflict");

    assert_eq!(available_fact(committed.fact()).source_label(), Some("mcp"));
    assert_eq!(committed.commit_receipt(), replayed.commit_receipt());
    assert!(replayed.commit_replayed());
    assert!(matches!(conflict, FactStoreError::OperationConflict));

    let cleared = source_update_command(target, "operation.update-source-clear", None);
    let clear_committed = store
        .update_project_memory_fact(cleared.clone(), &control)
        .await
        .expect("clear source label");
    let clear_replayed = store
        .update_project_memory_fact(cleared, &control)
        .await
        .expect("replay source clear");
    assert_eq!(available_fact(clear_committed.fact()).source_label(), None);
    assert_eq!(
        clear_committed.commit_receipt(),
        clear_replayed.commit_receipt()
    );
}

#[tokio::test]
async fn normalized_equivalent_add_is_the_only_no_write_near_duplicate() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let first = add_fact(
        &store,
        &control,
        "operation.add-normalized-base",
        "Use pnpm for workspace installs",
        None,
    )
    .await;
    let duplicate = add_fact(
        &store,
        &control,
        "operation.add-normalized-duplicate",
        "  use   PNPM for workspace installs  ",
        None,
    )
    .await;

    assert_eq!(
        duplicate.disposition(),
        ProjectMemoryFactAddDispositionV1::NearDuplicate
    );
    assert_eq!(duplicate.fact().fact_id(), first.fact().fact_id());
    assert_eq!(
        duplicate
            .closest_fact_id()
            .map(ProjectMemoryFactIdV1::fact_id),
        Some(first.fact().fact_id())
    );
    assert!(duplicate.commit_receipt().is_none());
    assert!(!duplicate.commit_replayed());
}

#[tokio::test]
async fn semantic_near_duplicate_is_inserted_with_a_replayable_receipt() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let first = add_fact(
        &store,
        &control,
        "operation.add-semantic-base",
        "The deployment uses PostgreSQL for durable state in production",
        None,
    )
    .await;
    let command = accepted_add_command(
        FactOwnerV1::Profile,
        "operation.add-semantic-near",
        "The deployment uses PostgreSQL for durable state in production.",
        None,
    );
    let committed = store
        .add_project_memory_fact(command.clone(), &control)
        .await
        .expect("commit semantic near duplicate");
    let replayed = store
        .add_project_memory_fact(command, &control)
        .await
        .expect("replay semantic near duplicate");

    assert_eq!(
        committed.disposition(),
        ProjectMemoryFactAddDispositionV1::NearDuplicate
    );
    assert_ne!(committed.fact().fact_id(), first.fact().fact_id());
    assert_eq!(
        committed
            .closest_fact_id()
            .map(ProjectMemoryFactIdV1::fact_id),
        Some(first.fact().fact_id())
    );
    assert!(committed.commit_receipt().is_some());
    assert_eq!(committed.commit_receipt(), replayed.commit_receipt());
    assert!(replayed.commit_replayed());
}

#[tokio::test]
async fn high_similarity_change_cue_commits_as_possible_conflict() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let first = add_fact(
        &store,
        &control,
        "operation.add-conflict-base",
        "The deployment uses Redis for durable cache state in production",
        None,
    )
    .await;
    let conflict = add_fact(
        &store,
        &control,
        "operation.add-conflict-cue",
        "The deployment no longer uses Redis for durable cache state in production",
        None,
    )
    .await;

    assert_eq!(
        conflict.disposition(),
        ProjectMemoryFactAddDispositionV1::PossibleConflict
    );
    assert_ne!(conflict.fact().fact_id(), first.fact().fact_id());
    assert_eq!(
        conflict
            .closest_fact_id()
            .map(ProjectMemoryFactIdV1::fact_id),
        Some(first.fact().fact_id())
    );
    assert!(conflict.commit_receipt().is_some());
}

#[tokio::test]
async fn feedback_history_preserves_source_only_reason_only_neither_and_redaction() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let target = added_target(&store, &control).await;
    let commands = [
        feedback_command(
            target.clone(),
            "operation.feedback-source-only",
            Some("mcp"),
            None,
        ),
        feedback_command(
            target.clone(),
            "operation.feedback-reason-only",
            None,
            Some("confirmed by the operator"),
        ),
        feedback_command(target.clone(), "operation.feedback-neither", None, None),
        feedback_command(
            target.clone(),
            "operation.feedback-redacted",
            Some("vault_passphrase: ordinary-value\n  broken: [unclosed"),
            None,
        ),
    ];
    for (index, command) in commands.into_iter().enumerate() {
        let committed = store
            .record_project_memory_fact_feedback(command.clone(), &control)
            .await
            .expect("record feedback fixture");
        if index == 0 {
            let replayed = store
                .record_project_memory_fact_feedback(command, &control)
                .await
                .expect("replay source-only feedback");
            assert_eq!(committed.commit_receipt(), replayed.commit_receipt());
            assert!(replayed.commit_replayed());
        }
    }

    let history = store
        .project_memory_fact_feedback_history(
            ProjectMemoryFactFeedbackHistoryQueryV1::new(target, None, 10)
                .expect("feedback history query"),
            &read_control(),
        )
        .await
        .expect("read feedback history");
    assert_eq!(history.events().len(), 4);
    assert!(history.events().iter().any(|event| {
        event.source() == Some("mcp")
            && event.note().is_none()
            && event.details_availability()
                == ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available
    }));
    assert!(history.events().iter().any(|event| {
        event.source().is_none()
            && event.note() == Some("confirmed by the operator")
            && event.details_availability()
                == ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available
    }));
    assert!(history.events().iter().any(|event| {
        event.source().is_none()
            && event.note().is_none()
            && event.details_availability()
                == ProjectMemoryFactFeedbackDetailsAvailabilityV1::Unknown
    }));
    assert!(history.events().iter().any(|event| {
        event.source().is_none()
            && event.note().is_none()
            && event.details_availability()
                == ProjectMemoryFactFeedbackDetailsAvailabilityV1::Redacted
    }));
}

async fn tamper_operation_receipt(
    database: &Database,
    operation_id: &str,
    assignment: &str,
    value: &str,
) {
    let transaction = database
        .begin_memory_write_transaction("tamper canonical replay receipt")
        .await
        .expect("begin receipt tamper transaction");
    transaction
        .execute_batch("DROP TRIGGER memory_v2_operation_receipts_no_update;")
        .await
        .expect("disable receipt immutability in isolated tamper fixture");
    let sql =
        format!("UPDATE memory_v2_operation_receipts SET {assignment} WHERE operation_id = ?2");
    assert_eq!(
        transaction
            .execute(&sql, params![value, operation_id])
            .await
            .expect("tamper operation receipt"),
        1
    );
    transaction
        .execute_batch(
            "CREATE TRIGGER memory_v2_operation_receipts_no_update
             BEFORE UPDATE ON memory_v2_operation_receipts BEGIN
                 SELECT RAISE(ABORT, 'memory_v2 operation receipts are immutable');
             END;",
        )
        .await
        .expect("restore receipt immutability after isolated tamper");
    transaction
        .commit()
        .await
        .expect("commit receipt tamper transaction");
}

#[tokio::test]
async fn replay_rejects_event_id_not_owned_by_the_authoritative_lineage() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let target = added_target(&store, &control).await;
    let command = source_update_command(
        target,
        "operation.update-tampered-event-receipt",
        Some("mcp"),
    );
    store
        .update_project_memory_fact(command.clone(), &control)
        .await
        .expect("commit update before receipt tamper");
    tamper_operation_receipt(
        &database,
        "operation.update-tampered-event-receipt",
        "receipt_json = json_set(receipt_json, '$.committed_event_ids', json_array(?1, event_id))",
        "event.missing-authority",
    )
    .await;

    let error = store
        .update_project_memory_fact(command, &control)
        .await
        .expect_err("replay must reject a non-authoritative event id");
    assert!(matches!(error, FactStoreError::InvalidCommitReceipt));
}

#[tokio::test]
async fn replay_rejects_assertion_id_not_owned_by_the_authoritative_fact() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let target = added_target(&store, &control).await;
    let command = source_update_command(
        target,
        "operation.update-tampered-assertion-receipt",
        Some("mcp"),
    );
    store
        .update_project_memory_fact(command.clone(), &control)
        .await
        .expect("commit update before assertion tamper");
    tamper_operation_receipt(
        &database,
        "operation.update-tampered-assertion-receipt",
        "receipt_json = json_set(receipt_json, '$.active_assertion_id', ?1)",
        "assertion.missing-authority",
    )
    .await;

    let error = store
        .update_project_memory_fact(command, &control)
        .await
        .expect_err("replay must reject a non-authoritative assertion id");
    assert!(matches!(error, FactStoreError::InvalidCommitReceipt));
}

fn remove_command(
    target: ProjectMemoryFactIdV1,
    operation_id: &str,
    expected_last_event_id: Option<FactEventId>,
) -> ProjectMemoryFactRemoveCommandV1 {
    ProjectMemoryFactRemoveCommandV1::new(
        target,
        ProvenanceId::new(operation_id.to_owned()).expect("remove operation id"),
        expected_last_event_id,
        None,
    )
    .expect("remove command")
}

#[tokio::test]
async fn already_removed_noop_reserves_operation_id_and_deleted_writes_are_typed() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let control = write_control();
    let target = added_target(&store, &control).await;
    let remove = remove_command(target.clone(), "operation.remove-commit", None);
    let removed = store
        .remove_project_memory_fact(remove.clone(), &control)
        .await
        .expect("remove canonical fact");
    let removed_replay = store
        .remove_project_memory_fact(remove, &control)
        .await
        .expect("replay canonical removal");
    assert_eq!(removed.commit_receipt(), removed_replay.commit_receipt());
    assert!(removed_replay.commit_replayed());
    let noop = remove_command(target.clone(), "operation.remove-noop", None);
    let first = store
        .remove_project_memory_fact(noop.clone(), &control)
        .await
        .expect("reserve already-removed operation");
    let replay = store
        .remove_project_memory_fact(noop, &control)
        .await
        .expect("replay already-removed operation");
    assert!(!first.was_removed());
    assert!(!replay.was_removed());
    assert_eq!(first.fact(), replay.fact());
    assert!(first.commit_receipt().is_none());

    let changed = remove_command(
        target.clone(),
        "operation.remove-noop",
        Some(FactEventId::new("event.changed-remove-input".to_owned()).expect("event id")),
    );
    let conflict = store
        .remove_project_memory_fact(changed, &control)
        .await
        .expect_err("reserved no-op id must reject changed input");
    assert!(matches!(conflict, FactStoreError::OperationConflict));

    let update_error = store
        .update_project_memory_fact(
            source_update_command(
                target.clone(),
                "operation.update-deleted-target",
                Some("mcp"),
            ),
            &control,
        )
        .await
        .expect_err("deleted update target must be denied");
    assert!(matches!(
        update_error,
        FactStoreError::FactDeleted { fact_id } if fact_id == *target.fact_id()
    ));
    let feedback_error = store
        .record_project_memory_fact_feedback(
            feedback_command(
                target.clone(),
                "operation.feedback-deleted-target",
                None,
                None,
            ),
            &control,
        )
        .await
        .expect_err("deleted feedback target must be denied");
    assert!(matches!(
        feedback_error,
        FactStoreError::FactDeleted { fact_id } if fact_id == *target.fact_id()
    ));
}

#[tokio::test]
async fn missing_and_unavailable_write_targets_return_typed_errors() {
    let (_source_directory, source_database) = database().await;
    let source_store = DatabaseFactStore::new(&source_database);
    let control = write_control();
    let target = added_target(&source_store, &control).await;

    let (_empty_directory, empty_database) = database().await;
    let empty_store = DatabaseFactStore::new(&empty_database);
    let missing = empty_store
        .update_project_memory_fact(
            source_update_command(
                target.clone(),
                "operation.update-missing-target",
                Some("mcp"),
            ),
            &control,
        )
        .await
        .expect_err("missing update target must be typed");
    assert!(matches!(
        missing,
        FactStoreError::FactNotFound { fact_id } if fact_id == *target.fact_id()
    ));

    let transaction = source_database
        .begin_memory_write_transaction("mark fact unavailable for typed denial")
        .await
        .expect("begin unavailable fixture transaction");
    assert_eq!(
        transaction
            .execute(
                "UPDATE memory_v2_current_facts SET payload_access = 'unavailable'
                 WHERE fact_id = ?1",
                params![target.fact_id().as_str()],
            )
            .await
            .expect("mark current fact unavailable"),
        1
    );
    transaction
        .commit()
        .await
        .expect("commit unavailable fixture");
    let unavailable = source_store
        .record_project_memory_fact_feedback(
            feedback_command(
                target.clone(),
                "operation.feedback-unavailable-target",
                None,
                None,
            ),
            &control,
        )
        .await
        .expect_err("unavailable feedback target must be typed");
    assert!(matches!(
        unavailable,
        FactStoreError::FactUnavailable { fact_id } if fact_id == *target.fact_id()
    ));
}

#[tokio::test]
async fn project_memory_read_helpers_reject_a_pre_cancelled_control() {
    let (_directory, database) = database().await;
    let store = DatabaseFactStore::new(&database);
    let target = added_target(&store, &write_control()).await;
    let cancelled = cancelled_read_control();
    let transaction = database
        .begin_memory_read_transaction("pre-cancelled project-memory read helpers")
        .await
        .expect("begin cancelled read fixture transaction");

    let list_query = ProjectMemoryFactListQueryV1::new(FactOwnerV1::Profile, None, None, None, 10)
        .expect("cancelled list query");
    assert!(matches!(
        list_project_memory_facts_controlled_tx(&transaction, &list_query, &cancelled).await,
        Err(FactStoreError::ReadCancelled)
    ));
    assert!(matches!(
        get_project_memory_fact_controlled_tx(&transaction, &target, &cancelled).await,
        Err(FactStoreError::ReadCancelled)
    ));

    let digest_query = ProjectMemoryFactContentDigestQueryV1::new(
        FactOwnerV1::Profile,
        LocatorDigest::new(format!("sha256:{}", "0".repeat(64))).expect("cancelled content digest"),
    )
    .expect("cancelled content digest query");
    assert!(matches!(
        find_project_memory_fact_by_content_digest_controlled_tx(
            &transaction,
            &digest_query,
            &cancelled,
        )
        .await,
        Err(FactStoreError::ReadCancelled)
    ));

    let history_query = ProjectMemoryFactHistoryQueryV1::new(target.clone(), None, 10)
        .expect("cancelled history query");
    assert!(matches!(
        project_memory_fact_history_controlled_tx(&transaction, &history_query, &cancelled).await,
        Err(FactStoreError::ReadCancelled)
    ));
    assert!(matches!(
        inspect_project_memory_fact_controlled_tx(&transaction, &target, &cancelled).await,
        Err(FactStoreError::ReadCancelled)
    ));

    transaction
        .rollback()
        .await
        .expect("rollback cancelled read fixture transaction");
}
