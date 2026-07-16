use libsql::params;
use serde_json::Value;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, ComponentVersion, DurableObservationV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
};
use tracedecay_store::{
    ObservationProjectionStore, ObservationStore, ObservationWrite, ProjectionPersistOutcome,
    SESSION_MESSAGE_PROJECTOR_VERSION_V2, SESSION_MESSAGE_PROJECTOR_VERSION_V3,
};

use crate::global_db::GlobalDb;
use crate::sessions::cursor_composer::{
    normalize_cursor_composer_observation,
    normalize_cursor_composer_observation_with_projected_message_id,
};
use crate::store::GlobalDbObservationStore;

fn durable_fixture_observation(
    envelope: CanonicalObservationEnvelopeV1,
    range: ObservationSourceRangeV1,
    generation: u64,
    ordering_domain: ObservationOrderingDomainV1,
    record_id: ObservationId,
    receipt_id: &str,
) -> DurableObservationV1 {
    let source = ObservationSourceIdentityV1::for_provider(
        envelope.provider().clone(),
        envelope.relations().session_id().clone(),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.migration-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .unwrap();
    DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(generation).unwrap(),
            range,
            ordering_domain,
            record_id,
        )
        .unwrap(),
        receipt,
        RetentionClass::new("retention.migration-test").unwrap(),
        payload,
    )
    .unwrap()
}

fn checked_in_v2_observations() -> Vec<DurableObservationV1> {
    let mut composer_native: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble.input.json"
    ))
    .unwrap();
    composer_native.as_object_mut().unwrap().insert(
        "tracedecayProjectPath".to_owned(),
        Value::String("/workspace/project".to_owned()),
    );
    let composer_range = ObservationSourceRangeV1::new(1, 2).unwrap();
    let composer_record = ObservationId::new("record.cursor-composer.v2-v3").unwrap();
    let composer = normalize_cursor_composer_observation(
        &composer_native,
        "comp-1",
        composer_record.clone(),
        composer_range,
        1,
    )
    .unwrap();

    let mut todos_native: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble_with_todos.input.json"
    ))
    .unwrap();
    todos_native.as_object_mut().unwrap().insert(
        "tracedecayProjectPath".to_owned(),
        Value::String("/workspace/project".to_owned()),
    );
    let todos_range = ObservationSourceRangeV1::new(2, 3).unwrap();
    let todos_record = ObservationId::new("record.cursor-composer-todos.v2-v3").unwrap();
    let todos = normalize_cursor_composer_observation(
        &todos_native,
        "comp-todos",
        todos_record.clone(),
        todos_range,
        2,
    )
    .unwrap();

    vec![
        durable_fixture_observation(
            composer,
            composer_range,
            1,
            ObservationOrderingDomainV1::SnapshotOrder,
            composer_record,
            "receipt.cursor-composer.v2-v3",
        ),
        durable_fixture_observation(
            todos,
            todos_range,
            1,
            ObservationOrderingDomainV1::SnapshotOrder,
            todos_record,
            "receipt.cursor-composer-todos.v2-v3",
        ),
        durable_fixture_observation(
            serde_json::from_str::<CanonicalObservationEnvelopeV1>(&include_str!(
                "../../../tests/fixtures/provider_normalization/codex/session_meta.expected_envelope.json"
            )
            .replace("$STABLE_RECORD_ID", "record.codex-session-meta.v2-v3"))
            .unwrap(),
            ObservationSourceRangeV1::new(0, 1).unwrap(),
            1,
            ObservationOrderingDomainV1::FileBytes,
            ObservationId::new("record.codex-session-meta.v2-v3").unwrap(),
            "receipt.codex-session-meta.v2-v3",
        ),
        durable_fixture_observation(
            normalize_cursor_composer_observation(
                &todos_native,
                "comp-later",
                ObservationId::new("record.cursor-composer-later.v2-v3").unwrap(),
                ObservationSourceRangeV1::new(3, 4).unwrap(),
                3,
            )
            .unwrap(),
            ObservationSourceRangeV1::new(3, 4).unwrap(),
            1,
            ObservationOrderingDomainV1::SnapshotOrder,
            ObservationId::new("record.cursor-composer-later.v2-v3").unwrap(),
            "receipt.cursor-composer-later.v2-v3",
        ),
    ]
}

