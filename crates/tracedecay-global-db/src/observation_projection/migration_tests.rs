use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use serde_json::Value;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, ComponentVersion, DurableObservationV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId, ProviderId,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_rusqlite_runtime::migration_sql::{
    MigrationSqlError, MigrationSqlWriteAuthority, MigrationSqlWriteIntent,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
    ProjectionPersistOutcome, SESSION_MESSAGE_PROJECTOR_VERSION_V1,
    SESSION_MESSAGE_PROJECTOR_VERSION_V2, SESSION_MESSAGE_PROJECTOR_VERSION_V3,
    SESSION_MESSAGE_PROJECTOR_VERSION_V4, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};

use super::prepare_projection_version_migration_with_engine;
use crate::RegisteredGlobalDb;
use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_runtime_core::db::engine::{TestConnection, params};
use tracedecay_sessions::runtime::cursor_composer::{
    normalize_cursor_composer_observation,
    normalize_cursor_composer_observation_with_projected_message_id,
};

async fn registered_runtime(
    profile_root: &std::path::Path,
) -> tracedecay_runtime_core::errors::Result<HostAdmissionTestRuntimeV1> {
    HostAdmissionTestRuntimeV1::profile(profile_root).await
}

fn registered_database(runtime: &HostAdmissionTestRuntimeV1) -> &RegisteredGlobalDb {
    runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile observation database")
}

async fn audit_observation_authority(runtime: &HostAdmissionTestRuntimeV1) {
    let snapshot = registered_database(runtime).read_snapshot().await.unwrap();
    crate::schema_stages::validate_observation_authority_connection(&snapshot)
        .await
        .unwrap();
}

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
        "../../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble.input.json"
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
        "../../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble_with_todos.input.json"
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
                "../../../../tests/fixtures/provider_normalization/codex/session_meta.expected_envelope.json"
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
        "../../../../tests/fixtures/provider_normalization/codex/session_meta.expected_envelope.json"
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

fn write(observation: DurableObservationV1) -> AnchoredObservationWrite {
    write_after(observation, None)
}

fn write_after(
    observation: DurableObservationV1,
    previous_cursor: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, previous_cursor, next_cursor).unwrap();
    let generation = ProjectionGenerationId::new("projection.migration-test.v4").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "migration-test")
            .unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, generation).unwrap()
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
        "../../../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble.input.json"
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

const LEGACY_CLAUDE_SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const LEGACY_CLAUDE_MESSAGE_ID: &str = "22222222-2222-4222-8222-222222222222";
const LEGACY_CLAUDE_SOURCE_KEY: &str = concat!(
    "tracedecay-claude-observation-source-v1-sha256-",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
);

fn legacy_claude_source_key_observation() -> DurableObservationV1 {
    let payload = serde_json::json!({
        "cwd": "/workspace/project",
        "message": {
            "content": "Synthetic legacy projection payload.",
            "role": "user"
        },
        "sessionId": LEGACY_CLAUDE_SESSION_ID,
        "timestamp": "2026-01-01T00:00:00.000Z",
        "type": "user",
        "uuid": LEGACY_CLAUDE_MESSAGE_ID
    });
    let source = ObservationSourceIdentityV1::for_provider_source(
        ProviderId::new("claude").unwrap(),
        SessionId::new(LEGACY_CLAUDE_SESSION_ID).unwrap(),
        SessionId::new(LEGACY_CLAUDE_SOURCE_KEY).unwrap(),
    )
    .unwrap();
    let identity = ObservationIdentityMaterialV1::new(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(23).unwrap(),
        ObservationSourceRangeV1::new(41, 42).unwrap(),
    )
    .unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.synthetic-legacy-claude").unwrap(),
            ComponentVersion::new("sanitizer.synthetic-legacy-claude.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.synthetic-legacy-claude").unwrap(),
        payload,
    )
    .unwrap()
}

struct LegacyClaudeProjectionSeed {
    observation_id: String,
    output_digest: String,
}

