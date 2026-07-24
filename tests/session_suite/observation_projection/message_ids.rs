use super::*;

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
