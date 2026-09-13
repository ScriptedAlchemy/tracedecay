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

use super::observation_matches_filter;
use tracedecay_temporal_query::ports::{TemporalCandidateFilterV1, TemporalMessageTypeFilterV1};

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

fn encoded_observation(facts: Vec<CanonicalObservationFactV1>) -> String {
    encoded_observation_at(facts, None)
}

fn encoded_observation_at(
    facts: Vec<CanonicalObservationFactV1>,
    native_timestamp: Option<i64>,
) -> String {
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
    serde_json::to_string(
        &DurableObservationV1::new(
            identity,
            receipt(&payload),
            RetentionClass::new("retention.semantic-filter-test").unwrap(),
            payload,
        )
        .unwrap(),
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
    let encoded = encoded_observation_at(vec![goal.clone()], Some(42));
    let filter = TemporalCandidateFilterV1 {
        start_time: Some(40),
        end_time: Some(50),
        goals: true,
        ..TemporalCandidateFilterV1::default()
    };

    assert!(observation_matches_filter(&encoded, "user", &filter).unwrap());
    assert!(
        !observation_matches_filter(
            &encoded,
            "user",
            &TemporalCandidateFilterV1 {
                start_time: Some(43),
                ..filter.clone()
            },
        )
        .unwrap()
    );
    assert!(
        !observation_matches_filter(&encoded_observation(vec![goal]), "user", &filter,).unwrap(),
        "a Goal without Message.timestamp or canonical observation time stays ineligible"
    );
}

#[test]
fn goal_role_and_time_eligibility_are_conjunctive_before_ranking() {
    let encoded = encoded_observation(vec![
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

    assert!(observation_matches_filter(&encoded, "user", &filter).unwrap());

    let too_late = TemporalCandidateFilterV1 {
        start_time: Some(43),
        ..filter
    };
    assert!(!observation_matches_filter(&encoded, "user", &too_late).unwrap());
}

#[test]
fn tool_results_do_not_leak_into_direct_user_filter() {
    let encoded = encoded_observation(vec![
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

    assert!(!observation_matches_filter(&encoded, "user", &direct).unwrap());
    assert!(observation_matches_filter(&encoded, "user", &tool).unwrap());
}

#[test]
fn canonical_source_filter_matches_provider_or_source_identity_before_ranking() {
    let encoded = encoded_observation(vec![CanonicalObservationFactV1::Message {
        role: CanonicalMessageRoleV1::User,
        content: json!({"text": "source-bound evidence"}),
        model: None,
        timestamp: Some(42),
    }]);

    for source in ["codex", "session-semantic-filter"] {
        assert!(
            observation_matches_filter(
                &encoded,
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
            &encoded,
            "user",
            &TemporalCandidateFilterV1 {
                source: Some("claude".to_string()),
                ..TemporalCandidateFilterV1::default()
            },
        )
        .unwrap()
    );
}