async fn seed_v1_legacy_claude_projection(
    profile_root: &std::path::Path,
    corrupted_text: Option<&str>,
) -> LegacyClaudeProjectionSeed {
    let observation = legacy_claude_source_key_observation();
    let observation_id = observation.observation_id().as_str().to_owned();
    let runtime = registered_runtime(profile_root).await.unwrap();
    let db = registered_database(&runtime);
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let writer = db.writer_connection().unwrap();
    let previous_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().start(),
    )
    .unwrap();
    writer
        .execute(
            "INSERT INTO source_cursors (source_json, scope_json, cursor_json)
             VALUES (?1, ?2, ?3)",
            params![
                serde_json::to_string(observation.source()).unwrap(),
                serde_json::to_string(observation.scope()).unwrap(),
                serde_json::to_string(&previous_cursor).unwrap()
            ],
        )
        .await
        .unwrap();
    store
        .persist_observation(write_after(observation, Some(previous_cursor)))
        .await
        .unwrap();
    let queued = store.next_queued_observation().await.unwrap().unwrap();
    assert!(matches!(
        store.project_observation(&queued).await.unwrap(),
        ProjectionPersistOutcome::Projected(_)
    ));
    let mut digest_rows = writer
        .query(
            "SELECT output_digest FROM observation_projection_provenance
             WHERE projector_version = ?1 AND observation_id = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
                observation_id.as_str()
            ],
        )
        .await
        .unwrap();
    let output_digest = digest_rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    drop(digest_rows);

    // Recreate the V1 predecessor shape without retaining live session data.
    writer
        .execute(
            "UPDATE session_messages SET source_path = ?2
             WHERE provider = 'claude' AND message_id = ?1",
            params![
                LEGACY_CLAUDE_MESSAGE_ID,
                format!("claude:{LEGACY_CLAUDE_SESSION_ID}")
            ],
        )
        .await
        .unwrap();
    if let Some(text) = corrupted_text {
        writer
            .execute(
                "UPDATE session_messages SET text = ?2
                 WHERE provider = 'claude' AND message_id = ?1",
                params![LEGACY_CLAUDE_MESSAGE_ID, text],
            )
            .await
            .unwrap();
    }
    writer
        .execute(
            "UPDATE observation_projection_provenance
             SET projector_version = ?1
             WHERE projector_version = ?2 AND observation_id = ?3",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V1,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
                observation_id.as_str()
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE observation_projection_checkpoints SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V1,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    drop(runtime);
    LegacyClaudeProjectionSeed {
        observation_id,
        output_digest,
    }
}

#[tokio::test]
async fn v1_upgrade_adopts_legacy_claude_source_path_and_preserves_ownership() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    let seed = Box::pin(seed_v1_legacy_claude_projection(&profile_root, None)).await;
    let reopened = registered_runtime(&profile_root).await.unwrap();
    audit_observation_authority(&reopened).await;
    let writer = registered_database(&reopened).writer_connection().unwrap();
    let mut rows = writer
        .query(
            "SELECT
                (SELECT source_path FROM session_messages
                 WHERE provider = 'claude' AND message_id = ?3),
                (SELECT output_digest FROM observation_projection_provenance
                 WHERE projector_version = ?1 AND observation_id = ?4),
                (SELECT message_created FROM observation_projection_provenance
                 WHERE projector_version = ?1 AND observation_id = ?4),
                (SELECT message_created FROM observation_projection_provenance
                 WHERE projector_version = ?2 AND observation_id = ?4),
                (SELECT last_sequence FROM observation_projection_checkpoints
                 WHERE projector_version = ?1),
                (SELECT completed FROM observation_projection_migrations
                 WHERE source_projector_version = ?2
                   AND target_projector_version = ?1)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
                SESSION_MESSAGE_PROJECTOR_VERSION_V1,
                LEGACY_CLAUDE_MESSAGE_ID,
                seed.observation_id.as_str()
            ],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), LEGACY_CLAUDE_SOURCE_KEY);
    assert_eq!(row.get::<String>(1).unwrap(), seed.output_digest);
    assert_eq!(row.get::<i64>(2).unwrap(), 0);
    assert_eq!(row.get::<i64>(3).unwrap(), 1);
    assert_eq!(row.get::<i64>(4).unwrap(), 1);
    assert_eq!(row.get::<i64>(5).unwrap(), 1);
}

