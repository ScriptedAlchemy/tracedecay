use super::*;

#[tokio::test]
async fn batch_receipts_require_contiguous_ordinals_and_replay_exactly() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.receipts");
    let persisted = persist_observation(&observation_store, &session_id, 0, "receipt").await;
    let projected = occurrence(&session_id, &persisted);
    begin_candidate(&store, &session_id, 2, 1).await;

    let skipped = batch(&session_id, 2, 1, vec![projected.clone()], vec![], vec![])
        .with_checkpoint(1, 1, 1)
        .unwrap();
    assert!(
        store
            .persist_session_temporal_projection_batch(skipped)
            .await
            .is_err()
    );

    let first = batch(&session_id, 2, 1, vec![projected], vec![], vec![])
        .with_checkpoint(0, 1, 1)
        .unwrap();
    assert_eq!(
        store
            .persist_session_temporal_projection_batch(first.clone())
            .await
            .unwrap()
            .disposition(),
        SessionTemporalProjectionBatchDispositionV1::Applied
    );
    assert_eq!(
        store
            .persist_session_temporal_projection_batch(first)
            .await
            .unwrap()
            .disposition(),
        SessionTemporalProjectionBatchDispositionV1::ExactReplay
    );
    let wrong_ordinal = batch(
        &session_id,
        2,
        1,
        vec![occurrence(&session_id, &persisted)],
        vec![],
        vec![],
    )
    .with_checkpoint(1, 1, 1)
    .unwrap();
    assert!(
        store
            .persist_session_temporal_projection_batch(wrong_ordinal)
            .await
            .is_err()
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts"
        )
        .await,
        1
    );

    let conflict = batch(&session_id, 2, 1, vec![], vec![], vec![])
        .with_checkpoint(0, 1, 1)
        .unwrap();
    assert!(
        store
            .persist_session_temporal_projection_batch(conflict)
            .await
            .is_err()
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn caller_forged_occurrence_fields_never_cross_the_canonical_boundary() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.untrusted");
    let first = persist_observation(&observation_store, &session_id, 0, "first").await;
    let second = persist_observation(&observation_store, &session_id, 1, "second").await;
    let canonical = occurrence(&session_id, &first);
    begin_candidate(&store, &session_id, 2, 2).await;

    let mut forged = Vec::new();
    let mut knowledge = canonical.clone();
    knowledge.knowledge_at = UtcMicros(2);
    forged.push(knowledge);
    let mut anchor = canonical.clone();
    anchor.retrieval_anchor_id = occurrence(&session_id, &second).retrieval_anchor_id;
    forged.push(anchor);
    let mut message = canonical.clone();
    message.message_id = Some(tracedecay_domain::MessageId::new("message.forged").unwrap());
    forged.push(message);
    let mut authority = canonical;
    authority.evidence.authority = tracedecay_domain::SessionAuthorityClassV1::ProviderNative;
    forged.push(authority);

    for occurrence in forged {
        assert!(
            store
                .persist_session_temporal_projection_batch(batch(
                    &session_id,
                    2,
                    2,
                    vec![occurrence],
                    vec![],
                    vec![],
                ))
                .await
                .is_err()
        );
    }
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_occurrences").await,
        0
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn incremental_batch_commit_is_atomic_and_rolls_back_on_late_failure() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.atomic");
    let persisted = persist_observation(&observation_store, &session_id, 0, "atomic").await;
    let persisted_occurrence = occurrence(&session_id, &persisted);
    let missing = occurrence(&session_id, &observation(&session_id, 99, "not persisted"));
    begin_candidate(&store, &session_id, 2, 1).await;

    let result = store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            1,
            vec![persisted_occurrence.clone()],
            vec![parent_message_copy(&persisted_occurrence, &missing)],
            vec![],
        ))
        .await;

    assert!(matches!(result, Err(SessionStoreError::Storage { .. })));
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_occurrences").await,
        0
    );
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_threads").await,
        0
    );
    for table in [
        "session_turns",
        "session_agents",
        "session_turn_members",
        "session_assertions",
        "session_assertion_supersession",
        "session_current_entities",
    ] {
        assert_eq!(
            scalar(&path, &format!("SELECT COUNT(*) FROM {table}")).await,
            0,
            "{table} must roll back with the rejected batch"
        );
    }
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_occurrences_fts").await,
        0
    );
}

