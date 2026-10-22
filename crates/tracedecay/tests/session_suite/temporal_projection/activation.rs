use tracedecay_session_runtime::session_temporal_refresh_scheduler::SessionTemporalRefreshSchedulerRegistry;

use super::*;

const OMITTED_LINEAGE_REFUSAL: &str = "session-temporal storage operation activate session temporal generation failed: candidate omits canonical typed assertion lineage through the frozen frontier";

#[tokio::test]
async fn refused_activation_retries_and_a_corrected_refresh_activates() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.omitted-relations");
    let (first, second) = persist_superseding_pair(&observation_store, &session_id).await;
    let refused = begin_candidate(&store, &session_id, 2).await;
    persist_batch(
        &store,
        &refused,
        batch(
            &refused,
            vec![first.clone(), second.clone()],
            vec![],
            vec![],
        ),
    )
    .await
    .unwrap();

    let error = complete_candidate(&store, &refused)
        .await
        .expect_err("activation must refuse a graph missing its supersession lineage");
    assert_eq!(error.to_string(), OMITTED_LINEAGE_REFUSAL);
    // The refused attempt already applied the candidate's relation graph; a
    // retry must reach the same validation instead of a receipt conflict.
    let retried = complete_candidate(&store, &refused)
        .await
        .expect_err("a retried incomplete candidate stays refused");
    assert_eq!(retried.to_string(), OMITTED_LINEAGE_REFUSAL);

    fail_candidate(&store, &refused).await;
    let corrected = begin_candidate(&store, &session_id, 2).await;
    assert_ne!(corrected.operation_id(), refused.operation_id());
    persist_batch(
        &store,
        &corrected,
        batch(
            &corrected,
            vec![first.clone(), second.clone()],
            vec![],
            vec![assertion(&second, &first)],
        ),
    )
    .await
    .unwrap();
    complete_candidate(&store, &corrected).await.unwrap();
    assert_eq!(
        rows_runtime(
            &runtime,
            "SELECT generation || ':' || state
             FROM session_temporal_generations
             WHERE session_id = 'session.temporal.omitted-relations' AND state = 'active'"
        )
        .await,
        vec![format!(
            "{}:active",
            corrected.candidate_generation().value()
        )]
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
    let (first, second) = persist_superseding_pair(&observation_store, &session_id).await;
    let candidate = begin_candidate(&store, &session_id, 2).await;
    persist_batch(
        &store,
        &candidate,
        batch(
            &candidate,
            vec![first.clone(), second.clone()],
            vec![],
            vec![assertion(&second, &first)],
        ),
    )
    .await
    .unwrap();
    complete_candidate(&store, &candidate).await.unwrap();

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
            AnchorProvenanceRelation::Supersedes,
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
            AnchorProvenanceRelation::Supersedes,
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
            AnchorProvenanceRelation::Supersedes,
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
    let candidate = begin_candidate(&store, &session_id, 4).await;

    persist_batch(
        &store,
        &candidate,
        batch(
            &candidate,
            vec![first.clone(), second.clone(), third.clone(), fourth],
            vec![],
            assertions.clone(),
        ),
    )
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
async fn activation_is_pinned_to_the_generation_the_refresh_extends() {
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
    let candidate = begin_candidate(&store, &session_id, 1).await;
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
        complete_candidate(&store, &candidate).await,
        Err(SessionStoreError::InvalidStateTransition {
            context: "refresh candidate base generation is no longer active"
        })
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
async fn reset_requested_during_a_refresh_supersedes_its_candidate() {
    let tmp = TempDir::new().unwrap();
    let (runtime, path, stale) = frontier_digest_fixture(&tmp, true).await;
    let session_id = stale.session_id().clone();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO observation_projection_dispositions
                 (projector_version, observation_id, receipt_id, reason)
             SELECT ?1, observation_id, receipt_id, ?2
             FROM observations
             WHERE observation_id = (
                 SELECT observation_id FROM session_temporal_observation_effects
                 WHERE session_id = ?3 AND observation_sequence = 2
             )",
            rusqlite::params![
                tracedecay_store::SESSION_MESSAGE_PROJECTOR_VERSION,
                tracedecay_store::ProjectionSkipReason::SourceRecordRetired.as_str(),
                session_id.as_str(),
            ],
        )
        .unwrap();
    let requested = rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO session_temporal_resets (session_id, requested_at)
             SELECT effect.session_id, unixepoch() * 1000000
             FROM session_temporal_observation_effects AS effect
             JOIN observation_projection_dispositions AS retired
               ON retired.observation_id = effect.observation_id
             WHERE effect.output_count > 0",
            [],
        )
        .unwrap();
    assert_eq!(requested, 1, "the retired observation requests one reset");

    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    registry
        .ensure_profile(
            path.clone(),
            runtime
                .registered_database_arc(HostAdmissionScope::Profile)
                .unwrap(),
        )
        .await;
    registry.wait_profile_idle(&path).await;
    registry.shutdown().await;

    assert_eq!(
        rows(
            &path,
            "SELECT state FROM session_refresh_operations ORDER BY created_at, operation_id"
        )
        .await,
        vec!["failed", "complete"]
    );
    let replacement_generation = rows(
        &path,
        "SELECT binding.generation || ':active'
         FROM session_refresh_bindings AS binding
         JOIN session_refresh_operations AS operation
           ON operation.session_id = binding.session_id
          AND operation.operation_id = binding.operation_id
         WHERE operation.state = 'complete'",
    )
    .await;
    assert_eq!(replacement_generation.len(), 1);
    assert_ne!(replacement_generation, vec!["1:active"]);
    assert_eq!(active_generations(&path).await, replacement_generation);
    assert_eq!(
        rows(&path, "SELECT CAST(MAX(observation_sequence) AS TEXT) FROM session_temporal_observation_effects").await,
        vec!["2"]
    );
    assert_eq!(
        rows(
            &path,
            "SELECT CAST(reset_generation IS NOT NULL AND requested_at IS NULL AS TEXT)
             FROM session_temporal_resets"
        )
        .await,
        vec!["1"]
    );
    assert_eq!(
        rows(
            &path,
            "SELECT occurrence.message_id FROM session_occurrences AS occurrence
             JOIN session_temporal_generations AS generation
               ON generation.session_id = occurrence.session_id
              AND generation.generation = occurrence.generation
             WHERE generation.state = 'active' ORDER BY occurrence.message_id"
        )
        .await,
        vec![format!("message.temporal.{}.0", session_id.as_str())]
    );
}