#[tokio::test]
async fn v1_upgrade_rejects_non_source_path_projection_differences() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    Box::pin(seed_v1_legacy_claude_projection(
        &profile_root,
        Some("Conflicting legacy text."),
    ))
    .await;

    let Err(error) = registered_runtime(&profile_root).await else {
        panic!("non-source-path mismatch must collide");
    };
    assert!(error.to_string().contains("projection output collided"));
}

#[tokio::test]
async fn v2_upgrade_materializes_the_complete_v3_effect_before_authority_audit() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    let runtime = registered_runtime(&profile_root).await.unwrap();
    let db = registered_database(&runtime);
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let writer = db.writer_connection().unwrap();
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
    let mut initial_rows = writer
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
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V4],
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

    writer
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
    writer
        .execute(
            "UPDATE observation_projection_dispositions SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE observation_projection_provenance SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE observation_projection_checkpoints SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    writer
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
    writer
        .execute(
            "INSERT INTO observation_projection_checkpoints (
                projector_version, last_sequence
             ) VALUES (?1, 900)",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V4],
        )
        .await
        .unwrap();
    drop(runtime);

    let reopened = registered_runtime(&profile_root).await.unwrap();
    audit_observation_authority(&reopened).await;
    let reopened_db = registered_database(&reopened);
    let reopened_writer = reopened_db.writer_connection().unwrap();
    let mut rows = reopened_writer
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
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
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

    let store = reopened
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
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

    let converged = registered_runtime(&profile_root).await.unwrap();
    audit_observation_authority(&converged).await;
    let converged_writer = registered_database(&converged).writer_connection().unwrap();
    let mut converged_rows = converged_writer
        .query(
            "SELECT
                (SELECT COUNT(*) FROM projection_queue),
                (SELECT last_sequence FROM observation_projection_checkpoints
                 WHERE projector_version = ?1)",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V4],
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
    let profile_root = tmp.path().join(".tracedecay");
    let runtime = registered_runtime(&profile_root).await.unwrap();
    let db = registered_database(&runtime);
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let writer = db.writer_connection().unwrap();
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
    writer
        .execute(
            "UPDATE observation_projection_provenance SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE observation_projection_checkpoints SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    drop(runtime);

    let reopened = registered_runtime(&profile_root).await.unwrap();
    audit_observation_authority(&reopened).await;
    let reopened_db = registered_database(&reopened);
    let reopened_writer = reopened_db.writer_connection().unwrap();
    let mut rows = reopened_writer
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
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
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
    let store = reopened
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    store
        .persist_observation(write_after(third, Some(second_cursor)))
        .await
        .unwrap();
    let queued = store.next_queued_observation().await.unwrap().unwrap();
    store.project_observation(&queued).await.unwrap();
    let mut text_rows = reopened_writer
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
async fn duplicate_output_identity_converges_as_a_durable_collision_skip() {
    let tmp = TempDir::new().unwrap();
    let runtime = registered_runtime(&tmp.path().join(".tracedecay"))
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();

    // Two distinct claude observations (different byte ranges, so different
    // observation identities) carrying the same session/message uuid — the
    // duplicate-era provider-record shape whose second binder collides on
    // output ownership.
    let build = |range: (u64, u64), content: &str, receipt: &str, source_key: &str| {
        let payload = serde_json::json!({
            "cwd": "/workspace/project",
            "message": {"content": content, "role": "user"},
            "sessionId": LEGACY_CLAUDE_SESSION_ID,
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "user",
            "uuid": LEGACY_CLAUDE_MESSAGE_ID
        });
        let source = ObservationSourceIdentityV1::for_provider_source(
            ProviderId::new("claude").unwrap(),
            SessionId::new(LEGACY_CLAUDE_SESSION_ID).unwrap(),
            SessionId::new(source_key).unwrap(),
        )
        .unwrap();
        let identity = ObservationIdentityMaterialV1::new(
            source,
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).unwrap(),
            ObservationSourceRangeV1::new(range.0, range.1).unwrap(),
        )
        .unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(receipt).unwrap(),
                ComponentVersion::new("sanitizer.collision-skip.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
        )
        .unwrap();
        DurableObservationV1::new(
            identity,
            receipt,
            RetentionClass::new("retention.collision-skip").unwrap(),
            payload,
        )
        .unwrap()
    };
    // Different source keys model the same message re-serialized in two
    // transcript files: no shared projection lineage, so the second binder
    // must not adopt or supersede the first's output.
    let second_source_key = concat!(
        "tracedecay-claude-observation-source-v1-sha256-",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    );
    let first = build(
        (0, 2),
        "First binder keeps the output.",
        "receipt.collision.1",
        LEGACY_CLAUDE_SOURCE_KEY,
    );
    let second = build(
        (0, 3),
        "Second binder must converge.",
        "receipt.collision.2",
        second_source_key,
    );
    store.persist_observation(write(first)).await.unwrap();
    store.persist_observation(write(second)).await.unwrap();

    let first_queued = store.next_queued_observation().await.unwrap().unwrap();
    assert!(matches!(
        store.project_observation(&first_queued).await.unwrap(),
        ProjectionPersistOutcome::Projected(_)
    ));
    let second_queued = store.next_queued_observation().await.unwrap().unwrap();
    match store.project_observation(&second_queued).await.unwrap() {
        ProjectionPersistOutcome::Skipped { reason, .. } => {
            assert_eq!(
                reason,
                tracedecay_store::ProjectionSkipReason::OutputCollision,
                "the second binder must converge as a collision skip"
            );
        }
        other => panic!("expected a collision skip, got {other:?}"),
    }
    assert!(
        store.next_queued_observation().await.unwrap().is_none(),
        "the projection queue must converge"
    );

    // Replay is idempotent through the recorded disposition, and the
    // authority audit accepts the skip against a message-shaped derivation.
    assert!(matches!(
        store.project_observation(&second_queued).await.unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    audit_observation_authority(&runtime).await;
}

#[tokio::test]
async fn v2_upgrade_with_broken_predecessor_lineage_falls_back_to_rebuild() {
    const PREDECESSOR_ROWS: usize = 6;

    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    let runtime = registered_runtime(&profile_root).await.unwrap();
    let db = registered_database(&runtime);
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let writer = db.writer_connection().unwrap();
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
    writer
        .execute(
            "UPDATE observation_projection_dispositions SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE observation_projection_checkpoints SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    // Break the predecessor lineage the way an interrupted older writer can:
    // one observation has no terminal predecessor outcome at all, so the
    // incremental page validation can never pass.
    writer
        .execute(
            "DELETE FROM observation_projection_dispositions
             WHERE projector_version = ?1
               AND observation_id = (
                   SELECT disposition.observation_id
                   FROM observation_projection_dispositions AS disposition
                   JOIN observations AS observation
                     ON observation.observation_id = disposition.observation_id
                   WHERE disposition.projector_version = ?1
                   ORDER BY observation.sequence LIMIT 1
               )",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V2],
        )
        .await
        .unwrap();
    drop(runtime);

    // Every reopen must succeed (never fail closed) and the daemon-owned
    // background advancement path must converge the staged rebuild.
    let mut superseded = false;
    let cancelled = AtomicBool::new(false);
    for attempt in 0..16 {
        let open = match registered_runtime(&profile_root).await {
            Ok(open) => open,
            Err(error) => panic!("open attempt {attempt} failed: {error}"),
        };
        registered_database(&open)
            .advance_projection_version_migration_until_cancelled(&cancelled)
            .await
            .expect("advance staged observation rebuild");
        let writer = registered_database(&open).writer_connection().unwrap();
        let mut rows = writer
            .query(
                "SELECT completed FROM observation_projection_migrations
                 WHERE source_projector_version = ?1
                   AND target_projector_version = ?2",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                    SESSION_MESSAGE_PROJECTOR_VERSION_V4
                ],
            )
            .await
            .unwrap();
        let completed = rows
            .next()
            .await
            .unwrap()
            .map(|row| row.get::<i64>(0).unwrap());
        drop(rows);
        if completed == Some(1) {
            audit_observation_authority(&open).await;
            superseded = true;
            break;
        }
        drop(open);
    }
    assert!(
        superseded,
        "rebuild fallback must converge and supersede the incremental migration"
    );
}

#[tokio::test]
async fn schema_open_defers_rebuild_until_background_migration_advances_it() {
    const PREDECESSOR_ROWS: usize = 513;

    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    let runtime = registered_runtime(&profile_root).await.unwrap();
    let db = registered_database(&runtime);
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let writer = db.writer_connection().unwrap();
    for index in 0..PREDECESSOR_ROWS {
        store
            .persist_observation(write(checked_in_codex_session_boundary(index)))
            .await
            .unwrap();
    }
    while let Some(queued) = store.next_queued_observation().await.unwrap() {
        store.project_observation(&queued).await.unwrap();
    }
    writer
        .execute(
            "UPDATE observation_projection_dispositions SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE observation_projection_checkpoints SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "DELETE FROM observation_projection_dispositions
             WHERE projector_version = ?1
               AND observation_id = (
                   SELECT disposition.observation_id
                   FROM observation_projection_dispositions AS disposition
                   JOIN observations AS observation
                     ON observation.observation_id = disposition.observation_id
                   WHERE disposition.projector_version = ?1
                   ORDER BY observation.sequence LIMIT 1
               )",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V2],
        )
        .await
        .unwrap();

    drop(runtime);

    let first_open = registered_runtime(&profile_root).await.unwrap();
    let first_writer = registered_database(&first_open)
        .writer_connection()
        .unwrap();
    let mut rows = first_writer
        .query(
            "SELECT aliases_staged_through, staged_through
             FROM observation_projection_rebuilds
             WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V4],
        )
        .await
        .unwrap();
    let before = rows
        .next()
        .await
        .unwrap()
        .map(|row| (row.get::<i64>(0).unwrap(), row.get::<i64>(1).unwrap()))
        .expect("first fallback starts a rebuild");
    assert_eq!(
        before,
        (0, 0),
        "schema open must record, but not synchronously advance, the rebuild"
    );
    drop(rows);
    first_writer
        .execute(
            "INSERT OR REPLACE INTO observation_projection_migrations (
                source_projector_version, target_projector_version,
                source_frontier, migrated_through, completed
             ) VALUES (?1, ?2, ?3, ?3, 1)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
                PREDECESSOR_ROWS as i64,
            ],
        )
        .await
        .unwrap();
    drop(first_open);

    let second_open = registered_runtime(&profile_root).await.unwrap();
    let second_writer = registered_database(&second_open)
        .writer_connection()
        .unwrap();
    let mut rows = second_writer
        .query(
            "SELECT aliases_staged_through, staged_through
             FROM observation_projection_rebuilds
             WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V4],
        )
        .await
        .unwrap();
    let progress = rows
        .next()
        .await
        .unwrap()
        .map(|row| (row.get::<i64>(0).unwrap(), row.get::<i64>(1).unwrap()));
    drop(rows);
    assert_eq!(
        progress,
        Some(before),
        "reopening a store must leave the rebuild for the background worker"
    );
    let database = registered_database(&second_open);
    let cancelled = AtomicBool::new(true);
    assert!(
        !database
            .advance_projection_version_migration_until_cancelled(&cancelled)
            .await
            .unwrap(),
        "a cancelled background worker must leave the rebuild pending"
    );
    let writer = database.writer_connection().unwrap();
    let mut rows = writer
        .query(
            "SELECT aliases_staged_through, staged_through
             FROM observation_projection_rebuilds
             WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V4],
        )
        .await
        .unwrap();
    let cancelled_progress = rows
        .next()
        .await
        .unwrap()
        .map(|row| (row.get::<i64>(0).unwrap(), row.get::<i64>(1).unwrap()));
    drop(rows);
    assert_eq!(
        cancelled_progress,
        Some(before),
        "cancellation must stop at the last committed rebuild checkpoint"
    );
    cancelled.store(false, Ordering::Release);
    let mut complete = false;
    for _ in 0..16 {
        complete = database
            .advance_projection_version_migration_until_cancelled(&cancelled)
            .await
            .unwrap();
        if complete {
            break;
        }
    }
    assert!(
        complete,
        "the daemon background migration primitive must converge the rebuild"
    );
}