#[tokio::test]
async fn batches_reject_cross_session_and_cross_generation_ownership() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.owner");
    let observation = persist_observation(&observation_store, &session_id, 0, "owner").await;
    begin_candidate(&store, &session_id, 2, 1).await;

    let other_session = session("session.temporal.other");
    assert!(matches!(
        SessionTemporalProjectionBatchV1::new(
            session_id.clone(),
            generation(2),
            watermarks(1, 1),
            vec![occurrence(&other_session, &observation)],
            vec![],
            vec![],
        ),
        Err(SessionStoreError::SessionMismatch { .. })
    ));
    assert!(matches!(
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                3,
                1,
                vec![occurrence(&session_id, &observation)],
                vec![],
                vec![],
            ))
            .await,
        Err(SessionStoreError::MissingGeneration { .. })
            | Err(SessionStoreError::ProjectionBatchGenerationMismatch)
    ));
}

#[tokio::test]
async fn exact_replay_is_idempotent_and_conflicting_replay_rolls_back() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.replay");
    let observation = persist_observation(&observation_store, &session_id, 0, "replay").await;
    let occurrence = occurrence(&session_id, &observation);
    begin_candidate(&store, &session_id, 2, 1).await;
    let projection = batch(&session_id, 2, 1, vec![occurrence.clone()], vec![], vec![]);

    assert_eq!(
        store
            .persist_session_temporal_projection_batch(projection.clone())
            .await
            .unwrap()
            .disposition(),
        SessionTemporalProjectionBatchDispositionV1::Applied
    );
    assert_eq!(
        store
            .persist_session_temporal_projection_batch(projection)
            .await
            .unwrap()
            .disposition(),
        SessionTemporalProjectionBatchDispositionV1::ExactReplay
    );
    let canonical_knowledge_at = scalar(
        &path,
        "SELECT knowledge_at FROM session_occurrences LIMIT 1",
    )
    .await;

    let mut conflicting = occurrence;
    conflicting.knowledge_at = UtcMicros(51);
    assert!(matches!(
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                2,
                1,
                vec![conflicting],
                vec![],
                vec![],
            ))
            .await,
        Err(SessionStoreError::Storage { .. })
    ));
    assert_eq!(
        scalar(
            &path,
            "SELECT knowledge_at FROM session_occurrences LIMIT 1"
        )
        .await,
        canonical_knowledge_at
    );
}

#[tokio::test]
async fn projection_batch_rejects_item_count_above_max() {
    let session_id = session("session.temporal.batch-limit");
    let observation = observation(&session_id, 0, "limit");
    let projected = occurrence(&session_id, &observation);
    let oversized = vec![projected; MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS + 1];
    assert!(matches!(
        SessionTemporalProjectionBatchV1::new(
            session_id,
            generation(2),
            watermarks(1, 1),
            oversized,
            vec![],
            vec![],
        ),
        Err(SessionStoreError::BatchLimitExceeded { max, .. })
            if max == MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS
    ));
}