fn checked_in_codex_session_boundary(index: usize) -> DurableObservationV1 {
    let record_id = format!("record.codex-session-meta.page-{index}");
    let session_id = format!("codex-migration-page-{index}");
    let mut fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/provider_normalization/codex/session_meta.expected_envelope.json"
    ))
    .unwrap();
    fixture["stable_record_id"] = Value::String(record_id.clone());
    fixture["relations"]["session_id"] = Value::String(session_id.clone());
    fixture["relations"]["thread_id"] = Value::String(session_id);
    durable_fixture_observation(
        serde_json::from_value(fixture).unwrap(),
        ObservationSourceRangeV1::new(0, 1).unwrap(),
        1,
        ObservationOrderingDomainV1::FileBytes,
        ObservationId::new(record_id).unwrap(),
        &format!("receipt.codex-session-meta.page-{index}"),
    )
}

fn write(observation: DurableObservationV1) -> ObservationWrite {
    write_after(observation, None)
}

fn write_after(
    observation: DurableObservationV1,
    previous_cursor: Option<ObservationSourceCursorV1>,
) -> ObservationWrite {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    ObservationWrite::new(observation, previous_cursor, next_cursor).unwrap()
}

fn cursor_after(observation: &DurableObservationV1) -> ObservationSourceCursorV1 {
    let identity = observation.identity();
    ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap()
}

fn composer_rollover_observation(
    generation: u64,
    text: &str,
    receipt_id: &str,
) -> DurableObservationV1 {
    let mut native: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble.input.json"
    ))
    .unwrap();
    native["text"] = Value::String(text.to_owned());
    native["tracedecayProjectPath"] = Value::String("/workspace/project".to_owned());
    let range = ObservationSourceRangeV1::new(1, 2).unwrap();
    let record_id = ObservationId::new(format!(
        "record.cursor-composer-rollover.v2-v3.generation-{generation}"
    ))
    .unwrap();
    durable_fixture_observation(
        normalize_cursor_composer_observation_with_projected_message_id(
            &native,
            "comp-rollover",
            record_id.clone(),
            ObservationId::new("record.cursor-composer-rollover.v2-v3").unwrap(),
            range,
            1,
        )
        .unwrap(),
        range,
        generation,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
        receipt_id,
    )
}