#[tokio::test]
async fn v2_upgrade_runs_one_page_per_open_and_resumes() {
    const PREDECESSOR_ROWS: usize = 257;

    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    let runtime = registered_runtime(&profile_root).await.unwrap();
    let db = registered_database(&runtime);
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let writer = db.writer_connection().unwrap();
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
    writer
        .execute(
            "UPDATE observation_projection_dispositions SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE observation_projection_checkpoints SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V2,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "INSERT INTO observation_projection_checkpoints (
                projector_version, last_sequence
             ) VALUES (?1, 900)",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V4],
        )
        .await
        .unwrap();
    drop(runtime);

    let first_open = registered_runtime(&profile_root).await.unwrap();
    audit_observation_authority(&first_open).await;
    let first_writer = registered_database(&first_open)
        .writer_connection()
        .unwrap();
    let mut page_rows = first_writer
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
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
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

    let second_open = registered_runtime(&profile_root).await.unwrap();
    audit_observation_authority(&second_open).await;
    let second_writer = registered_database(&second_open)
        .writer_connection()
        .unwrap();
    let mut rows = second_writer
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
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
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

    let final_open = registered_runtime(&profile_root).await.unwrap();
    audit_observation_authority(&final_open).await;
    let final_writer = registered_database(&final_open)
        .writer_connection()
        .unwrap();
    let mut rows = final_writer
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
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
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

    let converged = registered_runtime(&profile_root).await.unwrap();
    audit_observation_authority(&converged).await;
}

