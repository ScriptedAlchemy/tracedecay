//! Generation-bound session-derived spans/bursts: rebuild identity and restart.

use std::collections::BTreeSet;

use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::{
    AnchorProvenanceRelationV2, MessageOccurrenceRecordV1, RetrievalGrainV1, SessionId,
    TemporalModeV1,
};
use tracedecay_store::{
    ObservationProjectionStore, ObservationStore, SessionGenerationActivationRequestV1,
    SessionRetrievalStore, SessionTemporalProjectionStore, SessionTemporalRetrievalRequestV1,
    SessionTemporalSnapshotRequestV1,
};
use tracedecay_temporal_query::ports::ExecutionControl;
use tracedecay_usecases::host_admission::HostAdmissionScope;

use crate::temporal_projection::{
    assertion, batch, begin_candidate, generation, occurrence, parent_message_copy,
    persist_observation, persist_observation_with_lineage, profile_runtime, rows_runtime,
    scalar_runtime, session, snapshot,
};

async fn derived_identity_rows(
    runtime: &HostAdmissionTestRuntimeV1,
    session_id: &str,
    generation: u64,
) -> Vec<String> {
    rows_runtime(
        runtime,
        &format!(
            "SELECT evidence_kind || '|' || evidence_id || '|' || member_digest || '|' ||
                    configuration_digest || '|' || COALESCE(retrieval_anchor_id, '')
             FROM session_derived_evidence
             WHERE session_id = '{session_id}' AND generation = {generation}
             ORDER BY evidence_kind, evidence_id"
        ),
    )
    .await
}

async fn project_and_activate<O, T>(
    runtime: &HostAdmissionTestRuntimeV1,
    observation_store: &O,
    temporal_store: &T,
    session_name: &str,
    candidate_generation: u64,
) -> (
    SessionId,
    Vec<String>,
    MessageOccurrenceRecordV1,
    MessageOccurrenceRecordV1,
)
where
    O: ObservationStore + ObservationProjectionStore,
    T: SessionTemporalProjectionStore,
{
    let session_id = session(session_name);
    let first = occurrence(
        &session_id,
        &persist_observation(observation_store, &session_id, 0, "derived-alpha pipeline").await,
    );
    let second = occurrence(
        &session_id,
        &persist_observation_with_lineage(
            observation_store,
            &session_id,
            1,
            "derived-beta pipeline",
            AnchorProvenanceRelationV2::Supersedes,
            first.retrieval_anchor_id.clone(),
            None,
        )
        .await,
    );
    // Observation sequences are DB-global; pin the frontier to the current max
    // so later sessions in the same DB are not rejected as watermark mismatches.
    let source_frontier = u64::try_from(
        scalar_runtime(
            runtime,
            "SELECT COALESCE(MAX(sequence), 0) FROM observations",
        )
        .await,
    )
    .expect("observation frontier fits u64");
    assert!(
        source_frontier > 0,
        "projected sessions must have durable observation sequences"
    );
    begin_candidate(
        temporal_store,
        &session_id,
        candidate_generation,
        source_frontier,
    )
    .await;
    temporal_store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            candidate_generation,
            source_frontier,
            vec![first.clone(), second.clone()],
            vec![parent_message_copy(&second, &first)],
            vec![assertion(&second, &first)],
        ))
        .await
        .unwrap();
    temporal_store
        .activate_session_temporal_generation(
            SessionGenerationActivationRequestV1::new(
                session_id.clone(),
                generation(candidate_generation),
                snapshot(
                    &session_id,
                    candidate_generation.saturating_sub(1).max(1),
                    source_frontier,
                ),
                ExecutionControl::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let derived = derived_identity_rows(runtime, session_id.as_str(), candidate_generation).await;
    assert!(
        !derived.is_empty(),
        "expected generation-bound span/burst rows after activation"
    );
    let kinds = derived
        .iter()
        .filter_map(|row| row.split('|').next())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        kinds,
        BTreeSet::from(["burst", "span"]),
        "projection must materialize both derived evidence kinds"
    );
    (session_id, derived, first, second)
}

