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

/// A Claude row projects under its `uuid`; a row without one falls back to the
/// host's positional `session:offset` record id.
#[tokio::test]
async fn claude_row_uuid_or_positional_record_id_is_the_message_id() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let mut with_uuid = claude_records::assistant_record("row uuid body");
    with_uuid["uuid"] = Value::from("row-uuid");
    persist(
        &store,
        observation("session-record-ids", 0, 100, "receipt.row-uuid", with_uuid),
        None,
    )
    .await;
    persist(
        &store,
        observation(
            "session-record-ids",
            100,
            200,
            "receipt.positional",
            claude_records::assistant_record("positional body"),
        ),
        Some(cursor("session-record-ids", 100)),
    )
    .await;
    drain_projection_queue(&store).await;

    assert_eq!(
        projection_output_ids(&projection_provenance_rows(&tmp).await),
        ["row-uuid", "session-record-ids:100"]
    );
}