#[tokio::test]
async fn v3_upgrade_backfills_v4_anchor_provenance_without_rekeying() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    let runtime = registered_runtime(&profile_root).await.unwrap();
    let db = registered_database(&runtime);
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let writer = db.writer_connection().unwrap();
    let observation = checked_in_v2_observations().remove(0);
    let observation_id = observation.observation_id().clone();
    store.persist_observation(write(observation)).await.unwrap();
    let queued = store.next_queued_observation().await.unwrap().unwrap();
    assert!(matches!(
        store.project_observation(&queued).await.unwrap(),
        ProjectionPersistOutcome::Projected(_)
    ));
    let canonical_anchor_id: String = writer
        .query(
            "SELECT anchor_id FROM observation_retrieval_anchors WHERE observation_id = ?1",
            params![observation_id.as_str()],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    writer
        .execute(
            "UPDATE observation_projection_provenance
             SET projector_version = ?1, retrieval_anchor_id = NULL
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE observation_projection_aliases SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "UPDATE observation_projection_checkpoints SET projector_version = ?1
             WHERE projector_version = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    drop(runtime);

    let reopened = registered_runtime(&profile_root).await.unwrap();
    audit_observation_authority(&reopened).await;
    let reopened_writer = registered_database(&reopened).writer_connection().unwrap();
    let mut rows = reopened_writer
        .query(
            "SELECT
                (SELECT retrieval_anchor_id FROM observation_projection_provenance
                 WHERE projector_version = ?1 AND observation_id = ?3),
                (SELECT retrieval_anchor_id IS NULL FROM observation_projection_provenance
                 WHERE projector_version = ?2 AND observation_id = ?3),
                (SELECT completed FROM observation_projection_migrations
                 WHERE source_projector_version = ?2 AND target_projector_version = ?1)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                observation_id.as_str()
            ],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), canonical_anchor_id);
    assert_eq!(row.get::<i64>(1).unwrap(), 1);
    assert_eq!(row.get::<i64>(2).unwrap(), 1);
}

