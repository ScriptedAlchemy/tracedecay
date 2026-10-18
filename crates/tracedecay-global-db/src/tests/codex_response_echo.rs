//! A store that projected both records Codex writes for one prompt converges
//! to one turn when it is next opened.

use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ComponentVersion,
    DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectionGenerationId, ProviderId, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId, UtcMicros,
};
use tracedecay_runtime_core::db::engine::QueryExecutor;
use tracedecay_runtime_core::params;
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationWrite, SESSION_MESSAGE_PROJECTOR_VERSION,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor,
};

use super::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

const SESSION: &str = "session.codex-response-echo";
const PROMPT: &str = "Find the callers of publish_generation.";

struct Record {
    id: &'static str,
    native_kind: &'static str,
    message_id: Option<&'static str>,
    content: serde_json::Value,
}

fn observation(record: &Record, index: u64) -> DurableObservationV1 {
    let provider = ProviderId::new("codex").unwrap();
    let session = SessionId::new(SESSION).unwrap();
    let record_id = ObservationId::new(record.id).unwrap();
    let range = ObservationSourceRangeV1::new(index * 100, index * 100 + 100).unwrap();
    let relations = CanonicalObservationRelationsV1::new(session.clone())
        .with_turn_id(ObservationId::new("turn-1").unwrap())
        .with_message_id(ObservationId::new(record.message_id.unwrap_or(record.id)).unwrap());
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider.clone(),
        record.native_kind,
        record_id.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::User,
            content: record.content.clone(),
            model: None,
            timestamp: Some(1_788_523_201),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.{}", record.id)).unwrap(),
            ComponentVersion::new("sanitizer.codex-response-echo.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .unwrap();
    DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_provider(provider, session).unwrap(),
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).unwrap(),
            range,
            ObservationOrderingDomainV1::FileBytes,
            record_id,
        )
        .unwrap(),
        receipt,
        RetentionClass::new("retention.codex-response-echo").unwrap(),
        payload,
    )
    .unwrap()
}

async fn seed(
    runtime: &HostAdmissionTestRuntimeV1,
    records: &[Record],
) -> Vec<DurableObservationV1> {
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let mut expected = None;
    let mut seeded = Vec::new();
    for (index, record) in (0_u64..).zip(records) {
        let observation = observation(record, index);
        let next = ObservationSourceCursorV1::for_ordering(
            observation.source().clone(),
            observation.scope().clone(),
            observation.identity().generation(),
            observation.identity().ordering_domain(),
            observation.identity().position().end(),
        )
        .unwrap();
        let write = ObservationWrite::new(observation.clone(), expected, next.clone()).unwrap();
        let generation = ProjectionGenerationId::new("projection.codex-response-echo.v1").unwrap();
        let authorization = build_observation_resolution_authorization_v1(
            write.observation(),
            "codex-response-echo",
        )
        .unwrap();
        let anchor = build_observation_retrieval_anchor(
            write.observation(),
            generation.clone(),
            UtcMicros(1),
            authorization,
        )
        .unwrap();
        assert!(matches!(
            store
                .persist_observation(
                    AnchoredObservationWrite::new(write, anchor, generation).unwrap()
                )
                .await
                .unwrap(),
            ObservationPersistOutcome::Committed(_)
        ));
        store
            .project_observation(observation.observation_id())
            .await
            .unwrap();
        expected = Some(next);
        seeded.push(observation);
    }
    seeded
}

async fn session_message_ids(conn: &impl QueryExecutor) -> Vec<String> {
    let mut rows = conn
        .query(
            "SELECT message_id FROM lcm_raw_messages
             WHERE provider = 'codex' AND session_id = ?1 ORDER BY ordinal",
            params![SESSION],
        )
        .await
        .unwrap();
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        ids.push(row.get::<String>(0).unwrap());
    }
    ids
}

async fn disposition(
    conn: &impl QueryExecutor,
    observation: &DurableObservationV1,
) -> Option<String> {
    let mut rows = conn
        .query(
            "SELECT reason FROM observation_projection_dispositions
             WHERE projector_version = ?1 AND observation_id = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str()
            ],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|row| row.get::<String>(0).unwrap())
}

/// Builds the store a binary without echo retirement left behind: the prompt's
/// `response_item` row and its `item_completed` row both projected and
/// adjacent. An unrelated row between them while they project keeps
/// projection-time retirement out of the way; retiring it leaves the pair.
#[asupersync::test]
async fn opening_a_store_with_a_projected_codex_echo_keeps_one_turn() {
    let directory = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .unwrap();
    let seeded = seed(
        &runtime,
        &[
            Record {
                id: "record.response-user-1",
                native_kind: "response_item",
                message_id: None,
                content: serde_json::json!([{"type": "input_text", "text": PROMPT}]),
            },
            Record {
                id: "record.unrelated-1",
                native_kind: "response_item",
                message_id: None,
                content: serde_json::json!([{"type": "input_text", "text": "unrelated"}]),
            },
            Record {
                id: "record.item-completed-1",
                native_kind: "event_msg",
                message_id: Some("user-item-1"),
                content: serde_json::json!([{"type": "text", "text": PROMPT}]),
            },
        ],
    )
    .await;
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let transaction = database
        .runtime_database()
        .begin_write_transaction("retire the unrelated record")
        .await
        .unwrap();
    assert!(
        crate::observation_projection::retire_source_record(
            &transaction,
            seeded[1].observation_id().as_str(),
        )
        .await
        .unwrap()
    );
    // Opening this fresh store already recorded its convergence; a store an
    // earlier binary wrote carries no such record.
    transaction
        .execute(
            "DELETE FROM tracedecay_schema_version
             WHERE component = 'codex-response-echo-retirement-v1'",
            (),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let snapshot = database.read_snapshot().await.unwrap();
    assert_eq!(
        session_message_ids(&snapshot).await.len(),
        2,
        "the fixture must hold the duplicated prompt"
    );
    drop(snapshot);

    crate::schema_stages::converge_attached_registered_schema(database.runtime_database())
        .await
        .unwrap();
    let snapshot = database.read_snapshot().await.unwrap();
    assert_eq!(session_message_ids(&snapshot).await, ["user-item-1"]);
    assert_eq!(
        disposition(&snapshot, &seeded[0]).await.as_deref(),
        Some("output_collision")
    );
    drop(snapshot);

    crate::schema_stages::converge_attached_registered_schema(database.runtime_database())
        .await
        .expect("the converged store stays admitted");
    let snapshot = database.read_snapshot().await.unwrap();
    assert_eq!(session_message_ids(&snapshot).await, ["user-item-1"]);
}
