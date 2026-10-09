use serde_json::{Value, json};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, CanonicalWorkflowSemanticKindV1,
    DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadReferenceV1, ProviderId,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId,
};

use super::{TemporalSqlRead, observation_matches_filter};
use crate::query::decode_stored_observation;
use tracedecay_runtime_core::db::engine::{Executor, TestConnection, params};
use tracedecay_store::canonical_body::{
    CANONICAL_BODIES_TABLE_SQL, INLINE_BODY_BYTES, UPSERT_CANONICAL_BODY_SQL, slim_json_value,
};
use tracedecay_temporal_query::snapshot::{TemporalCandidateFilterV1, TemporalMessageTypeFilterV1};

fn receipt(payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt-semantic-filter").unwrap(),
            tracedecay_domain::ComponentVersion::new("semantic-filter-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn canonical_observation(facts: Vec<CanonicalObservationFactV1>) -> DurableObservationV1 {
    observation_at(facts, None)
}

fn observation_at(
    facts: Vec<CanonicalObservationFactV1>,
    native_timestamp: Option<i64>,
) -> DurableObservationV1 {
    let session_id = SessionId::new("session-semantic-filter").unwrap();
    let provider = ProviderId::new("codex").unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(1, 2).unwrap();
    let record_id = ObservationId::new("record-semantic-filter").unwrap();
    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range);
    if let Some(native_timestamp) = native_timestamp {
        evidence = evidence.with_native_timestamp(native_timestamp);
    }
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        CanonicalObservationRelationsV1::new(session_id)
            .with_message_id(ObservationId::new("message-semantic-filter").unwrap()),
        facts,
        evidence,
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        receipt(&payload),
        RetentionClass::new("retention.semantic-filter-test").unwrap(),
        payload,
    )
    .unwrap()
}

#[test]
fn goal_only_time_filter_uses_canonical_observation_timestamp() {
    let goal = CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
        provider_reference: Some("thread-goal-only".to_string()),
        item_id: None,
        parent_reference: None,
        list_reference: None,
        state: None,
        status: Some("active".to_string()),
        item_order: None,
        revision: None,
        event_sequence: None,
        content: Some(json!({"objective": "finish temporal retrieval"})),
    };
    let observation = observation_at(vec![goal.clone()], Some(42));
    let filter = TemporalCandidateFilterV1 {
        start_time: Some(40),
        end_time: Some(50),
        goals: true,
        ..TemporalCandidateFilterV1::default()
    };

    assert!(observation_matches_filter(&observation, "user", &filter).unwrap());
    assert!(
        !observation_matches_filter(
            &observation,
            "user",
            &TemporalCandidateFilterV1 {
                start_time: Some(43),
                ..filter.clone()
            },
        )
        .unwrap()
    );
    assert!(
        !observation_matches_filter(&canonical_observation(vec![goal]), "user", &filter,).unwrap(),
        "a Goal without Message.timestamp or canonical observation time stays ineligible"
    );
}

#[test]
fn goal_role_and_time_eligibility_are_conjunctive_before_ranking() {
    let observation = canonical_observation(vec![
        CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::User,
            content: json!({"text": "ship temporal retrieval"}),
            model: None,
            timestamp: Some(42),
        },
        CanonicalObservationFactV1::WorkflowLifecycle {
            semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
            provider_reference: None,
            item_id: None,
            parent_reference: None,
            list_reference: None,
            state: None,
            status: None,
            item_order: None,
            revision: None,
            event_sequence: None,
            content: Some(json!({"text": "ship temporal retrieval"})),
        },
    ]);
    let filter = TemporalCandidateFilterV1 {
        message_type: TemporalMessageTypeFilterV1::DirectUser,
        roles: vec!["user".to_string()],
        start_time: Some(40),
        end_time: Some(50),
        goals: true,
        ..TemporalCandidateFilterV1::default()
    };

    assert!(observation_matches_filter(&observation, "user", &filter).unwrap());

    let too_late = TemporalCandidateFilterV1 {
        start_time: Some(43),
        ..filter
    };
    assert!(!observation_matches_filter(&observation, "user", &too_late).unwrap());
}

