use super::*;

#[tokio::test]
async fn hermes_v1_message_identity_projects_unchanged() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let provider = ProviderId::new("hermes").unwrap();
    let session_id = SessionId::new("session-redacted").unwrap();
    let message_id = ObservationId::new("20260101_000000_abc123:7").unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let generation = ObservationSourceGenerationV1::new(1).unwrap();
    let range = ObservationSourceRangeV1::new(1, 7).unwrap();
    let record_id = ObservationId::new("record.hermes.message-identity").unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        CanonicalObservationRelationsV1::new(session_id).with_message_id(message_id.clone()),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": "safe fixture content"}),
            model: Some("model-redacted".to_owned()),
            timestamp: Some(1_750_000_000),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SqliteRowId, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            generation,
            range,
            ObservationOrderingDomainV1::SqliteRowId,
            record_id,
        )
        .unwrap(),
        receipt("receipt.hermes.message-identity", &payload),
        RetentionClass::new("transcript.hermes.v1").unwrap(),
        payload,
    )
    .unwrap();
    assert!(matches!(
        store
            .persist_observation(canonical_write(observation))
            .await
            .unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    drain_projection_queue(&store).await;

    assert_eq!(
        projection_output_ids(&projection_provenance_rows(&tmp).await),
        [message_id.as_str()]
    );
}

#[tokio::test]
async fn safe_sanitized_uuid_remains_the_v1_message_id() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let payload = json!({
        "type": "assistant",
        "uuid": "safe-sanitized-uuid",
        "timestamp": "2025-06-15T15:06:40Z",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": "safe UUID body"}],
            "model": "claude-sonnet-4"
        }
    });
    persist(
        &store,
        observation("session-safe-uuid", 0, 100, "receipt.safe-uuid", payload),
        None,
    )
    .await;
    drain_projection_queue(&store).await;

    assert_eq!(
        projection_output_ids(&projection_provenance_rows(&tmp).await),
        ["safe-sanitized-uuid"]
    );
}

#[tokio::test]
async fn redacted_message_ids_use_injective_v1_fallbacks() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let marker = "[TraceDecay redacted:message-id]";
    let mut first = conversational_payload(marker, "first redacted message ID");
    first["uuid"] = Value::from("record-first-redacted-message-id");
    let mut second = conversational_payload(marker, "second redacted message ID");
    second["uuid"] = Value::from("record-second-redacted-message-id");
    persist(
        &store,
        observation(
            "session-redacted-message-id",
            0,
            100,
            "receipt.redacted-message-id-first",
            first,
        ),
        None,
    )
    .await;
    persist(
        &store,
        observation(
            "session-redacted-message-id",
            100,
            200,
            "receipt.redacted-message-id-second",
            second,
        ),
        Some(cursor("session-redacted-message-id", 100)),
    )
    .await;
    drain_projection_queue(&store).await;

    assert_eq!(
        projection_output_ids(&projection_provenance_rows(&tmp).await),
        [
            "session-redacted-message-id:11:0",
            "session-redacted-message-id:11:100",
        ]
    );
}

#[tokio::test]
async fn redacted_uuid_ids_use_injective_v1_fallbacks() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    for (start, end, receipt_id, text) in [
        (0, 100, "receipt.redacted-uuid-first", "first redacted UUID"),
        (
            100,
            200,
            "receipt.redacted-uuid-second",
            "second redacted UUID",
        ),
    ] {
        let payload = json!({
            "type": "assistant",
            "uuid": "[TraceDecay redacted:uuid]",
            "timestamp": "2025-06-15T15:06:40Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": text}],
                "model": "claude-sonnet-4"
            }
        });
        persist(
            &store,
            observation("session-redacted-uuid", start, end, receipt_id, payload),
            (start != 0).then(|| cursor("session-redacted-uuid", start)),
        )
        .await;
    }
    drain_projection_queue(&store).await;

    assert_eq!(
        projection_output_ids(&projection_provenance_rows(&tmp).await),
        ["session-redacted-uuid:11:0", "session-redacted-uuid:11:100",]
    );
}
