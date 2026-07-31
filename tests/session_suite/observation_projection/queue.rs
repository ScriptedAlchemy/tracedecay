use super::*;

#[tokio::test]
async fn v3_projection_persists_stable_multi_output_ordinals() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let provider = ProviderId::new("cursor").unwrap();
    let session_id = SessionId::new("session.multi-output").unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let generation = ObservationSourceGenerationV1::new(1).unwrap();
    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let record_id = ObservationId::new("record.multi-output").unwrap();
    let message_id = ObservationId::new("message.multi-output").unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "composer_bubble",
        record_id.clone(),
        CanonicalObservationRelationsV1::new(session_id).with_message_id(message_id.clone()),
        vec![
            CanonicalObservationFactV1::Session {
                project_path: Some("/workspace/project".to_owned()),
                location_path: None,
                transcript_path: None,
                title: None,
                started_at: None,
                ended_at: None,
                source: Some("cursor_composer".to_owned()),
                native_source: Some("cursor".to_owned()),
                profile: None,
                location_provenance: None,
            },
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: json!("authored"),
                model: None,
                timestamp: Some(42),
            },
            CanonicalObservationFactV1::Reasoning {
                visibility: CanonicalReasoningVisibilityV1::Visible,
                content: Some(json!("reasoning")),
            },
            CanonicalObservationFactV1::ToolInvocation {
                invocation_id: ObservationId::new("tool.multi-output").unwrap(),
                name: "edit_file".to_owned(),
                arguments: json!({"path": "src/lib.rs"}),
            },
            CanonicalObservationFactV1::Git {
                evidence_kind: CanonicalGitEvidenceKindV1::PullRequest,
                reference: Some("https://example.invalid/pr/1".to_owned()),
                content: None,
            },
        ],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            generation,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            record_id,
        )
        .unwrap(),
        receipt("receipt.multi-output", &payload),
        RetentionClass::new("retention.projection-test").unwrap(),
        payload,
    )
    .unwrap();
    store
        .persist_observation(canonical_write(observation))
        .await
        .unwrap();
    let queued = store.next_queued_observation().await.unwrap().unwrap();
    let outcome = store.project_observation(&queued).await.unwrap();
    let ProjectionPersistOutcome::Projected(projected) = outcome else {
        panic!("observation should project");
    };
    assert_eq!(projected.output_count(), 4);
    drop(runtime);

    let conn = rusqlite::Connection::open(database_path).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT output_ordinal, output_message_id
             FROM observation_projection_provenance
             WHERE projector_version = ?1
             ORDER BY output_ordinal",
        )
        .unwrap();
    let actual = statement
        .query_map(
            rusqlite::params![SESSION_MESSAGE_PROJECTOR_VERSION_V4],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        actual,
        vec![
            (0, message_id.as_str().to_owned()),
            (1, format!("{}:thinking", message_id.as_str())),
            (2, format!("{}:tool", message_id.as_str())),
            (3, format!("{}:pr:0", message_id.as_str())),
        ]
    );
}

