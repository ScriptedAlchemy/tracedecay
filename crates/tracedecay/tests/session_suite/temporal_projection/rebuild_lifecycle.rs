use super::*;

#[tokio::test]
async fn first_session_rebuild_bootstraps_active_generation_under_writer_authority() {
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

    assert_eq!(
        begin_candidate(&store, &session_id, 2, 0).await,
        SessionGenerationRebuildDispositionV1::Started
    );
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

/// Projects the parity fixture into candidate generation 2 of a fresh profile,
/// either as one batch or as two checkpointed batches, and returns the
/// canonical rows that generation reads.
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
    begin_candidate(&store, &session_id, 2, 2).await;
    if incremental {
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                2,
                2,
                vec![first],
                vec![],
                vec![],
            ))
            .await
            .unwrap();
        store
            .persist_session_temporal_projection_batch(
                batch(&session_id, 2, 2, vec![second], vec![edge], vec![assertion])
                    .with_checkpoint(1, 2, 2)
                    .unwrap(),
            )
            .await
            .unwrap();
    } else {
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                2,
                2,
                vec![first, second],
                vec![edge],
                vec![assertion],
            ))
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
async fn incremental_and_one_shot_rebuilds_have_identical_bytes_and_order() {
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
async fn cancelled_candidates_and_stale_source_frontiers_reject_writes() {
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
    begin_candidate(&store, &session_id, 2, 1).await;

    assert!(matches!(
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                2,
                1,
                vec![occurrence(&session_id, &stale)],
                vec![],
                vec![],
            ))
            .await,
        Err(SessionStoreError::FrozenWatermarkMismatch)
    ));

    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE session_temporal_generations
         SET state = 'cancelled', completed_at = created_at
         WHERE session_id = ?1 AND generation = 2",
            rusqlite::params![session_id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                2,
                1,
                vec![occurrence(&session_id, &first)],
                vec![],
                vec![],
            ))
            .await,
        Err(SessionStoreError::Storage { .. })
    ));
}

