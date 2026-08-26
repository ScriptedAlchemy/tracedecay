use super::*;

#[tokio::test]
async fn activation_rejects_omitted_canonical_assertion_lineage() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.omitted-relations");
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
            AnchorProvenanceRelationV2::Supersedes,
            first.retrieval_anchor_id.clone(),
            None,
        )
        .await,
    );
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone(), second.clone()],
            vec![parent_message_copy(&second, &first)],
            vec![],
        ))
        .await
        .unwrap();

    assert!(
        store
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
            .is_err()
    );
}

#[tokio::test]
async fn activation_accepts_complete_canonical_graph_and_receipt_coverage() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let path = runtime.database_path(HostAdmissionScope::Profile).unwrap();
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.complete");
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
            AnchorProvenanceRelationV2::Supersedes,
            first.retrieval_anchor_id.clone(),
            None,
        )
        .await,
    );
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone(), second.clone()],
            vec![parent_message_copy(&second, &first)],
            vec![assertion(&second, &first)],
        ))
        .await
        .unwrap();
    store
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
        .unwrap();

    assert_eq!(
        rows(
            path,
            "SELECT generation || ':' || state
             FROM session_temporal_generations
             WHERE session_id = 'session.temporal.complete'
             ORDER BY generation"
        )
        .await,
        vec!["1:superseded", "2:active"]
    );
}

#[tokio::test]
async fn supersession_derivatives_resolve_transitive_current_state() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let path = runtime.database_path(HostAdmissionScope::Profile).unwrap();
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.transitive-supersession");
    let first = occurrence(
        &session_id,
        &persist_observation(&observation_store, &session_id, 0, "first").await,
    );
    let mut second = occurrence(
        &session_id,
        &persist_observation_with_lineage(
            &observation_store,
            &session_id,
            1,
            "second",
            AnchorProvenanceRelationV2::Supersedes,
            first.retrieval_anchor_id.clone(),
            Some(20),
        )
        .await,
    );
    second.valid_time = TemporalValidityV1::Known {
        valid_at: UtcMicros(20),
    };
    let mut third = occurrence(
        &session_id,
        &persist_observation_with_lineage(
            &observation_store,
            &session_id,
            2,
            "third",
            AnchorProvenanceRelationV2::Supersedes,
            second.retrieval_anchor_id.clone(),
            Some(30),
        )
        .await,
    );
    third.valid_time = TemporalValidityV1::Known {
        valid_at: UtcMicros(30),
    };
    let mut fourth = occurrence(
        &session_id,
        &persist_observation_with_lineage(
            &observation_store,
            &session_id,
            3,
            "fourth",
            AnchorProvenanceRelationV2::Supersedes,
            third.retrieval_anchor_id.clone(),
            Some(40),
        )
        .await,
    );
    fourth.valid_time = TemporalValidityV1::Known {
        valid_at: UtcMicros(40),
    };
    let assertions = vec![
        assertion(&second, &first),
        assertion(&third, &second),
        assertion(&fourth, &third),
    ];
    let terminal_assertion_id = assertions[2].assertion_id.as_str().to_owned();
    begin_candidate(&store, &session_id, 2, 4).await;

    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            4,
            vec![first.clone(), second.clone(), third.clone(), fourth],
            vec![],
            assertions.clone(),
        ))
        .await
        .unwrap();

    let mut expected_supersession = vec![
        format!(
            "{}:{}",
            assertions[0].assertion_id.as_str(),
            assertions[1].assertion_id.as_str()
        ),
        format!(
            "{}:{}",
            assertions[0].assertion_id.as_str(),
            assertions[2].assertion_id.as_str()
        ),
        format!(
            "{}:{}",
            assertions[1].assertion_id.as_str(),
            assertions[2].assertion_id.as_str()
        ),
    ];
    expected_supersession.sort_unstable();
    assert_eq!(
        rows(
            path,
            "SELECT superseded_assertion_id || ':' || superseding_assertion_id
             FROM session_assertion_supersession
             ORDER BY superseded_assertion_id, superseding_assertion_id"
        )
        .await,
        expected_supersession
    );

    let mut expected_current = [
        first.retrieval_anchor_id,
        second.retrieval_anchor_id,
        third.retrieval_anchor_id,
    ]
    .map(|anchor_id| format!("{}:{terminal_assertion_id}", anchor_id.as_str()))
    .to_vec();
    expected_current.sort_unstable();
    assert_eq!(
        rows(
            path,
            "SELECT entity_id || ':' || current_assertion_id
             FROM session_current_entities
             WHERE entity_kind = 'assertion_anchor'
             ORDER BY entity_id"
        )
        .await,
        expected_current
    );
}

