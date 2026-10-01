use super::*;

#[tokio::test]
async fn first_session_refresh_bootstraps_active_generation_under_writer_authority() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let session_id = session("session.temporal.bootstrap");
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();

    let candidate = begin_candidate(&store, &session_id, 0).await;
    assert_eq!(candidate.candidate_generation(), generation(2));
    assert_eq!(
        rows(
            &path,
            "SELECT generation || ':' || state
             FROM session_temporal_generations
             WHERE session_id = 'session.temporal.bootstrap'
             ORDER BY generation"
        )
        .await,
        vec!["1:active", "2:building"]
    );
}

/// Projects the parity fixture into the refresh candidate of a fresh profile,
/// either as one batch or as two checkpointed batches, and returns the
/// canonical rows that candidate reads.
async fn project_parity_fixture(tmp: &TempDir, incremental: bool) -> Vec<Vec<String>> {
    let runtime = profile_runtime(tmp).await;
    let path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.parity");
    let first = occurrence(
        &session_id,
        &persist_observation(&observation_store, &session_id, 0, "first").await,
    );
    let second = occurrence(
        &session_id,
        &persist_observation_with_lineage(
            &observation_store,
            &session_id,
            1,
            "second",
            AnchorProvenanceRelation::Supersedes,
            first.retrieval_anchor_id.clone(),
            None,
        )
        .await,
    );
    let edge = parent_message_copy(&second, &first);
    let assertion = assertion(&second, &first);
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let candidate = begin_candidate(&store, &session_id, 2).await;
    if incremental {
        persist_batch(
            &store,
            &candidate,
            batch(&candidate, vec![first], vec![], vec![])
                .with_checkpoint(0, 1, 1)
                .unwrap(),
        )
        .await
        .unwrap();
        persist_batch(
            &store,
            &candidate,
            batch(&candidate, vec![second], vec![edge], vec![assertion])
                .with_checkpoint(1, 2, 2)
                .unwrap(),
        )
        .await
        .unwrap();
    } else {
        persist_batch(
            &store,
            &candidate,
            batch(&candidate, vec![first, second], vec![edge], vec![assertion]),
        )
        .await
        .unwrap();
    }

    let mut projected = Vec::new();
    for projection in [
        "SELECT json_object(
                'occurrence_id', occurrence_id,
                'source_observation_id', source_observation_id,
                'source_sequence', source_sequence,
                'projection_output_ordinal', projection_output_ordinal,
                'retrieval_anchor_id', retrieval_anchor_id,
                'thread_id', thread_id,
                'thread_grouping_json', json(thread_grouping_json),
                'turn_id', turn_id,
                'turn_grouping_json', json(turn_grouping_json),
                'message_id', message_id,
                'agent_id', agent_id,
                'parent_message_id', parent_message_id,
                'parent_agent_id', parent_agent_id,
                'parent_session_id', parent_session_id,
                'copied_from_anchor_ids_json', json(copied_from_anchor_ids_json),
                'role', role,
                'knowledge_at', knowledge_at,
                'valid_time_json', json(valid_time_json),
                'evidence_json', json(evidence_json),
                'snippet_text', snippet_text,
                'index_text', index_text
             )
             FROM session_occurrences
             WHERE generation <= 2
             ORDER BY knowledge_at, occurrence_id",
        "SELECT assertion_id || ':' || assertion_kind || ':' ||
                subject_anchor_id || ':' || object_anchor_id || ':' ||
                valid_time_json || ':' || evidence_json
         FROM session_assertions
         WHERE generation <= 2
         ORDER BY assertion_id",
        "SELECT entity_kind || ':' || entity_id || ':' ||
                COALESCE(current_assertion_id, '') || ':' ||
                COALESCE(current_occurrence_id, '') || ':' || coverage_json
         FROM session_current_entities
         WHERE generation <= 2
         ORDER BY entity_kind, entity_id",
        "SELECT turn_id || ':' || occurrence_id || ':' || ordinal
         FROM session_turn_members
         WHERE generation <= 2
         ORDER BY turn_id, ordinal, occurrence_id",
        "SELECT thread_id || ':' || grouping_provenance || ':' || created_at
         FROM session_threads
         WHERE generation <= 2
         ORDER BY thread_id",
        "SELECT turn_id || ':' || ordinal || ':' || grouping_provenance || ':' || created_at
         FROM session_turns
         WHERE generation <= 2
         ORDER BY turn_id",
        "SELECT agent_id || ':' || agent_json || ':' || created_at
         FROM session_agents
         WHERE generation <= 2
         ORDER BY agent_id",
        "SELECT superseded_assertion_id || ':' || superseding_assertion_id || ':' || created_at
         FROM session_assertion_supersession
         WHERE generation <= 2
         ORDER BY superseded_assertion_id, superseding_assertion_id",
        "SELECT occurrence.occurrence_id || ':' || fts.index_text || ':' || occurrence.snippet_text
         FROM session_occurrences AS occurrence
         JOIN session_occurrences_fts AS fts ON fts.rowid = occurrence.rowid
         WHERE occurrence.generation <= 2
         ORDER BY occurrence.occurrence_id",
    ] {
        projected.push(rows(&path, projection).await);
    }
    projected
}