#[tokio::test]
async fn v2_upgrade_materializes_the_complete_v3_effect_before_authority_audit() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let store = GlobalDbObservationStore::new(&db);
    for observation in checked_in_v2_observations() {
        store.persist_observation(write(observation)).await.unwrap();
    }
    for _ in 0..3 {
        let queued = store.next_queued_observation().await.unwrap().unwrap();
        assert!(matches!(
            store.project_observation(&queued).await.unwrap(),
            ProjectionPersistOutcome::Projected(_) | ProjectionPersistOutcome::Skipped { .. }
        ));
    }
    assert!(store.next_queued_observation().await.unwrap().is_some());
    let mut initial_rows = db
        .conn
        .query(
            "SELECT
                (SELECT COUNT(*) FROM observation_projection_provenance
                 WHERE projector_version = ?1),
                (SELECT COUNT(*) FROM observation_workflow_facts
                 WHERE projector_version = ?1),
                (SELECT COUNT(*) FROM session_messages WHERE provider = 'cursor'),
                (SELECT COUNT(*) FROM observation_projection_dispositions
                 WHERE projector_version = ?1),
                (SELECT COALESCE(SUM(message_created), 0)
                 FROM observation_projection_provenance WHERE projector_version = ?1),
                (SELECT last_sequence FROM observation_projection_checkpoints
                 WHERE projector_version = ?1),
                (SELECT COUNT(*) FROM projection_queue)",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V3],
        )
        .await
        .unwrap();
    let initial = initial_rows.next().await.unwrap().unwrap();
    let expected_provenance = initial.get::<i64>(0).unwrap();
    let expected_workflow = initial.get::<i64>(1).unwrap();
    let expected_messages = initial.get::<i64>(2).unwrap();
    let expected_dispositions = initial.get::<i64>(3).unwrap();
    let expected_owned_outputs = initial.get::<i64>(4).unwrap();
    let expected_checkpoint = initial.get::<i64>(5).unwrap();
    assert!(
        expected_provenance > 2,
        "fixtures must exercise V3 multi-output expansion"
    );
    assert!(
        expected_workflow > 0,
        "fixture must exercise V3 workflow expansion"
    );
    assert_eq!(expected_dispositions, 1);
    assert_eq!(expected_checkpoint, 3);
    assert_eq!(initial.get::<i64>(6).unwrap(), 1);
    drop(initial);
    drop(initial_rows);

    db.conn
        .execute_batch(
            "DELETE FROM observation_workflow_facts;
             DELETE FROM session_messages
             WHERE provider = 'cursor'
               AND message_id NOT IN (
                   'record.cursor-composer.v2-v3',
                   'record.cursor-composer-todos.v2-v3'
             );
             DELETE FROM observation_projection_provenance WHERE output_ordinal > 0;
             DROP TRIGGER IF EXISTS projection_output_audit_invalidate_update_v1;
             DROP TRIGGER IF EXISTS projection_output_audit_invalidate_delete_v1;",
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE observation_projection_dispositions SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V3
            ],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE observation_projection_provenance SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V3
            ],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE observation_projection_checkpoints SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V3
            ],
        )
        .await
        .unwrap();
    db.conn
        .execute_batch(
            "CREATE TABLE observation_projection_provenance_v2 (
                projector_version TEXT NOT NULL,
                observation_id TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                output_provider TEXT NOT NULL,
                output_message_id TEXT NOT NULL,
                output_digest TEXT NOT NULL,
                message_created INTEGER NOT NULL CHECK(message_created IN (0, 1)),
                PRIMARY KEY(projector_version, observation_id),
                UNIQUE(projector_version, output_provider, output_message_id),
                FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
                FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
             );
             INSERT INTO observation_projection_provenance_v2 (
                projector_version, observation_id, receipt_id, output_provider,
                output_message_id, output_digest, message_created
             ) SELECT projector_version, observation_id, receipt_id, output_provider,
                      output_message_id, output_digest, message_created
               FROM observation_projection_provenance;
             DROP TABLE observation_projection_provenance;
             ALTER TABLE observation_projection_provenance_v2
                RENAME TO observation_projection_provenance;",
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO observation_projection_checkpoints (
                projector_version, last_sequence
             ) VALUES (?1, 900)",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V3],
        )
        .await
        .unwrap();
    drop(db);

    let reopened = GlobalDb::try_open_at(&db_path).await.unwrap().unwrap();
    reopened.audit_observation_authority().await.unwrap();
    let mut rows = reopened
        .conn
        .query(
            "SELECT
                (SELECT COUNT(*) FROM observation_projection_provenance
                 WHERE projector_version = ?1),
                (SELECT COUNT(*) FROM observation_workflow_facts
                 WHERE projector_version = ?1),
                (SELECT COUNT(*) FROM session_messages WHERE provider = 'cursor'),
                (SELECT COUNT(*) FROM observation_projection_dispositions
                 WHERE projector_version = ?1),
                (SELECT COALESCE(SUM(message_created), 0)
                 FROM observation_projection_provenance WHERE projector_version = ?1),
                (SELECT COALESCE(SUM(message_created), 0)
                 FROM observation_projection_provenance
                 WHERE projector_version = ?2),
                (SELECT COUNT(*) FROM projection_queue),
                (SELECT last_sequence FROM observation_projection_checkpoints
                 WHERE projector_version = ?1)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                SESSION_MESSAGE_PROJECTOR_VERSION_V2
            ],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), expected_provenance);
    assert_eq!(row.get::<i64>(1).unwrap(), expected_workflow);
    assert_eq!(row.get::<i64>(2).unwrap(), expected_messages);
    assert_eq!(row.get::<i64>(3).unwrap(), expected_dispositions);
    assert_eq!(row.get::<i64>(4).unwrap(), expected_owned_outputs - 2);
    assert_eq!(row.get::<i64>(5).unwrap(), 2);
    assert_eq!(row.get::<i64>(6).unwrap(), 1);
    assert_eq!(row.get::<i64>(7).unwrap(), expected_checkpoint);
    drop(row);
    drop(rows);

    let store = GlobalDbObservationStore::new(&reopened);
    let mut projected = 0;
    while let Some(queued) = store.next_queued_observation().await.unwrap() {
        assert!(matches!(
            store.project_observation(&queued).await.unwrap(),
            ProjectionPersistOutcome::Projected(_)
        ));
        projected += 1;
    }
    assert_eq!(projected, 1);
    drop(reopened);

    let converged = GlobalDb::try_open_at(&db_path).await.unwrap().unwrap();
    converged.audit_observation_authority().await.unwrap();
    let mut converged_rows = converged
        .conn
        .query(
            "SELECT
                (SELECT COUNT(*) FROM projection_queue),
                (SELECT last_sequence FROM observation_projection_checkpoints
                 WHERE projector_version = ?1)",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V3],
        )
        .await
        .unwrap();
    let converged_row = converged_rows.next().await.unwrap().unwrap();
    assert_eq!(converged_row.get::<i64>(0).unwrap(), 0);
    assert_eq!(converged_row.get::<i64>(1).unwrap(), 4);
}