async fn initialize_engine_migration_state(connection: &TestConnection) {
    crate::ensure_registered_schema(connection)
        .await
        .expect("initialize observation migration fixture through production migrations");
    connection
        .execute_batch(
            "DELETE FROM observation_projection_migrations;
             DELETE FROM observation_projection_checkpoints;",
        )
        .await
        .expect("reset migrated observation progress rows for the focused fixture");
}

#[tokio::test]
async fn registered_engine_migration_replay_preserves_completed_version_receipt() {
    let temporary = TempDir::new().unwrap();
    let connection = TestConnection::open(&temporary.path().join("projection.db"));
    initialize_engine_migration_state(&connection).await;
    connection
        .execute(
            "INSERT INTO observation_projection_checkpoints VALUES (?1, 7)",
            tracedecay_runtime_core::db::engine::params![SESSION_MESSAGE_PROJECTOR_VERSION_V3],
        )
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO observation_projection_migrations VALUES (?1, ?2, 7, 7, 1)",
            tracedecay_runtime_core::db::engine::params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
            ],
        )
        .await
        .unwrap();

    prepare_projection_version_migration_with_engine(&connection)
        .await
        .unwrap();
    prepare_projection_version_migration_with_engine(&connection)
        .await
        .unwrap();

    let mut rows = connection
        .query(
            "SELECT source_frontier, migrated_through, completed, COUNT(*)
             FROM observation_projection_migrations
             WHERE source_projector_version = ?1 AND target_projector_version = ?2",
            tracedecay_runtime_core::db::engine::params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4
            ],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 7);
    assert_eq!(row.get::<i64>(1).unwrap(), 7);
    assert_eq!(row.get::<i64>(2).unwrap(), 1);
    assert_eq!(row.get::<i64>(3).unwrap(), 1);
}