#[tokio::test]
async fn incremental_and_one_shot_refreshes_have_identical_bytes_and_order() {
    let one_shot_profile = TempDir::new().unwrap();
    let incremental_profile = TempDir::new().unwrap();
    let one_shot = project_parity_fixture(&one_shot_profile, false).await;
    let incremental = project_parity_fixture(&incremental_profile, true).await;

    assert_eq!(one_shot[0].len(), 2, "both occurrences are projected");
    assert_eq!(
        one_shot[1].len(),
        1,
        "the supersession assertion is projected"
    );
    assert_eq!(one_shot, incremental);
}

#[tokio::test]
async fn cancelled_refreshes_and_stale_source_frontiers_reject_writes() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.cancelled");
    let first = persist_observation(&observation_store, &session_id, 0, "within frontier").await;
    let stale = persist_observation(&observation_store, &session_id, 1, "past frontier").await;
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let candidate = begin_candidate(&store, &session_id, 1).await;

    assert!(matches!(
        persist_batch(
            &store,
            &candidate,
            batch(
                &candidate,
                vec![occurrence(&session_id, &stale)],
                vec![],
                vec![],
            ),
        )
        .await,
        Err(SessionStoreError::FrozenWatermarkMismatch)
    ));

    store
        .cancel_session_refresh(SessionRefreshCancellationRequestV1::new(
            candidate.operation_id().clone(),
            session_id.clone(),
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
            tracedecay_domain::TemporalCoverageCountsV1 {
                visible: 0,
                hidden: 0,
                unknown: 0,
                redacted: 0,
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        persist_batch(
            &store,
            &candidate,
            batch(
                &candidate,
                vec![occurrence(&session_id, &first)],
                vec![],
                vec![],
            ),
        )
        .await,
        Err(SessionStoreError::InvalidRefreshState { .. })
    ));
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_occurrences").await,
        0
    );
}

#[tokio::test]
async fn restart_joins_the_running_refresh_and_activates_without_duplicates() {
    let tmp = TempDir::new().unwrap();
    let path;
    let session_id = session("session.temporal.restart");
    let (observation, operation_id) = {
        let runtime = profile_runtime(&tmp).await;
        path = runtime
            .database_path(HostAdmissionScope::Profile)
            .unwrap()
            .to_path_buf();
        let observation_store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let observation =
            persist_observation(&observation_store, &session_id, 0, "resume-activate").await;
        let store = runtime
            .session_temporal_store(HostAdmissionScope::Profile)
            .unwrap();
        let candidate = begin_candidate(&store, &session_id, 1).await;
        assert_eq!(
            persist_batch(
                &store,
                &candidate,
                batch(
                    &candidate,
                    vec![occurrence(&session_id, &observation)],
                    vec![],
                    vec![],
                ),
            )
            .await
            .unwrap()
            .disposition(),
            SessionTemporalProjectionBatchDispositionV1::Applied
        );
        (observation, candidate.operation_id().clone())
    };
    {
        let runtime = profile_runtime(&tmp).await;
        assert_eq!(
            runtime.database_path(HostAdmissionScope::Profile),
            Some(path.as_path())
        );
        let store = runtime
            .session_temporal_store(HostAdmissionScope::Profile)
            .unwrap();
        let candidate = begin_candidate(&store, &session_id, 1).await;
        assert_eq!(candidate.operation_id(), &operation_id);
        assert_eq!(
            persist_batch(
                &store,
                &candidate,
                batch(
                    &candidate,
                    vec![occurrence(&session_id, &observation)],
                    vec![],
                    vec![],
                ),
            )
            .await
            .unwrap()
            .disposition(),
            SessionTemporalProjectionBatchDispositionV1::ExactReplay
        );
        complete_candidate(&store, &candidate).await.unwrap();
    }
    assert_eq!(
        rows(
            &path,
            "SELECT generation || ':' || state || ':' ||
                    json_extract(frozen_watermarks_json, '$.active_generation')
             FROM session_temporal_generations
             WHERE session_id = 'session.temporal.restart'
             ORDER BY generation"
        )
        .await,
        vec!["1:superseded:1", "2:active:1"]
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts WHERE generation = 2"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn running_refresh_refuses_a_different_target_until_it_completes() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.begin-complete");
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let first = persist_observation(&observation_store, &session_id, 0, "complete").await;
    let candidate = begin_candidate(&store, &session_id, 1).await;
    assert!(matches!(
        store
            .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                SessionRefreshFrontierV1::new(2, 0).unwrap(),
            ))
            .await,
        Err(SessionStoreError::IdempotencyConflict {
            context: "session refresh busy"
        })
    ));
    assert_eq!(
        begin_candidate(&store, &session_id, 1).await.operation_id(),
        candidate.operation_id()
    );
    persist_batch(
        &store,
        &candidate,
        batch(
            &candidate,
            vec![occurrence(&session_id, &first)],
            vec![],
            vec![],
        ),
    )
    .await
    .unwrap();
    complete_candidate(&store, &candidate).await.unwrap();

    persist_observation(&observation_store, &session_id, 1, "appended").await;
    let next = begin_candidate(&store, &session_id, 2).await;
    assert_ne!(next.operation_id(), candidate.operation_id());
    assert_eq!(
        rows(
            &path,
            "SELECT generation || ':' || state || ':' ||
                    json_extract(frozen_watermarks_json, '$.active_generation')
             FROM session_temporal_generations
             WHERE session_id = 'session.temporal.begin-complete'
             ORDER BY generation"
        )
        .await,
        vec!["1:superseded:1", "2:active:1", "3:building:2"]
    );
}