#[tokio::test]
async fn v2_upgrade_preserves_changed_generation_lineage_and_future_supersession() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let store = GlobalDbObservationStore::new(&db);
    let first = composer_rollover_observation(
        1,
        "First generation body.",
        "receipt.cursor-composer-rollover.first",
    );
    let first_cursor = cursor_after(&first);
    store.persist_observation(write(first)).await.unwrap();
    let queued = store.next_queued_observation().await.unwrap().unwrap();
    store.project_observation(&queued).await.unwrap();

    let second = composer_rollover_observation(
        2,
        "Second generation body.",
        "receipt.cursor-composer-rollover.second",
    );
    let second_cursor = cursor_after(&second);
    store
        .persist_observation(write_after(second, Some(first_cursor)))
        .await
        .unwrap();
    let queued = store.next_queued_observation().await.unwrap().unwrap();
    store.project_observation(&queued).await.unwrap();
    db.conn
        .execute(
            "UPDATE observation_projection_provenance SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V3
            ],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE observation_projection_checkpoints SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V3
            ],
        )
        .await
        .unwrap();
    drop(db);

    let reopened = GlobalDb::try_open_at(&db_path).await.unwrap().unwrap();
    reopened.audit_observation_authority().await.unwrap();
    let mut rows = reopened
        .conn
        .query(
            "SELECT
                (SELECT text FROM session_messages
                 WHERE provider = 'cursor'
                   AND message_id = 'record.cursor-composer-rollover.v2-v3'),
                (SELECT COALESCE(SUM(message_created), 0)
                 FROM observation_projection_provenance
                 WHERE projector_version = ?1),
                (SELECT COALESCE(SUM(message_created), 0)
                 FROM observation_projection_provenance
                 WHERE projector_version = ?2)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V3
            ],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "Second generation body.");
    assert!(row.get::<i64>(1).unwrap() > 0);
    assert_eq!(row.get::<i64>(2).unwrap(), 0);
    drop(row);
    drop(rows);

    let third = composer_rollover_observation(
        3,
        "Third generation body.",
        "receipt.cursor-composer-rollover.third",
    );
    let store = GlobalDbObservationStore::new(&reopened);
    store
        .persist_observation(write_after(third, Some(second_cursor)))
        .await
        .unwrap();
    let queued = store.next_queued_observation().await.unwrap().unwrap();
    store.project_observation(&queued).await.unwrap();
    let mut text_rows = reopened
        .conn
        .query(
            "SELECT text FROM session_messages
             WHERE provider = 'cursor'
               AND message_id = 'record.cursor-composer-rollover.v2-v3'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        text_rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "Third generation body."
    );
}