#[tokio::test]
async fn restart_resumes_one_existing_candidate_instead_of_duplicating_it() {
    let tmp = TempDir::new().unwrap();
    let path;
    let session_id = session("session.temporal.restart");
    let request = SessionGenerationRebuildRequestV1::new(
        session_id.clone(),
        generation(2),
        snapshot(&session_id, 1, 0),
    )
    .unwrap();
    {
        let runtime = profile_runtime(&tmp).await;
        path = runtime
            .database_path(HostAdmissionScope::Profile)
            .unwrap()
            .to_path_buf();
        let store = runtime
            .session_temporal_store(HostAdmissionScope::Profile)
            .unwrap();
        assert_eq!(
            store
                .begin_session_generation_rebuild(request.clone())
                .await
                .unwrap()
                .disposition(),
            SessionGenerationRebuildDispositionV1::Started
        );
        store
            .persist_session_temporal_projection_batch(
                batch(&session_id, 2, 0, vec![], vec![], vec![])
                    .with_checkpoint(0, 0, 0)
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    {
        let runtime = profile_runtime(&tmp).await;
        assert_eq!(
            runtime.database_path(HostAdmissionScope::Profile),
            Some(path.as_path())
        );
        let store = runtime
            .session_temporal_store(HostAdmissionScope::Profile)
            .unwrap();
        assert_eq!(
            store
                .begin_session_generation_rebuild(request)
                .await
                .unwrap()
                .disposition(),
            SessionGenerationRebuildDispositionV1::Resumed
        );
        assert_eq!(
            store
                .persist_session_temporal_projection_batch(
                    batch(&session_id, 2, 0, vec![], vec![], vec![])
                        .with_checkpoint(0, 0, 0)
                        .unwrap(),
                )
                .await
                .unwrap()
                .disposition(),
            SessionTemporalProjectionBatchDispositionV1::ExactReplay
        );
    }
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_generations WHERE generation = 2"
        )
        .await,
        1
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
async fn begin_rejects_watermark_mismatch_and_stale_pin_after_activation() {
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
    let candidate = SessionGenerationRebuildRequestV1::new(
        session_id.clone(),
        generation(2),
        snapshot(&session_id, 1, 1),
    )
    .unwrap();
    assert_eq!(
        store
            .begin_session_generation_rebuild(candidate.clone())
            .await
            .unwrap()
            .disposition(),
        SessionGenerationRebuildDispositionV1::Started
    );
    assert!(matches!(
        store
            .begin_session_generation_rebuild(
                SessionGenerationRebuildRequestV1::new(
                    session_id.clone(),
                    generation(2),
                    snapshot(&session_id, 1, 0),
                )
                .unwrap(),
            )
            .await,
        Err(SessionStoreError::FrozenWatermarkMismatch)
    ));
    assert_eq!(
        store
            .begin_session_generation_rebuild(candidate)
            .await
            .unwrap()
            .disposition(),
        SessionGenerationRebuildDispositionV1::Resumed
    );
    let observation = persist_observation(&observation_store, &session_id, 0, "complete").await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            1,
            vec![occurrence(&session_id, &observation)],
            vec![],
            vec![],
        ))
        .await
        .unwrap();
    store
        .activate_session_temporal_generation(
            SessionGenerationActivationRequestV1::new(
                session_id.clone(),
                generation(2),
                snapshot(&session_id, 1, 1),
                ExecutionControl::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    // Frozen watermarks are immutable; Complete uses the rebuild-time snapshot
    // while the live active pin has moved, so begin fails closed as stale.
    assert!(matches!(
        store
            .begin_session_generation_rebuild(
                SessionGenerationRebuildRequestV1::new(
                    session_id.clone(),
                    generation(2),
                    snapshot(&session_id, 1, 1),
                )
                .unwrap(),
            )
            .await,
        Err(SessionStoreError::StaleGeneration { .. })
    ));
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
        vec!["1:superseded:1", "2:active:1"]
    );
}

#[tokio::test]
async fn interrupted_rebuild_resumes_building_then_activates_ready_to_active() {
    let tmp = TempDir::new().unwrap();
    let path;
    let session_id = session("session.temporal.interrupted-activate");
    let request = SessionGenerationRebuildRequestV1::new(
        session_id.clone(),
        generation(2),
        snapshot(&session_id, 1, 1),
    )
    .unwrap();
    let observation = {
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
        assert_eq!(
            store
                .begin_session_generation_rebuild(request.clone())
                .await
                .unwrap()
                .disposition(),
            SessionGenerationRebuildDispositionV1::Started
        );
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                2,
                1,
                vec![occurrence(&session_id, &observation)],
                vec![],
                vec![],
            ))
            .await
            .unwrap();
        observation
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
        assert_eq!(
            store
                .begin_session_generation_rebuild(request)
                .await
                .unwrap()
                .disposition(),
            SessionGenerationRebuildDispositionV1::Resumed
        );
        assert_eq!(
            store
                .persist_session_temporal_projection_batch(batch(
                    &session_id,
                    2,
                    1,
                    vec![occurrence(&session_id, &observation)],
                    vec![],
                    vec![],
                ))
                .await
                .unwrap()
                .disposition(),
            SessionTemporalProjectionBatchDispositionV1::ExactReplay
        );
        store
            .activate_session_temporal_generation(
                SessionGenerationActivationRequestV1::new(
                    session_id.clone(),
                    generation(2),
                    snapshot(&session_id, 1, 1),
                    ExecutionControl::default(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        rows(
            &path,
            "SELECT generation || ':' || state || ':' ||
                    json_extract(frozen_watermarks_json, '$.active_generation')
             FROM session_temporal_generations
             WHERE session_id = 'session.temporal.interrupted-activate'
             ORDER BY generation"
        )
        .await,
        vec!["1:superseded:1", "2:active:1"]
    );
}