#[tokio::test]
async fn duplicate_message_ids_within_one_batch_are_rejected_deterministically() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.duplicate-within");
    let duplicate = "message.temporal.duplicate";
    let first = persist_custom_observation(
        &observation_store,
        observation_with_message_ids(&session_id, 0, "first", duplicate, None),
    )
    .await;
    let second = persist_custom_observation(
        &observation_store,
        observation_with_message_ids(&session_id, 1, "second", duplicate, None),
    )
    .await;
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![
                occurrence_with_message_id(&session_id, &first, duplicate),
                occurrence_with_message_id(&session_id, &second, duplicate),
            ],
            vec![],
            vec![],
        ))
        .await
        .unwrap();

    let error = store
        .activate_session_temporal_generation(
            SessionGenerationActivationRequestV1::new(
                session_id.clone(),
                generation(2),
                snapshot(&session_id, 1, 2),
                ExecutionControl::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:?}").contains("resolves to 2 occurrences"),
        "unexpected ambiguity error: {error:?}"
    );
}

#[tokio::test]
async fn duplicate_message_ids_across_batches_are_rejected_deterministically() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.duplicate-across");
    let duplicate = "message.temporal.duplicate";
    let first = persist_custom_observation(
        &observation_store,
        observation_with_message_ids(&session_id, 0, "first", duplicate, None),
    )
    .await;
    let second = persist_custom_observation(
        &observation_store,
        observation_with_message_ids(&session_id, 1, "second", duplicate, None),
    )
    .await;
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(
            batch(
                &session_id,
                2,
                2,
                vec![occurrence_with_message_id(&session_id, &first, duplicate)],
                vec![],
                vec![],
            )
            .with_checkpoint(0, 1, 1)
            .unwrap(),
        )
        .await
        .unwrap();
    store
        .persist_session_temporal_projection_batch(
            batch(
                &session_id,
                2,
                2,
                vec![occurrence_with_message_id(&session_id, &second, duplicate)],
                vec![],
                vec![],
            )
            .with_checkpoint(1, 2, 2)
            .unwrap(),
        )
        .await
        .unwrap();

    let error = store
        .activate_session_temporal_generation(
            SessionGenerationActivationRequestV1::new(
                session_id.clone(),
                generation(2),
                snapshot(&session_id, 1, 2),
                ExecutionControl::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:?}").contains("resolves to 2 occurrences"));
}

#[tokio::test]
async fn duplicate_message_ids_remain_rejected_after_restart() {
    let tmp = TempDir::new().unwrap();
    let session_id = session("session.temporal.duplicate-restart");
    let duplicate = "message.temporal.duplicate";
    let second = {
        let runtime = profile_runtime(&tmp).await;
        let observation_store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let store = runtime
            .session_temporal_store(HostAdmissionScope::Profile)
            .unwrap();
        let first = persist_custom_observation(
            &observation_store,
            observation_with_message_ids(&session_id, 0, "first", duplicate, None),
        )
        .await;
        let second = persist_custom_observation(
            &observation_store,
            observation_with_message_ids(&session_id, 1, "second", duplicate, None),
        )
        .await;
        begin_candidate(&store, &session_id, 2, 2).await;
        store
            .persist_session_temporal_projection_batch(
                batch(
                    &session_id,
                    2,
                    2,
                    vec![occurrence_with_message_id(&session_id, &first, duplicate)],
                    vec![],
                    vec![],
                )
                .with_checkpoint(0, 1, 1)
                .unwrap(),
            )
            .await
            .unwrap();
        second
    };
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    assert_eq!(
        begin_candidate(&store, &session_id, 2, 2).await,
        SessionGenerationRebuildDispositionV1::Resumed
    );
    store
        .persist_session_temporal_projection_batch(
            batch(
                &session_id,
                2,
                2,
                vec![occurrence_with_message_id(&session_id, &second, duplicate)],
                vec![],
                vec![],
            )
            .with_checkpoint(1, 2, 2)
            .unwrap(),
        )
        .await
        .unwrap();

    let error = store
        .activate_session_temporal_generation(
            SessionGenerationActivationRequestV1::new(
                session_id.clone(),
                generation(2),
                snapshot(&session_id, 1, 2),
                ExecutionControl::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:?}").contains("resolves to 2 occurrences"));
}