#[tokio::test]
async fn rebuilds_are_identity_stable_across_oneshot_incremental_and_restart() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let database_identity = runtime
        .session_database_identity_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.derived.identity");
    let oneshot;
    {
        let observation_store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let store = runtime
            .session_temporal_store(HostAdmissionScope::Profile)
            .unwrap();
        let first = occurrence(
            &session_id,
            &persist_observation(&observation_store, &session_id, 0, "derived-alpha pipeline")
                .await,
        );
        let second = occurrence(
            &session_id,
            &persist_observation_with_lineage(
                &observation_store,
                &session_id,
                1,
                "derived-beta pipeline",
                AnchorProvenanceRelationV2::Supersedes,
                first.retrieval_anchor_id.clone(),
                None,
            )
            .await,
        );
        let edge = parent_message_copy(&second, &first);
        let assertion = assertion(&second, &first);

        // One-shot rebuild into generation 2.
        begin_candidate(&store, &session_id, 2, 2).await;
        // Parallel building generation for incremental parity.
        begin_candidate(&store, &session_id, 3, 2).await;
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                2,
                2,
                vec![first.clone(), second.clone()],
                vec![edge.clone()],
                vec![assertion.clone()],
            ))
            .await
            .unwrap();
        // Incremental rebuild into generation 3.
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                3,
                2,
                vec![first.clone()],
                vec![],
                vec![],
            ))
            .await
            .unwrap();
        store
            .persist_session_temporal_projection_batch(
                batch(
                    &session_id,
                    3,
                    2,
                    vec![second.clone()],
                    vec![edge],
                    vec![assertion],
                )
                .with_checkpoint(1, 2, 2)
                .unwrap(),
            )
            .await
            .unwrap();

        oneshot = derived_identity_rows(&runtime, session_id.as_str(), 2).await;
        let incremental = derived_identity_rows(&runtime, session_id.as_str(), 3).await;
        assert_eq!(
            oneshot, incremental,
            "one-shot and incremental rebuilds must mint identical derived identities"
        );
        assert!(!oneshot.is_empty());

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
            .unwrap();
    }
    drop(runtime);

    let reopened = profile_runtime(&tmp).await;
    assert_eq!(
        reopened
            .session_database_identity_for_test(HostAdmissionScope::Profile)
            .unwrap(),
        database_identity
    );
    let store = reopened
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let snapshot = store
        .freeze_session_temporal_snapshot(SessionTemporalSnapshotRequestV1::new(session_id.clone()))
        .await
        .unwrap();
    assert_eq!(snapshot.watermarks().active_generation().value(), 3);
    let restarted = derived_identity_rows(&reopened, session_id.as_str(), 3).await;
    assert_eq!(oneshot, restarted);
}

#[tokio::test]
async fn frozen_temporal_page_returns_projected_occurrences_and_lineage() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let (session_id, _, first, second) = project_and_activate(
        &runtime,
        &observation_store,
        &store,
        "session.temporal.derived.page",
        2,
    )
    .await;
    let snapshot = store
        .freeze_session_temporal_snapshot(SessionTemporalSnapshotRequestV1::new(session_id.clone()))
        .await
        .unwrap();
    let page = store
        .retrieve_session_temporal_page(
            SessionTemporalRetrievalRequestV1::new(
                session_id.clone(),
                TemporalModeV1::Evolution,
                RetrievalGrainV1::Occurrence,
                snapshot,
                8,
                None,
                ExecutionControl::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let mut expected_occurrence_ids = vec![first.occurrence_id, second.occurrence_id];
    expected_occurrence_ids.sort_unstable();
    assert_eq!(
        page.occurrences()
            .iter()
            .map(|occurrence| occurrence.occurrence_id.clone())
            .collect::<Vec<_>>(),
        expected_occurrence_ids
    );
    assert_eq!(page.copies().len(), 1);
    assert_eq!(page.assertions().len(), 1);
    assert!(page.next_after_occurrence_id().is_none());

    let expected_copies = page.copies().to_vec();
    drop(observation_store);
    drop(runtime);

    let reopened = profile_runtime(&tmp).await;
    let store = reopened
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let snapshot = store
        .freeze_session_temporal_snapshot(SessionTemporalSnapshotRequestV1::new(session_id.clone()))
        .await
        .unwrap();
    let restarted_page = store
        .retrieve_session_temporal_page(
            SessionTemporalRetrievalRequestV1::new(
                session_id,
                TemporalModeV1::Evolution,
                RetrievalGrainV1::Occurrence,
                snapshot,
                8,
                None,
                ExecutionControl::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(restarted_page.copies(), expected_copies.as_slice());
}