#[tokio::test]
async fn v2_upgrade_runs_one_page_per_open_and_resumes() {
    const PREDECESSOR_ROWS: usize = 257;

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let store = GlobalDbObservationStore::new(&db);
    for index in 0..PREDECESSOR_ROWS {
        store
            .persist_observation(write(checked_in_codex_session_boundary(index)))
            .await
            .unwrap();
    }
    while let Some(queued) = store.next_queued_observation().await.unwrap() {
        assert!(matches!(
            store.project_observation(&queued).await.unwrap(),
            ProjectionPersistOutcome::Skipped { .. }
        ));
    }
    db.conn
        .execute(
            "UPDATE observation_projection_dispositions SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V3
            ],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE observation_projection_checkpoints SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V3
            ],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO observation_projection_checkpoints (
                projector_version, last_sequence
             ) VALUES (?1, 900)",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V3],
        )
        .await
        .unwrap();
    drop(db);

    let first_open = GlobalDb::try_open_at(&db_path).await.unwrap().unwrap();
    first_open.audit_observation_authority().await.unwrap();
    let mut page_rows = first_open
        .conn
        .query(
            "SELECT
                (SELECT last_sequence FROM observation_projection_checkpoints
                 WHERE projector_version = ?1),
                (SELECT COUNT(*) FROM observation_projection_dispositions
                 WHERE projector_version = ?1),
                (SELECT migrated_through FROM observation_projection_migrations
                 WHERE source_projector_version = ?2
                   AND target_projector_version = ?1),
                (SELECT completed FROM observation_projection_migrations
                 WHERE source_projector_version = ?2
                   AND target_projector_version = ?1)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                SESSION_MESSAGE_PROJECTOR_VERSION_V2
            ],
        )
        .await
        .unwrap();
    let page = page_rows.next().await.unwrap().unwrap();
    assert_eq!(page.get::<i64>(0).unwrap(), 128);
    assert_eq!(page.get::<i64>(1).unwrap(), 128);
    assert_eq!(page.get::<i64>(2).unwrap(), 128);
    assert_eq!(page.get::<i64>(3).unwrap(), 0);
    drop(page);
    drop(page_rows);
    drop(first_open);

    let second_open = GlobalDb::try_open_at(&db_path).await.unwrap().unwrap();
    second_open.audit_observation_authority().await.unwrap();
    let mut rows = second_open
        .conn
        .query(
            "SELECT
                (SELECT last_sequence FROM observation_projection_checkpoints
                 WHERE projector_version = ?1),
                (SELECT COUNT(*) FROM observation_projection_dispositions
                 WHERE projector_version = ?1),
                (SELECT COUNT(*) FROM projection_queue),
                (SELECT migrated_through FROM observation_projection_migrations
                 WHERE source_projector_version = ?2
                   AND target_projector_version = ?1),
                (SELECT completed FROM observation_projection_migrations
                 WHERE source_projector_version = ?2
                   AND target_projector_version = ?1)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                SESSION_MESSAGE_PROJECTOR_VERSION_V2
            ],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 256);
    assert_eq!(row.get::<i64>(1).unwrap(), 256);
    assert_eq!(row.get::<i64>(2).unwrap(), 1);
    assert_eq!(row.get::<i64>(3).unwrap(), 256);
    assert_eq!(row.get::<i64>(4).unwrap(), 0);
    drop(row);
    drop(rows);
    drop(second_open);

    let final_open = GlobalDb::try_open_at(&db_path).await.unwrap().unwrap();
    final_open.audit_observation_authority().await.unwrap();
    let mut rows = final_open
        .conn
        .query(
            "SELECT
                (SELECT last_sequence FROM observation_projection_checkpoints
                 WHERE projector_version = ?1),
                (SELECT COUNT(*) FROM observation_projection_dispositions
                 WHERE projector_version = ?1),
                (SELECT COUNT(*) FROM projection_queue),
                (SELECT migrated_through FROM observation_projection_migrations
                 WHERE source_projector_version = ?2
                   AND target_projector_version = ?1),
                (SELECT completed FROM observation_projection_migrations
                 WHERE source_projector_version = ?2
                   AND target_projector_version = ?1)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                SESSION_MESSAGE_PROJECTOR_VERSION_V2
            ],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), PREDECESSOR_ROWS as i64);
    assert_eq!(row.get::<i64>(1).unwrap(), PREDECESSOR_ROWS as i64);
    assert_eq!(row.get::<i64>(2).unwrap(), 0);
    assert_eq!(row.get::<i64>(3).unwrap(), PREDECESSOR_ROWS as i64);
    assert_eq!(row.get::<i64>(4).unwrap(), 1);
    drop(row);
    drop(rows);
    drop(final_open);

    let converged = GlobalDb::try_open_at(&db_path).await.unwrap().unwrap();
    converged.audit_observation_authority().await.unwrap();
}