#[tokio::test]
async fn ambiguous_candidate_failure_does_not_repeat_for_an_unchanged_frontier() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let session_id = session("session.temporal.ambiguous-discovery");
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let duplicate = "message.temporal.duplicate";
    let mut occurrences = Vec::new();
    for ordinal in 0..2 {
        let observation = persist_custom_observation(
            &observation_store,
            observation_with_message_ids(&session_id, ordinal, "duplicate", duplicate, None),
        )
        .await;
        occurrences.push(occurrence_with_message_id(
            &session_id,
            &observation,
            duplicate,
        ));
    }
    let candidate = begin_candidate(&store, &session_id, 2).await;
    persist_batch(
        &store,
        &candidate,
        batch(&candidate, occurrences, vec![], vec![]),
    )
    .await
    .unwrap();
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let wake = registry
        .ensure_profile(
            path.clone(),
            runtime
                .registered_database_arc(HostAdmissionScope::Profile)
                .unwrap(),
        )
        .await;
    registry.wait_profile_idle(&path).await;
    assert_eq!(
        rows(&path, "SELECT state FROM session_refresh_operations").await,
        vec!["failed"]
    );

    wake.wake();
    registry.wait_profile_idle(&path).await;
    registry.shutdown().await;
    assert_eq!(
        rows(&path, "SELECT state FROM session_refresh_operations").await,
        vec!["failed"]
    );
    assert_eq!(active_generations(&path).await, vec!["1:active"]);
}

/// Persists observations `one` and `two` in a fresh profile and projects the
/// refresh candidate through the frozen frontier of 2: both occurrences when
/// `complete`, otherwise only `one`.
async fn frontier_digest_fixture(
    tmp: &TempDir,
    complete: bool,
) -> (
    HostAdmissionTestRuntimeV1,
    std::path::PathBuf,
    SessionRefreshRecoveryV1,
) {
    let runtime = profile_runtime(tmp).await;
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
    let candidate = begin_candidate(&store, &session_id, 2).await;
    let occurrences = if complete {
        vec![first, second]
    } else {
        vec![first]
    };
    persist_batch(
        &store,
        &candidate,
        batch(&candidate, occurrences, vec![], vec![]),
    )
    .await
    .unwrap();
    drop(observation_store);
    (runtime, path, candidate)
}

async fn complete_frontier_digest_candidate(
    runtime: &HostAdmissionTestRuntimeV1,
    candidate: &SessionRefreshRecoveryV1,
) -> Result<(), SessionStoreError> {
    complete_candidate(
        &runtime
            .session_temporal_store(HostAdmissionScope::Profile)
            .unwrap(),
        candidate,
    )
    .await
}

async fn active_generations(path: &std::path::Path) -> Vec<String> {
    rows(
        path,
        "SELECT generation || ':' || state
         FROM session_temporal_generations
         WHERE state = 'active'",
    )
    .await
}

// A session holds one open candidate, so the incomplete and the tampered
// candidate each run in their own profile.
#[tokio::test]
async fn activation_rejects_incomplete_frontier_and_receipt_digest_mismatch() {
    let incomplete_profile = TempDir::new().unwrap();
    let (runtime, path, candidate) = frontier_digest_fixture(&incomplete_profile, false).await;
    assert_eq!(
        complete_frontier_digest_candidate(&runtime, &candidate)
            .await
            .expect_err("activation must refuse a candidate missing frontier outputs")
            .to_string(),
        "session-temporal storage operation activate session temporal generation failed: candidate occurrence coverage does not equal the frozen source frontier"
    );
    assert_eq!(active_generations(&path).await, vec!["1:active"]);

    let tampered_profile = TempDir::new().unwrap();
    let (runtime, path, candidate) = frontier_digest_fixture(&tampered_profile, true).await;
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE session_occurrences
             SET index_text = 'tampered'
             WHERE session_id = ?1 AND generation = ?2",
            rusqlite::params![
                candidate.session_id().as_str(),
                i64::try_from(candidate.candidate_generation().value()).unwrap()
            ],
        )
        .unwrap();
    let error = complete_frontier_digest_candidate(&runtime, &candidate)
        .await
        .expect_err("activation must refuse rows that no longer match their receipt");
    assert_eq!(
        error.to_string(),
        "session-temporal storage operation activate session temporal generation failed: candidate projection rows do not match the immutable final receipt"
    );
    assert_eq!(active_generations(&path).await, vec!["1:active"]);
}