#[tokio::test]
async fn queued_projection_commits_search_effect_provenance_checkpoint_and_replay_noop() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let candidate = observation(
        "session-atomic",
        0,
        100,
        "receipt.atomic-projection",
        json!({
            "type": "assistant",
            "uuid": "record-message-atomic",
            "timestamp": "2025-06-15T15:08:43Z",
            "message": {
                "id": "message-atomic",
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "private-reasoning-canary"},
                    {"type": "text", "text": "atomic searchable canary"},
                    {"type": "tool_use", "name": "Read", "input": {"path": "README.md"}}
                ],
                "model": "claude-sonnet-4"
            }
        }),
    );
    let sequence = persist(&store, candidate.clone(), None).await;
    assert_eq!(sequence, 1);
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        0
    );

    let projected = store
        .project_observation(candidate.observation_id())
        .await
        .unwrap();
    assert!(matches!(projected, ProjectionPersistOutcome::Projected(_)));
    assert_eq!(projected.checkpoint().last_sequence(), sequence);
    assert_eq!(
        projected.checkpoint().projector_version(),
        CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION
    );
    assert_eq!(
        store
            .get_observation(candidate.observation_id())
            .await
            .unwrap()
            .unwrap()
            .projection_status(),
        ObservationProjectionStatus::NotQueued
    );
    let hits = search_session_messages(&tmp, "atomic searchable", 10).await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message.message_id, "message-atomic");
    assert_eq!(hits[0].message.role, "assistant");
    assert_eq!(hits[0].message.timestamp, Some(1_750_000_123));
    assert_eq!(hits[0].message.ordinal, 0);
    assert_eq!(hits[0].message.kind.as_deref(), Some("message"));
    assert_eq!(hits[0].message.model.as_deref(), Some("claude-sonnet-4"));
    assert_eq!(hits[0].message.tool_names.as_deref(), Some("Read"));
    assert_eq!(
        hits[0].message.source_path.as_deref(),
        Some("claude:session-atomic")
    );
    assert_eq!(hits[0].message.source_offset, Some(0));
    assert!(!hits[0].message.text.contains("private-reasoning-canary"));
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 0, 0));
    let provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(provenance.len(), 1);
    assert_eq!(provenance[0].0, CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION);
    assert_eq!(provenance[0].1, candidate.observation_id().as_str());
    assert_eq!(
        provenance[0].2,
        derive_exact_observation_anchor_id(candidate.scope(), candidate.observation_id())
            .unwrap()
            .as_str()
    );
    assert_eq!(provenance[0].3, "receipt.atomic-projection");
    assert_eq!(provenance[0].4, "claude");
    assert_eq!(provenance[0].5, "message-atomic");
    assert!(PayloadDigestV1::new(provenance[0].6.clone()).is_ok());

    let raw_conn = rusqlite::Connection::open(database_path).unwrap();
    assert!(
        raw_conn
            .execute(
                "UPDATE observation_projection_provenance
                 SET retrieval_anchor_id = NULL
                 WHERE projector_version = ?1 AND observation_id = ?2",
                rusqlite::params![
                    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                    candidate.observation_id().as_str()
                ],
            )
            .is_err()
    );
    drop(raw_conn);

    let before = projection_counts(&tmp).await;
    let replay = store
        .project_observation(candidate.observation_id())
        .await
        .unwrap();
    assert!(matches!(
        replay,
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    assert_eq!(replay.checkpoint(), projected.checkpoint());
    assert_eq!(projection_counts(&tmp).await, before);
}

#[tokio::test]
async fn non_conversational_observation_is_skipped_without_blocking_the_checkpoint() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let skipped = observation(
        "session-skip",
        0,
        50,
        "receipt.skip",
        json!({"type": "progress", "data": {"status": "working"}}),
    );
    let message = observation(
        "session-skip",
        50,
        100,
        "receipt.after-skip",
        conversational_payload("message-after-skip", "checkpoint advanced canary"),
    );
    persist(&store, skipped.clone(), None).await;
    persist(&store, message.clone(), Some(cursor("session-skip", 50))).await;

    let outcome = store
        .project_observation(skipped.observation_id())
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        ProjectionPersistOutcome::Skipped {
            reason: ProjectionSkipReason::NonConversationalRecord,
            ..
        }
    ));
    assert_eq!(outcome.checkpoint().last_sequence(), 1);
    assert_eq!(projection_counts(&tmp).await, (0, 0, 0, 1, 1, 1));
    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(message.observation_id())
    );

    store
        .project_observation(message.observation_id())
        .await
        .unwrap();
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        2
    );
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 1, 0));
    assert!(matches!(
        store
            .project_observation(skipped.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    let rebuilt = rebuild_projection_to_completion(&store, 2).await;
    assert_eq!(rebuilt.projected_rows(), 1);
    assert_eq!(rebuilt.skipped_observations(), 1);
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 1, 0));
}

#[tokio::test]
async fn bounded_next_queue_item_resumes_after_restart_and_drains_idempotently() {
    let tmp = TempDir::new().unwrap();
    let first = observation(
        "session-restart",
        0,
        100,
        "receipt.restart-1",
        conversational_payload("message-restart-1", "restart first canary"),
    );
    let second = observation(
        "session-restart",
        100,
        200,
        "receipt.restart-2",
        conversational_payload("message-restart-2", "restart second canary"),
    );

    {
        let runtime = profile_runtime(&tmp).await;
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        persist(&store, first.clone(), None).await;
        persist(&store, second.clone(), Some(cursor("session-restart", 100))).await;
        assert_eq!(
            store.next_queued_observation().await.unwrap().as_ref(),
            Some(first.observation_id())
        );
        store
            .project_observation(first.observation_id())
            .await
            .unwrap();
    }

    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        1
    );
    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(second.observation_id())
    );
    store
        .project_observation(second.observation_id())
        .await
        .unwrap();
    assert!(store.next_queued_observation().await.unwrap().is_none());
    assert!(matches!(
        store
            .project_observation(second.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    assert_eq!(projection_counts(&tmp).await, (1, 2, 2, 1, 0, 0));
}

#[tokio::test]
async fn stale_exact_duplicate_queue_item_is_consumed_before_later_observation() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let first = observation(
        "session-stale-queue",
        0,
        100,
        "receipt.stale-queue-1",
        conversational_payload("message-stale-queue-1", "stale queue first canary"),
    );
    let second = observation(
        "session-stale-queue",
        100,
        200,
        "receipt.stale-queue-2",
        conversational_payload("message-stale-queue-2", "stale queue second canary"),
    );
    persist(&store, first.clone(), None).await;
    persist(
        &store,
        second.clone(),
        Some(cursor("session-stale-queue", 100)),
    )
    .await;
    store
        .project_observation(first.observation_id())
        .await
        .unwrap();

    rusqlite::Connection::open(database_path)
        .unwrap()
        .execute(
            "INSERT INTO projection_queue (observation_id, observation_sequence)
             VALUES (?1, 1)",
            rusqlite::params![first.observation_id().as_str()],
        )
        .unwrap();

    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(first.observation_id())
    );
    assert!(matches!(
        store
            .project_observation(first.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    assert_eq!(
        store.next_queued_observation().await.unwrap().as_ref(),
        Some(second.observation_id())
    );

    drain_projection_queue(&store).await;
    assert!(store.next_queued_observation().await.unwrap().is_none());
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        2
    );
    assert_eq!(projection_counts(&tmp).await, (1, 2, 2, 1, 0, 0));
}
