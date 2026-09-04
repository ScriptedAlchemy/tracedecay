use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_capture::cursor::{
    cursor_observation_identity, cursor_projected_message_id,
    normalize_cursor_observation_with_message_id,
};
use tracedecay_domain::{
    ComponentVersion, DurableObservationV1, ObservationCollisionOutcomeV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId, ProviderId,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_store::observation::ObservationIdentityCollisionDispositionV1;
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationStore, ObservationStoreError,
    ObservationWrite, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};

fn receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.cursor-identity-fallback.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn cursor_observation(
    native: &Value,
    session_id: &SessionId,
    range: ObservationSourceRangeV1,
    record_id: ObservationId,
    receipt_id: &str,
) -> DurableObservationV1 {
    let projected_message_id =
        cursor_projected_message_id(native, session_id.as_str(), range.start(), 1, false).unwrap();
    let envelope = normalize_cursor_observation_with_message_id(
        native,
        session_id.as_str(),
        record_id.clone(),
        projected_message_id,
        range,
        None,
        None,
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("cursor").unwrap(),
        session_id.clone(),
    )
    .unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::FileBytes,
        record_id,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        receipt(receipt_id, &payload),
        RetentionClass::new("retention.cursor-transcript").unwrap(),
        payload,
    )
    .unwrap()
}

fn anchored_write(
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> AnchoredObservationWrite {
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new("projection.cursor-identity-fallback.v1").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "cursor").unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

#[tokio::test]
async fn cursor_positional_identity_retries_without_consuming_the_primary_frontier() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.cursor-identical-no-id-records").unwrap();
    let native = json!({
        "role": "assistant",
        "message": {"content": "identical no-id Cursor record"}
    });
    let encoded_len = u64::try_from(serde_json::to_vec(&native).unwrap().len()).unwrap();
    let first_range = ObservationSourceRangeV1::new(0, encoded_len).unwrap();
    let second_range =
        ObservationSourceRangeV1::new(encoded_len, encoded_len.saturating_mul(2)).unwrap();
    let first_identity =
        cursor_observation_identity(session_id.as_str(), &native, first_range).unwrap();
    let second_identity =
        cursor_observation_identity(session_id.as_str(), &native, second_range).unwrap();
    assert_eq!(first_identity.primary(), second_identity.primary());
    let positional_id = second_identity
        .collision_disambiguation()
        .expect("a no-id Cursor record must offer one positional fallback")
        .clone();
    assert_ne!(positional_id, *second_identity.primary());

    let original = cursor_observation(
        &native,
        &session_id,
        first_range,
        first_identity.into_primary(),
        "receipt.cursor-identity-fallback.original",
    );
    assert!(matches!(
        store
            .persist_observation(anchored_write(original.clone(), None))
            .await
            .unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let frontier = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();

    let primary = cursor_observation(
        &native,
        &session_id,
        second_range,
        second_identity.into_primary(),
        "receipt.cursor-identity-fallback.primary",
    );
    let primary_error = store
        .persist_observation(
            anchored_write(primary.clone(), frontier.clone()).with_identity_collision_disposition(
                ObservationIdentityCollisionDispositionV1::RetryWithAlternateIdentity,
            ),
        )
        .await
        .expect_err("the second occurrence must collide on the legacy content identity");
    assert!(matches!(
        primary_error,
        ObservationStoreError::ObservationCollision {
            outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            ..
        }
    ));
    assert_eq!(
        store
            .get_source_cursor(primary.source(), primary.scope())
            .await
            .unwrap(),
        frontier,
        "the retryable primary collision must not consume the source frontier"
    );

    let fallback = cursor_observation(
        &native,
        &session_id,
        second_range,
        positional_id,
        "receipt.cursor-identity-fallback.positional",
    );
    let fallback_write = anchored_write(fallback.clone(), frontier);
    assert!(matches!(
        store
            .persist_observation(fallback_write.clone())
            .await
            .unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    assert!(
        store
            .get_observation(fallback.observation_id())
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        store.persist_observation(fallback_write).await.unwrap(),
        ObservationPersistOutcome::ExactDuplicate(_)
    ));
}

#[tokio::test]
async fn cursor_native_replay_across_offsets_is_covered_but_changed_content_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.cursor-native-conflict").unwrap();
    let first_native = json!({
        "id": "cursor-native-conflict",
        "role": "assistant",
        "message": {"content": "first content"}
    });
    let mirrored_native = json!({
        "id": null,
        "role": "assistant",
        "message": {
            "id": "cursor-native-conflict",
            "content": "first content"
        }
    });
    let changed_native = json!({
        "id": null,
        "role": "assistant",
        "message": {
            "id": "cursor-native-conflict",
            "content": "changed content"
        }
    });
    let first_range = ObservationSourceRangeV1::new(0, 64).unwrap();
    let mirrored_range = ObservationSourceRangeV1::new(64, 128).unwrap();
    let changed_range = ObservationSourceRangeV1::new(128, 192).unwrap();
    let first_identity =
        cursor_observation_identity(session_id.as_str(), &first_native, first_range).unwrap();
    let mirrored_identity =
        cursor_observation_identity(session_id.as_str(), &mirrored_native, mirrored_range).unwrap();
    let changed_identity =
        cursor_observation_identity(session_id.as_str(), &changed_native, changed_range).unwrap();
    assert_eq!(first_identity.primary(), mirrored_identity.primary());
    assert_eq!(first_identity.primary(), changed_identity.primary());
    assert!(first_identity.collision_disambiguation().is_none());
    assert!(mirrored_identity.collision_disambiguation().is_none());
    assert!(changed_identity.collision_disambiguation().is_none());

    let original = cursor_observation(
        &first_native,
        &session_id,
        first_range,
        first_identity.into_primary(),
        "receipt.cursor-native-conflict.original",
    );
    assert!(matches!(
        store
            .persist_observation(anchored_write(original.clone(), None))
            .await
            .unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
    let first_frontier = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();
    let mirrored = cursor_observation(
        &mirrored_native,
        &session_id,
        mirrored_range,
        mirrored_identity.into_primary(),
        "receipt.cursor-native-conflict.mirrored",
    );
    assert_eq!(mirrored.observation_id(), original.observation_id());
    assert_eq!(
        mirrored.payload().pointer("/relations/message_id"),
        original.payload().pointer("/relations/message_id")
    );
    assert!(matches!(
        store
            .persist_observation(anchored_write(mirrored, first_frontier))
            .await
            .unwrap(),
        ObservationPersistOutcome::CoveredDuplicate(_)
    ));
    let mirrored_frontier = store
        .get_source_cursor(original.source(), original.scope())
        .await
        .unwrap();
    let changed = cursor_observation(
        &changed_native,
        &session_id,
        changed_range,
        changed_identity.into_primary(),
        "receipt.cursor-native-conflict.changed",
    );
    let error = store
        .persist_observation(anchored_write(changed, mirrored_frontier))
        .await
        .expect_err("changed content under one native id must fail closed");
    assert!(matches!(
        error,
        ObservationStoreError::ObservationCollision {
            outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            ..
        }
    ));

    let retained = store
        .get_observation(original.observation_id())
        .await
        .unwrap()
        .expect("the original native record must remain authoritative");
    assert_eq!(retained.observation().payload(), original.payload());
}