#[tokio::test]
async fn failed_activation_leaves_the_prior_generation_active() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let path = runtime.database_path(HostAdmissionScope::Profile).unwrap();
    let session_id = session("session.temporal.activation-failure");
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    begin_candidate(&store, &session_id, 2, 0).await;

    assert!(
        store
            .activate_session_temporal_generation(
                SessionGenerationActivationRequestV1::new(
                    session_id.clone(),
                    generation(2),
                    snapshot(&session_id, 1, 0),
                    ExecutionControl::default(),
                )
                .unwrap(),
            )
            .await
            .is_err()
    );
    assert_eq!(
        rows(
            path,
            "SELECT generation || ':' || state
             FROM session_temporal_generations
             WHERE state = 'active'"
        )
        .await,
        vec!["1:active"]
    );
}

#[tokio::test]
async fn activation_is_pinned_to_the_snapshot_active_generation() {
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
    let session_id = session("session.temporal.pinning");
    let observation = persist_observation(&observation_store, &session_id, 0, "pinning").await;
    begin_candidate(&store, &session_id, 2, 1).await;
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

    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(&format!(
        "UPDATE session_temporal_generations
         SET state = 'superseded', completed_at = activated_at
         WHERE session_id = '{}' AND generation = 1;
         INSERT INTO session_temporal_generations (
             session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES ('{}', 3, 'building', '{{}}', unixepoch() * 1000000);
         UPDATE session_temporal_generations
         SET state = 'ready', ready_at = created_at
         WHERE session_id = '{}' AND generation = 3 AND state = 'building';
         UPDATE session_temporal_generations
         SET state = 'active', activated_at = ready_at
         WHERE session_id = '{}' AND generation = 3 AND state = 'ready';",
        session_id.as_str(),
        session_id.as_str(),
        session_id.as_str(),
        session_id.as_str()
    ))
    .unwrap();

    assert!(matches!(
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
            .await,
        Err(SessionStoreError::StaleGeneration { .. })
    ));
    assert_eq!(
        rows(
            &path,
            "SELECT generation || ':' || state
             FROM session_temporal_generations
             WHERE state = 'active'"
        )
        .await,
        vec!["3:active"]
    );
}

#[tokio::test]
async fn activation_rejects_incomplete_frontier_and_receipt_digest_mismatch() {
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
    let session_id = session("session.temporal.frontier-digest");
    let first = persist_observation(&observation_store, &session_id, 0, "one").await;
    let second = persist_observation(&observation_store, &session_id, 1, "two").await;
    let first = occurrence(&session_id, &first);
    let second = occurrence(&session_id, &second);
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone()],
            vec![],
            vec![],
        ))
        .await
        .unwrap();
    assert!(
        store
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
            .is_err()
    );
    assert_eq!(
        rows(
            &path,
            "SELECT generation || ':' || state
             FROM session_temporal_generations
             WHERE state = 'active'"
        )
        .await,
        vec!["1:active"]
    );

    begin_candidate(&store, &session_id, 3, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            3,
            2,
            vec![first.clone(), second.clone()],
            vec![parent_message_copy(&second, &first)],
            vec![],
        ))
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE session_occurrences
         SET snippet_text = 'tampered'
         WHERE session_id = ?1 AND generation = 3",
        rusqlite::params![session_id.as_str()],
    )
    .unwrap();
    assert!(
        store
            .activate_session_temporal_generation(
                SessionGenerationActivationRequestV1::new(
                    session_id.clone(),
                    generation(3),
                    snapshot(&session_id, 1, 2),
                    ExecutionControl::default(),
                )
                .unwrap(),
            )
            .await
            .is_err()
    );
    assert_eq!(
        rows(
            &path,
            "SELECT generation || ':' || state
             FROM session_temporal_generations
             WHERE state = 'active'"
        )
        .await,
        vec!["1:active"]
    );
}