#[test]
fn tool_results_do_not_leak_into_direct_user_filter() {
    let observation = canonical_observation(vec![
        CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::User,
            content: json!({"text": "tool payload"}),
            model: None,
            timestamp: Some(42),
        },
        CanonicalObservationFactV1::ToolResult {
            invocation_id: None,
            content: json!({"text": "tool payload"}),
            success: Some(true),
        },
    ]);
    let direct = TemporalCandidateFilterV1 {
        message_type: TemporalMessageTypeFilterV1::DirectUser,
        ..TemporalCandidateFilterV1::default()
    };
    let tool = TemporalCandidateFilterV1 {
        message_type: TemporalMessageTypeFilterV1::ToolResult,
        ..TemporalCandidateFilterV1::default()
    };

    assert!(!observation_matches_filter(&observation, "user", &direct).unwrap());
    assert!(observation_matches_filter(&observation, "user", &tool).unwrap());
}

#[test]
fn canonical_source_filter_matches_provider_or_source_identity_before_ranking() {
    let observation = canonical_observation(vec![CanonicalObservationFactV1::Message {
        role: CanonicalMessageRoleV1::User,
        content: json!({"text": "source-bound evidence"}),
        model: None,
        timestamp: Some(42),
    }]);

    for source in ["codex", "session-semantic-filter"] {
        assert!(
            observation_matches_filter(
                &observation,
                "user",
                &TemporalCandidateFilterV1 {
                    source: Some(source.to_string()),
                    ..TemporalCandidateFilterV1::default()
                },
            )
            .unwrap()
        );
    }
    assert!(
        !observation_matches_filter(
            &observation,
            "user",
            &TemporalCandidateFilterV1 {
                source: Some("claude".to_string()),
                ..TemporalCandidateFilterV1::default()
            },
        )
        .unwrap()
    );
}

#[tokio::test]
async fn semantic_filter_hydrates_compacted_content_and_rejects_missing_bodies() {
    let observation = canonical_observation(vec![CanonicalObservationFactV1::Message {
        role: CanonicalMessageRoleV1::User,
        content: json!({"text": "canonical content".repeat(INLINE_BODY_BYTES)}),
        model: None,
        timestamp: Some(42),
    }]);
    let mut stored = serde_json::to_value(&observation).unwrap();
    let bodies = slim_json_value(&mut stored).unwrap();
    assert!(!bodies.is_empty());
    let dir = tempfile::tempdir().unwrap();
    let conn = TestConnection::open(&dir.path().join("semantic-body.db"));
    conn.execute_batch(CANONICAL_BODIES_TABLE_SQL)
        .await
        .unwrap();
    for body in &bodies {
        conn.execute(
            UPSERT_CANONICAL_BODY_SQL,
            params![
                body.content_hash.as_str(),
                body.encoding,
                body.blob.as_slice(),
                body.uncompressed_bytes
            ],
        )
        .await
        .unwrap();
    }
    let read = TemporalSqlRead::engine_connection(&conn);
    let encoded = stored.to_string();
    let hydrated = decode_stored_observation(&read, &encoded, "semantic filter test")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&hydrated).unwrap(),
        serde_json::to_value(&observation).unwrap()
    );
    assert!(
        observation_matches_filter(
            &hydrated,
            "user",
            &TemporalCandidateFilterV1 {
                source: Some("codex".to_owned()),
                message_type: TemporalMessageTypeFilterV1::DirectUser,
                ..TemporalCandidateFilterV1::default()
            }
        )
        .unwrap()
    );
    conn.execute("DELETE FROM session_canonical_bodies", ())
        .await
        .unwrap();
    assert!(
        decode_stored_observation(&read, &encoded, "semantic filter test")
            .await
            .is_err()
    );
}