#[tokio::test]
async fn registered_schema_rejects_inconsistent_resume_receipt() {
    let temporary = TempDir::new().unwrap();
    let connection = TestConnection::open(&temporary.path().join("projection.db"));
    initialize_engine_migration_state(&connection).await;
    connection
        .execute(
            "INSERT INTO observation_projection_checkpoints VALUES (?1, 7)",
            tracedecay_runtime_core::db::engine::params![SESSION_MESSAGE_PROJECTOR_VERSION_V3],
        )
        .await
        .unwrap();
    let error = connection
        .execute(
            "INSERT INTO observation_projection_migrations VALUES (?1, ?2, 7, 6, 1)",
            tracedecay_runtime_core::db::engine::params![
                SESSION_MESSAGE_PROJECTOR_VERSION_V3,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
            ],
        )
        .await
        .expect_err("completed migration must cover its exact source frontier");
    assert!(
        error
            .to_string()
            .contains("completed = 0 OR migrated_through = source_frontier")
    );
}

struct RevocableMigrationWriteAuthority {
    active: AtomicBool,
}

impl RevocableMigrationWriteAuthority {
    fn active() -> Self {
        Self {
            active: AtomicBool::new(true),
        }
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::SeqCst);
    }
}

impl MigrationSqlWriteAuthority for RevocableMigrationWriteAuthority {
    fn verify(&self, _intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
        if self.active.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(MigrationSqlError::AuthorityDenied(
                "projection migration authority revoked".to_owned(),
            ))
        }
    }
}

#[tokio::test]
async fn registered_engine_migration_rechecks_actor_time_authority_before_progress_write() {
    let temporary = TempDir::new().unwrap();
    let authority = Arc::new(RevocableMigrationWriteAuthority::active());
    let connection = TestConnection::open_with_write_authority(
        &temporary.path().join("projection.db"),
        authority.clone(),
    );
    initialize_engine_migration_state(&connection).await;
    connection
        .execute(
            "INSERT INTO observation_projection_checkpoints VALUES (?1, 1)",
            tracedecay_runtime_core::db::engine::params![SESSION_MESSAGE_PROJECTOR_VERSION_V3],
        )
        .await
        .unwrap();
    authority.revoke();

    let error = prepare_projection_version_migration_with_engine(&connection)
        .await
        .expect_err("revoked actor-time authority must deny migration");
    let tracedecay_store::ProjectionStoreError::Storage { operation, source } = error else {
        panic!("revoked migration returned the wrong error kind");
    };
    assert_eq!(operation, "begin projection version migration page");
    assert!(source.to_string().contains("authority revoked"), "{source}");
    let mut rows = connection
        .query("SELECT COUNT(*) FROM observation_projection_migrations", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
}
