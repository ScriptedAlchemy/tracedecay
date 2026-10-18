//! Generation-bound session-derived spans/bursts: rebuild identity and restart.

use std::collections::BTreeSet;

use tempfile::TempDir;
use tracedecay_domain::{
    AnchorProvenanceRelation, CopyProofV1, LogicalCopyRecordV1, MessageOccurrenceRecordV1,
    RetrievalGrainV1, SessionId, TemporalModeV1,
};
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_session_temporal_store::SessionRefreshRecoveryV1;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_store::{
    ObservationProjectionStore, ObservationStore, SessionRetrievalStore,
    SessionTemporalRetrievalRequestV1, SessionTemporalSnapshotRequestV1,
};
use tracedecay_temporal_query::execution::ExecutionControl;

use crate::temporal_projection::{
    TemporalStore, assertion, batch, begin_candidate, complete_candidate, occurrence,
    persist_batch, persist_observation, persist_observation_with_lineage, profile_runtime,
    rows_runtime, scalar_runtime, session,
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
             FROM session_derived_evidence AS evidence
             WHERE session_id = '{session_id}'
               AND generation = (
                   SELECT MAX(version.generation)
                   FROM session_derived_evidence AS version
                   WHERE version.session_id = evidence.session_id
                     AND version.evidence_kind = evidence.evidence_kind
                     AND version.first_occurrence_id = evidence.first_occurrence_id
                     AND version.generation <= {generation}
               )
             ORDER BY evidence_kind, evidence_id"
        ),
    )
    .await
}

async fn project_and_activate<O>(
    runtime: &HostAdmissionTestRuntimeV1,
    observation_store: &O,
    temporal_store: &TemporalStore<'_>,
    session_name: &str,
) -> (SessionId, Vec<String>, Vec<MessageOccurrenceRecordV1>)
where
    O: ObservationStore + ObservationProjectionStore,
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
            AnchorProvenanceRelation::Supersedes,
            first.retrieval_anchor_id.clone(),
            None,
        )
        .await,
    );
    // A reply link is conversation threading, not a copy: the retained
    // derivation only emits a logical copy for a re-emission of the same
    // logical message, and `CopiedFrom` lineage stays explicit
    // (`derive_retained_projection_relations`). So the copy edge under test is
    // a third occurrence carrying real `CopiedFrom` anchor provenance and the
    // matching explicit-assertion proof.
    let copied = occurrence(
        &session_id,
        &persist_observation_with_lineage(
            observation_store,
            &session_id,
            2,
            "derived-alpha pipeline copied",
            AnchorProvenanceRelation::CopiedFrom,
            first.retrieval_anchor_id.clone(),
            None,
        )
        .await,
    );
    let copy = LogicalCopyRecordV1 {
        occurrence_id: copied.occurrence_id.clone(),
        copied_from_occurrence_id: first.occurrence_id.clone(),
        proof: CopyProofV1::ExplicitAnchorAssertion {
            source_occurrence_id: first.occurrence_id.clone(),
            assertion_anchor_id: first.retrieval_anchor_id.clone(),
        },
        knowledge_at: copied.knowledge_at,
        valid_time: copied.valid_time,
    };
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
    let candidate = begin_candidate(temporal_store, &session_id, source_frontier).await;
    persist_batch(
        temporal_store,
        &candidate,
        batch(
            &candidate,
            vec![first.clone(), second.clone(), copied.clone()],
            vec![copy],
            vec![assertion(&second, &first)],
        ),
    )
    .await
    .unwrap();
    complete_candidate(temporal_store, &candidate)
        .await
        .unwrap();
    let derived = derived_identity_rows(
        runtime,
        session_id.as_str(),
        candidate.candidate_generation().value(),
    )
    .await;
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
    (session_id, derived, vec![first, second, copied])
}

/// Projects the identity fixture into the refresh candidate of `runtime`,
/// either as one batch or as two checkpointed batches, and returns the
/// derived identities that candidate reads before activation.
async fn project_identity_fixture(
    runtime: &HostAdmissionTestRuntimeV1,
    session_id: &SessionId,
    incremental: bool,
) -> (Vec<String>, SessionRefreshRecoveryV1) {
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let first = occurrence(
        session_id,
        &persist_observation(&observation_store, session_id, 0, "derived-alpha pipeline").await,
    );
    let second = occurrence(
        session_id,
        &persist_observation_with_lineage(
            &observation_store,
            session_id,
            1,
            "derived-beta pipeline",
            AnchorProvenanceRelation::Supersedes,
            first.retrieval_anchor_id.clone(),
            None,
        )
        .await,
    );
    let assertion = assertion(&second, &first);
    let candidate = begin_candidate(&store, session_id, 2).await;
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
            batch(&candidate, vec![second], vec![], vec![assertion])
                .with_checkpoint(1, 2, 2)
                .unwrap(),
        )
        .await
        .unwrap();
    } else {
        persist_batch(
            &store,
            &candidate,
            batch(&candidate, vec![first, second], vec![], vec![assertion]),
        )
        .await
        .unwrap();
    }
    (
        derived_identity_rows(
            runtime,
            session_id.as_str(),
            candidate.candidate_generation().value(),
        )
        .await,
        candidate,
    )
}

#[tokio::test]
async fn rebuilds_are_identity_stable_across_oneshot_incremental_and_restart() {
    let session_id = session("session.temporal.derived.identity");
    let oneshot_profile = TempDir::new().unwrap();
    let oneshot = {
        let runtime = profile_runtime(&oneshot_profile).await;
        project_identity_fixture(&runtime, &session_id, false)
            .await
            .0
    };
    let kinds = oneshot
        .iter()
        .filter_map(|row| row.split('|').next())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        kinds,
        BTreeSet::from(["burst", "span"]),
        "projection must materialize both derived evidence kinds"
    );

    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let database_identity = runtime
        .session_database_identity_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let (incremental, candidate) = project_identity_fixture(&runtime, &session_id, true).await;
    assert_eq!(
        oneshot, incremental,
        "one-shot and incremental refreshes must mint identical derived identities"
    );
    complete_candidate(
        &runtime
            .session_temporal_store(HostAdmissionScope::Profile)
            .unwrap(),
        &candidate,
    )
    .await
    .unwrap();
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
    assert_eq!(snapshot.watermarks().active_generation().value(), 2);
    let restarted = derived_identity_rows(&reopened, session_id.as_str(), 2).await;
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
    let (session_id, _, expected_occurrences) = project_and_activate(
        &runtime,
        &observation_store,
        &store,
        "session.temporal.derived.page",
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

    let mut expected_occurrence_ids = expected_occurrences
        .iter()
        .map(|occurrence| occurrence.occurrence_id.clone())
        .collect::<Vec<_>>();
    expected_occurrence_ids.sort_unstable();
    assert_eq!(
        page.occurrences()
            .iter()
            .map(|occurrence| occurrence.occurrence_id.clone())
            .collect::<Vec<_>>(),
        expected_occurrence_ids
    );
    assert_eq!(page.copies().len(), 1);
    assert_eq!(
        page.copies()[0].occurrence_id,
        expected_occurrences[2].occurrence_id
    );
    assert_eq!(
        page.copies()[0].copied_from_occurrence_id,
        expected_occurrences[0].occurrence_id
    );
    assert_ne!(
        page.copies()[0].occurrence_id,
        expected_occurrences[1].occurrence_id,
        "a reply link alone must not fabricate logical-copy evidence"
    );
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
