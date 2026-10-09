use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{
    AnchorProvenanceRelation, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CopyProofV1, DurableObservationV1, LogicalCopyRecordV1, MessageOccurrenceIdV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId,
    ProjectionOutputOrdinalV1, ProviderId, RetentionClass, RetrievalAnchorId,
    RetrievalAnchorRecord, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, TemporalAssertionKindV1, TemporalValidityV1,
    UtcMicros, derive_exact_observation_anchor_id,
};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjection, ObservationProjectionStore, ObservationStore,
    ObservationWrite, ProjectionSkipReason, ProjectionStoreError,
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCompletionRequestV1,
    SessionRefreshFrontierV1, SessionRefreshProgressV1, SessionRefreshStore,
    SessionRefreshTerminalStateV1, SessionRetrievalStore, SessionStoreError, SessionStoreResult,
    SessionTemporalProjectionBatchV1,
};
use tracedecay_temporal_query::execution::ExecutionControl;

use super::super::refresh::SessionRefreshRestartStateV1;
use super::materialize::*;
use super::record_canonical_observation_effect;
use crate::handle::SessionTemporalRegisteredDb;
use crate::test_support::QueryCountingConnection;
use crate::{SessionTemporalAccess, SessionTemporalRefreshDiscoveryCursor, SessionTemporalStore};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::tests::harness::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, SessionTemporalFixtureCountV1,
    open_registered_test_database_fixture,
};
use tracedecay_lcm::retrieval_content::derived_text_for_index;
use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, TestConnection, params};
use tracedecay_sessions::runtime::user_sessions_db_path;

fn fixture_session(value: &str) -> SessionId {
    SessionId::new(value).unwrap()
}

fn temporal_store(
    runtime: &HostAdmissionTestRuntimeV1,
) -> SessionTemporalStore<'_, RegisteredGlobalDb> {
    SessionTemporalStore::new(
        runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile session-temporal store"),
    )
}

fn fixture_receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.projector-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn fixture_observation(
    session_id: &SessionId,
    ordinal: u64,
    lineage: Option<(AnchorProvenanceRelation, RetrievalAnchorId)>,
    include_parent: bool,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    let provider = ProviderId::new(format!("projector-test-{ordinal}")).unwrap();
    let record_id = ObservationId::new(format!("record.projector.{ordinal}")).unwrap();
    let mut relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_thread_id(ObservationId::new("thread.projector").unwrap())
        .with_turn_id(ObservationId::new("turn.projector").unwrap())
        .with_message_id(ObservationId::new(format!("message.projector.{ordinal}")).unwrap())
        .with_agent_id(ObservationId::new("agent.projector").unwrap());
    if include_parent && ordinal > 0 {
        relations = relations.with_parent_message_id(
            ObservationId::new(format!("message.projector.{}", ordinal - 1)).unwrap(),
        );
    }
    fixture_observation_from_facts(
        session_id,
        ordinal,
        provider,
        record_id,
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": format!("projector {ordinal}")}),
            model: Some("model.projector".to_owned()),
            timestamp: Some(1_750_000_000 + i64::try_from(ordinal).unwrap()),
        }],
        lineage,
    )
}

fn fixture_observation_from_facts(
    session_id: &SessionId,
    ordinal: u64,
    provider: ProviderId,
    record_id: ObservationId,
    relations: CanonicalObservationRelationsV1,
    facts: Vec<CanonicalObservationFactV1>,
    lineage: Option<(AnchorProvenanceRelation, RetrievalAnchorId)>,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        relations,
        facts,
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let receipt_id = format!("receipt.projector.{}", record_id.as_str());
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        identity,
        fixture_receipt(&receipt_id, &payload),
        RetentionClass::new("retention.projector-test").unwrap(),
        payload,
    )
    .unwrap();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation.clone(), None, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new("projection.projector-test.v1").unwrap();
    let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
        write.observation(),
        "projector-test",
    )
    .unwrap();
    let anchor = tracedecay_store::build_observation_retrieval_anchor(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    let mut anchor_json = serde_json::to_value(anchor).unwrap();
    if let Some((relation, anchor_id)) = lineage {
        anchor_json["source_anchors"] = json!([{
            "relation": relation,
            "anchor_id": anchor_id,
            "owner": write.observation().scope(),
        }]);
    }
    let anchor: RetrievalAnchorRecord = serde_json::from_value(anchor_json).unwrap();
    let anchored = AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap();
    (observation, anchored)
}

fn fixture_goal_observation() -> (DurableObservationV1, AnchoredObservationWrite) {
    let record_id = ObservationId::new("record.goal.fixture").unwrap();
    let encoded = include_str!(
        "../../../../tests/fixtures/provider_normalization/codex/thread_goal_updated.expected_envelope.json"
    )
    .replace("$STABLE_RECORD_ID", record_id.as_str());
    let envelope: CanonicalObservationEnvelopeV1 = serde_json::from_str(&encoded).unwrap();
    let provider = envelope.provider().clone();
    let session_id = envelope.relations().session_id().clone();
    let range = envelope.evidence().range();
    let source = ObservationSourceIdentityV1::for_provider(provider, session_id).unwrap();
    let payload = serde_json::to_value(&envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        envelope.evidence().ordering_domain(),
        record_id,
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        identity,
        fixture_receipt("receipt.goal.fixture", &payload),
        RetentionClass::new("retention.projector-test").unwrap(),
        payload,
    )
    .unwrap();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation.clone(), None, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new("projection.projector-test.v1").unwrap();
    let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
        write.observation(),
        "projector-test",
    )
    .unwrap();
    let anchor = tracedecay_store::build_observation_retrieval_anchor(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    let anchored = AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap();
    (observation, anchored)
}

async fn persist_fixture(
    runtime: &HostAdmissionTestRuntimeV1,
    observation: DurableObservationV1,
    anchored: AnchoredObservationWrite,
) {
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .expect("registered profile observation store");
    store.persist_observation(anchored).await.unwrap();
    store
        .project_observation(observation.observation_id())
        .await
        .unwrap();
}

async fn ready_single_observation_projection(
    session_name: &str,
) -> (
    TempDir,
    HostAdmissionTestRuntimeV1,
    SessionRefreshProgressV1,
    SessionTemporalProjectionBatchV1,
) {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let session_id = fixture_session(session_name);
    let (observation, write) = fixture_observation(&session_id, 0, None, false);
    Box::pin(persist_fixture(&runtime, observation, write)).await;
    let store = temporal_store(&runtime);
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    (tmp, runtime, progress, batch)
}

async fn ready_single_observation_completion(
    session_name: &str,
) -> (
    TempDir,
    HostAdmissionTestRuntimeV1,
    SessionRefreshCompletionRequestV1,
    i64,
) {
    let (tmp, runtime, progress, batch) = ready_single_observation_projection(session_name).await;
    let request = SessionRefreshCompletionRequestV1::new(
        progress.operation_id().clone(),
        progress.session_id().clone(),
        progress.frontier(),
        *progress.coverage(),
    )
    .unwrap();
    let generation = i64::try_from(batch.generation().value()).unwrap();
    temporal_store(&runtime)
        .persist_session_refresh_projection_batch(progress, batch)
        .await
        .unwrap();
    (tmp, runtime, request, generation)
}

#[tokio::test]
async fn checked_in_codex_goal_materializes_one_generation_bound_occurrence() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let (observation, anchored) = fixture_goal_observation();
    let session_id = observation.source().session_id().clone();
    let expected_anchor =
        derive_exact_observation_anchor_id(observation.scope(), observation.observation_id())
            .unwrap();
    Box::pin(persist_fixture(&runtime, observation, anchored)).await;
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (_, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(batch.occurrences().len(), 1);
    assert!(batch.copies().is_empty());
    assert!(batch.assertions().is_empty());
    let occurrence = &batch.occurrences()[0];
    assert_eq!(occurrence.session_id, session_id);
    assert_eq!(occurrence.retrieval_anchor_id, expected_anchor);
    assert_eq!(
        occurrence
            .message_id
            .as_ref()
            .map(tracedecay_domain::MessageId::as_str),
        Some("record.goal.fixture")
    );
    assert_eq!(
        occurrence.valid_time,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1_783_500_569_000_000)
        }
    );
}

#[tokio::test]
async fn relation_batch_persists_restarts_and_completes_without_duplicates() {
    let tmp = TempDir::new().unwrap();
    let session_id = fixture_session("session.projector.relation-restart");
    let operation_id;
    {
        let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
            .await
            .unwrap();
        let store = temporal_store(&runtime);
        let (first, first_write) = fixture_observation(&session_id, 0, None, false);
        let first_anchor =
            derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
        Box::pin(persist_fixture(&runtime, first, first_write)).await;
        let (second, second_write) = fixture_observation(
            &session_id,
            1,
            Some((AnchorProvenanceRelation::Supersedes, first_anchor)),
            true,
        );
        Box::pin(persist_fixture(&runtime, second, second_write)).await;
        let begin = store
            .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                SessionRefreshFrontierV1::new(2, 0).unwrap(),
            ))
            .await
            .unwrap();
        operation_id = begin.operation_id().clone();
        let recovery = store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap();
        let (progress, batch) = store
            .materialize_session_temporal_refresh_batch_for_test(&recovery)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(batch.occurrences().len(), 2);
        // A parent-message link is conversation threading, not a logical copy:
        // the derived copy edge requires the occurrence's own logical message id
        // to be the parent link, so a reply contributes no copy record.
        assert!(batch.copies().is_empty());
        assert_eq!(batch.assertions().len(), 1);
        assert_eq!(batch.item_count(), 3);
        assert_eq!(progress.committed_records(), 3);
        assert_eq!(progress.coverage().visible, 3);
        store
            .persist_session_refresh_projection_batch(progress, batch)
            .await
            .unwrap();
    }

    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovery.restart_state(),
        SessionRefreshRestartStateV1::ReadyToComplete
    );
    assert!(
        store
            .materialize_session_temporal_refresh_batch_for_test(&recovery)
            .await
            .unwrap()
            .is_none()
    );
    let progress = recovery.progress().unwrap();
    let request = SessionRefreshCompletionRequestV1::new(
        operation_id,
        session_id,
        progress.frontier(),
        *progress.coverage(),
    )
    .unwrap();
    let receipt = store
        .complete_session_refresh(request.clone(), ExecutionControl::default())
        .await
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    assert_eq!(
        store
            .complete_session_refresh(request, ExecutionControl::default())
            .await
            .unwrap(),
        receipt
    );
    for (kind, expected) in [
        (SessionTemporalFixtureCountV1::ProjectionReceipts, 1),
        (SessionTemporalFixtureCountV1::Occurrences, 2),
        (SessionTemporalFixtureCountV1::Assertions, 1),
        (SessionTemporalFixtureCountV1::RefreshReceipts, 1),
    ] {
        assert_eq!(
            runtime
                .session_temporal_fixture_count_for_test(HostAdmissionScope::Profile, kind)
                .await
                .unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn all_skipped_refresh_persists_reopens_and_completes_idempotently() {
    let tmp = TempDir::new().unwrap();
    let session_id = fixture_session("session.projector.all-skipped");
    let request;
    {
        let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
            .await
            .unwrap();
        let (observation, write) = fixture_observation(&session_id, 0, None, false);
        Box::pin(persist_fixture(&runtime, observation, write)).await;
        let store = temporal_store(&runtime);
        refresh_through(&store, &session_id, 1, 0).await;
        for ordinal in 1..=2 {
            let (observation, write) = fixture_observation_from_facts(
                &session_id,
                ordinal,
                ProviderId::new(format!("projector-test-{ordinal}")).unwrap(),
                ObservationId::new(format!("record.projector.{ordinal}")).unwrap(),
                CanonicalObservationRelationsV1::new(session_id.clone()),
                vec![CanonicalObservationFactV1::Boundary {
                    boundary_kind: tracedecay_domain::CanonicalBoundaryKindV1::TurnStart,
                }],
                None,
            );
            assert_eq!(
                tracedecay_store::derive_canonical_projection(&observation)
                    .unwrap()
                    .skip_reason(),
                Some(ProjectionSkipReason::NonConversationalRecord)
            );
            Box::pin(persist_fixture(&runtime, observation, write)).await;
        }
        store
            .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                SessionRefreshFrontierV1::new(3, 1).unwrap(),
            ))
            .await
            .unwrap();
        let recovery = store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap();
        let (progress, batch) = store
            .materialize_session_temporal_refresh_batch_for_test(&recovery)
            .await
            .unwrap()
            .unwrap();
        assert!(batch.occurrences().is_empty());
        assert_eq!(batch.item_count(), 0);
        assert_eq!(progress.frontier().committed_through(), 3);
        request = SessionRefreshCompletionRequestV1::new(
            progress.operation_id().clone(),
            session_id.clone(),
            progress.frontier(),
            *progress.coverage(),
        )
        .unwrap();
        store
            .persist_session_refresh_projection_batch(progress, batch)
            .await
            .unwrap();
    }
    let receipt = {
        let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
            .await
            .unwrap();
        let store = temporal_store(&runtime);
        let recovery = store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            recovery.restart_state(),
            SessionRefreshRestartStateV1::ReadyToComplete
        );
        let receipt = store
            .complete_session_refresh(request.clone(), ExecutionControl::default())
            .await
            .unwrap();
        assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
        receipt
    };
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    assert_eq!(
        temporal_store(&runtime)
            .complete_session_refresh(request, ExecutionControl::default())
            .await
            .unwrap(),
        receipt
    );
    for (kind, expected) in [
        (SessionTemporalFixtureCountV1::Occurrences, 1),
        (SessionTemporalFixtureCountV1::RefreshReceipts, 2),
    ] {
        assert_eq!(
            runtime
                .session_temporal_fixture_count_for_test(HostAdmissionScope::Profile, kind)
                .await
                .unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn refresh_at_committed_frontier_needs_no_new_observation_effect() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let session_id = fixture_session("session.projector.unchanged-frontier");
    let (observation, write) = fixture_observation(&session_id, 0, None, false);
    Box::pin(persist_fixture(&runtime, observation, write)).await;
    let store = temporal_store(&runtime);
    let baseline = refresh_through(&store, &session_id, 1, 0).await;
    let refreshed = refresh_through(&store, &session_id, 1, 1).await;
    assert_ne!(refreshed, baseline);
    assert_eq!(
        runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::Occurrences
            )
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn refresh_frontier_without_observation_effects_cannot_activate() {
    assert_unsupported_frontier_cannot_activate(FrontierPrefix::Absent, false).await;
}

#[tokio::test]
async fn skipped_prefix_cannot_prove_an_unsupported_target_frontier() {
    for foreign_target in [false, true] {
        assert_unsupported_frontier_cannot_activate(FrontierPrefix::Skipped, foreign_target).await;
    }
}

#[tokio::test]
async fn message_prefix_cannot_prove_an_unsupported_target_frontier() {
    assert_unsupported_frontier_cannot_activate(FrontierPrefix::Message, false).await;
}

enum FrontierPrefix {
    Absent,
    Skipped,
    Message,
}

async fn assert_unsupported_frontier_cannot_activate(prefix: FrontierPrefix, foreign_target: bool) {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let session_id = fixture_session("session.projector.unsupported-frontier");
    let (observation, write) = fixture_observation(&session_id, 0, None, false);
    Box::pin(persist_fixture(&runtime, observation, write)).await;
    let store = temporal_store(&runtime);
    refresh_through(&store, &session_id, 1, 0).await;
    let target = if matches!(prefix, FrontierPrefix::Skipped) {
        let (observation, write) = fixture_observation_from_facts(
            &session_id,
            1,
            ProviderId::new("projector-test-1").unwrap(),
            ObservationId::new("record.projector.1").unwrap(),
            CanonicalObservationRelationsV1::new(session_id.clone()),
            vec![CanonicalObservationFactV1::Boundary {
                boundary_kind: tracedecay_domain::CanonicalBoundaryKindV1::TurnStart,
            }],
            None,
        );
        assert_eq!(
            tracedecay_store::derive_canonical_projection(&observation)
                .unwrap()
                .skip_reason(),
            Some(ProjectionSkipReason::NonConversationalRecord)
        );
        Box::pin(persist_fixture(&runtime, observation, write)).await;
        3
    } else if matches!(prefix, FrontierPrefix::Message) {
        let (observation, write) = fixture_observation(&session_id, 1, None, false);
        Box::pin(persist_fixture(&runtime, observation, write)).await;
        3
    } else {
        2
    };
    if foreign_target {
        let foreign_session = fixture_session("session.projector.foreign-frontier");
        let (observation, write) = fixture_observation(&foreign_session, 2, None, false);
        Box::pin(persist_fixture(&runtime, observation, write)).await;
    }
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(target, 1).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        batch.occurrences().len(),
        usize::from(matches!(prefix, FrontierPrefix::Message))
    );
    assert_eq!(progress.frontier().committed_through(), target);
    let request = SessionRefreshCompletionRequestV1::new(
        progress.operation_id().clone(),
        session_id.clone(),
        progress.frontier(),
        *progress.coverage(),
    )
    .unwrap();
    store
        .persist_session_refresh_projection_batch(progress, batch)
        .await
        .unwrap();
    let error = store
        .complete_session_refresh(request, ExecutionControl::default())
        .await
        .expect_err("a prefix batch cannot prove an unsupported source frontier");
    assert!(matches!(error, SessionStoreError::Storage { .. }));
    assert_eq!(
        store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap()
            .restart_state(),
        SessionRefreshRestartStateV1::ReadyToComplete
    );
    assert_eq!(
        runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::RefreshReceipts
            )
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn terminal_receipt_rejects_corrupted_derived_evidence() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.derived-receipt-corruption");
    for ordinal in 0..3 {
        let (observation, write) = fixture_observation(&session_id, ordinal, None, ordinal > 0);
        Box::pin(persist_fixture(&runtime, observation, write)).await;
    }
    let begin = store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(3, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    let generation = batch.generation();
    store
        .persist_session_refresh_projection_batch(progress.clone(), batch)
        .await
        .unwrap();

    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    let changed = transaction
        .execute(
            "UPDATE session_derived_evidence
             SET evidence_json = json_set(evidence_json, '$.corrupted', 1)
             WHERE session_id = ?1 AND generation = ?2",
            params![
                session_id.as_str(),
                i64::try_from(generation.value()).unwrap()
            ],
        )
        .await
        .unwrap();
    assert!(
        changed > 0,
        "fixture must produce receipt-bound derived evidence"
    );
    transaction.commit().await.unwrap();

    let request = SessionRefreshCompletionRequestV1::new(
        begin.operation_id().clone(),
        session_id,
        progress.frontier(),
        *progress.coverage(),
    )
    .unwrap();
    let error = store
        .complete_session_refresh(request, ExecutionControl::default())
        .await
        .expect_err("derived evidence corruption must invalidate the terminal receipt");
    assert!(matches!(error, SessionStoreError::Storage { .. }));
}

#[tokio::test]
async fn refresh_begin_reports_busy_only_for_a_running_operation() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let busy_session = fixture_session("session.refresh.busy");
    let refused_session = fixture_session("session.refresh.refused-row");
    for ordinal in 0..2 {
        let (observation, write) = fixture_observation(&busy_session, ordinal, None, false);
        Box::pin(persist_fixture(&runtime, observation, write)).await;
    }
    let (observation, write) = fixture_observation(&refused_session, 2, None, false);
    Box::pin(persist_fixture(&runtime, observation, write)).await;

    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            busy_session.clone(),
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
        ))
        .await
        .unwrap();
    let busy = store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            busy_session,
            SessionRefreshFrontierV1::new(2, 0).unwrap(),
        ))
        .await
        .expect_err("a second target while one refresh runs is busy");
    assert_eq!(
        busy.to_string(),
        "session temporal idempotency conflict in session refresh busy"
    );

    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "CREATE TRIGGER refuse_refresh_operation_row
             BEFORE INSERT ON session_refresh_operations
             WHEN NEW.session_id = 'session.refresh.refused-row'
             BEGIN SELECT RAISE(ABORT, 'session_refresh_operations row refused'); END",
            (),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let refused = store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            refused_session,
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
        ))
        .await
        .expect_err("a refused operation row fails the begin");
    assert!(
        matches!(
            refused,
            SessionStoreError::Storage {
                operation: "begin or join session refresh",
                ..
            }
        ),
        "a refused row with no running refresh is a storage fault, got {refused}"
    );
}

#[tokio::test]
async fn cancelled_terminal_persistence_rolls_back_candidate_state() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.cancelled-terminal-persist");
    let (observation, write) = fixture_observation(&session_id, 0, None, false);
    Box::pin(persist_fixture(&runtime, observation, write)).await;
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    let error = store
        .persist_session_refresh_projection_batch_controlled(
            progress,
            batch,
            // Seeding consumes 21 checkpoints, persistence admission one,
            // the source group one, and the first occurrence one. The next
            // terminal-phase checkpoint therefore fails after that occurrence
            // has been inserted into the still-uncommitted candidate.
            ExecutionControl::default().with_work_limit(24),
        )
        .await
        .expect_err("terminal persistence must honor its execution budget");
    assert!(matches!(error, SessionStoreError::BudgetExceeded { .. }));
    assert_eq!(
        runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::ProjectionReceipts,
            )
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::Occurrences,
            )
            .await
            .unwrap(),
        0,
        "the occurrence inserted before cancellation must roll back with the transaction"
    );
    assert_eq!(
        runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::RefreshProgress,
            )
            .await
            .unwrap(),
        0,
        "projection progress must not advance when the candidate transaction rolls back"
    );
    assert_eq!(
        store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap()
            .restart_state(),
        SessionRefreshRestartStateV1::BeginProjection
    );
}

#[tokio::test]
async fn cancellation_at_final_precommit_checkpoint_rolls_back_all_projection_writes() {
    const WORK_LIMIT: usize = 4_096;

    let (_successful_tmp, successful_runtime, successful_progress, successful_batch) =
        ready_single_observation_projection("session.projector.precommit-meter").await;
    let successful_store = temporal_store(&successful_runtime);
    let successful_control = ExecutionControl::default().with_work_limit(WORK_LIMIT);
    let meter = successful_control.clone();
    successful_store
        .persist_session_refresh_projection_batch_controlled(
            successful_progress,
            successful_batch,
            successful_control,
        )
        .await
        .unwrap();
    let mut unused_work = 0usize;
    while meter.checkpoint().is_ok() {
        unused_work += 1;
    }
    let used_work = WORK_LIMIT.checked_sub(unused_work).unwrap();
    assert!(used_work > 1 && used_work < WORK_LIMIT);

    let (_cancelled_tmp, cancelled_runtime, cancelled_progress, cancelled_batch) =
        ready_single_observation_projection("session.projector.precommit-cancel").await;
    let cancelled_store = temporal_store(&cancelled_runtime);
    let session_id = cancelled_progress.session_id().clone();
    let error = cancelled_store
        .persist_session_refresh_projection_batch_controlled(
            cancelled_progress,
            cancelled_batch,
            ExecutionControl::default().with_work_limit(used_work - 1),
        )
        .await
        .expect_err("the final checkpoint must reject cancellation before commit");
    assert!(matches!(error, SessionStoreError::BudgetExceeded { .. }));
    for kind in [
        SessionTemporalFixtureCountV1::ProjectionReceipts,
        SessionTemporalFixtureCountV1::Occurrences,
        SessionTemporalFixtureCountV1::RefreshProgress,
    ] {
        assert_eq!(
            cancelled_runtime
                .session_temporal_fixture_count_for_test(HostAdmissionScope::Profile, kind)
                .await
                .unwrap(),
            0,
            "all candidate writes before the final checkpoint must roll back"
        );
    }
    let database = cancelled_runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    let mut rows = transaction
        .query(
            "SELECT COUNT(*) FROM session_derived_evidence WHERE session_id = ?1",
            params![session_id.as_str()],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);
    drop(transaction);
    assert_eq!(
        cancelled_store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap()
            .restart_state(),
        SessionRefreshRestartStateV1::BeginProjection
    );
}

#[tokio::test]
async fn cancellation_at_completion_precommit_rolls_back_and_the_retry_completes() {
    const WORK_LIMIT: usize = 4_096;
    // Sync point, not a performance budget: one less than the successful
    // completion's checkpoint count so cancellation fires on the pre-commit
    // checkpoint after activation and the terminal receipt. The measured
    // count includes identity-index cancellation polls during relation load.
    const COMPLETION_PRECOMMIT_WORK_LIMIT: usize = 82;

    let (_successful_tmp, successful_runtime, successful_request, _) =
        ready_single_observation_completion("session.projector.completion-meter").await;
    let successful_control = ExecutionControl::default().with_work_limit(WORK_LIMIT);
    let meter = successful_control.clone();
    temporal_store(&successful_runtime)
        .complete_session_refresh(successful_request, successful_control)
        .await
        .unwrap();
    let mut unused_work = 0usize;
    while meter.checkpoint().is_ok() {
        unused_work += 1;
    }
    let used_work = WORK_LIMIT.checked_sub(unused_work).unwrap();
    assert_eq!(
        used_work,
        COMPLETION_PRECOMMIT_WORK_LIMIT + 1,
        "the cancellation budget below must expire only after finalization writes"
    );

    let (_cancelled_tmp, cancelled_runtime, cancelled_request, candidate_generation) =
        ready_single_observation_completion("session.projector.completion-cancel").await;
    let session_id = cancelled_request.session_id().clone();
    let operation_id = cancelled_request.operation_id().clone();
    let error = temporal_store(&cancelled_runtime)
        .complete_session_refresh(
            cancelled_request.clone(),
            ExecutionControl::default().with_work_limit(COMPLETION_PRECOMMIT_WORK_LIMIT),
        )
        .await
        .expect_err("completion must checkpoint after finalization writes");
    assert!(matches!(error, SessionStoreError::BudgetExceeded { .. }));

    let database = cancelled_runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    let mut generation_rows = transaction
        .query(
            "SELECT state FROM session_temporal_generations
             WHERE session_id = ?1 AND generation = ?2",
            params![session_id.as_str(), candidate_generation],
        )
        .await
        .unwrap();
    assert_eq!(
        generation_rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "building"
    );
    drop(generation_rows);
    let mut operation_rows = transaction
        .query(
            "SELECT state FROM session_refresh_operations
             WHERE session_id = ?1 AND operation_id = ?2",
            params![session_id.as_str(), operation_id.as_str()],
        )
        .await
        .unwrap();
    assert_eq!(
        operation_rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "running"
    );
    drop(operation_rows);
    drop(transaction);
    assert_eq!(
        cancelled_runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::RefreshReceipts,
            )
            .await
            .unwrap(),
        0
    );

    // The cancelled attempt already applied the candidate's relation graph,
    // so the retry must replay that receipt rather than refuse it.
    let receipt = temporal_store(&cancelled_runtime)
        .complete_session_refresh(cancelled_request, ExecutionControl::default())
        .await
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    let transaction = database.begin_write_transaction().await.unwrap();
    let mut generation_rows = transaction
        .query(
            "SELECT generation || ':' || state FROM session_temporal_generations
             WHERE session_id = ?1 ORDER BY generation",
            params![session_id.as_str()],
        )
        .await
        .unwrap();
    let mut generations = Vec::new();
    while let Some(row) = generation_rows.next().await.unwrap() {
        generations.push(row.get::<String>(0).unwrap());
    }
    assert_eq!(
        generations,
        vec![
            "1:superseded".to_owned(),
            format!("{candidate_generation}:active")
        ]
    );
}

#[tokio::test]
async fn copied_from_lineage_is_not_auto_emitted_by_materializer() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.copied-from");
    let (first, first_write) = fixture_observation(&session_id, 0, None, false);
    let first_anchor =
        derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
    Box::pin(persist_fixture(&runtime, first, first_write)).await;
    let (second, second_write) = fixture_observation(
        &session_id,
        1,
        Some((AnchorProvenanceRelation::CopiedFrom, first_anchor)),
        false,
    );
    Box::pin(persist_fixture(&runtime, second, second_write)).await;
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(2, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(batch.occurrences().len(), 2);
    assert!(batch.copies().is_empty());
    assert!(batch.assertions().is_empty());
    assert_eq!(progress.committed_records(), batch.item_count() as u64);
}

#[tokio::test]
async fn relation_derivation_backs_off_to_the_total_batch_limit() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.derived-limit");
    // Every observation after the first supersedes its predecessor, so each of
    // those occurrences derives one typed assertion edge on top of its own
    // occurrence record. 501 observations therefore derive 1001 records, one
    // past `MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS`, and the materializer
    // must back off to a 500-observation prefix.
    let mut previous_anchor = None;
    for ordinal in 0..501 {
        let lineage = previous_anchor
            .take()
            .map(|anchor| (AnchorProvenanceRelation::Supersedes, anchor));
        let (observation, write) = fixture_observation(&session_id, ordinal, lineage, ordinal > 0);
        previous_anchor = Some(
            derive_exact_observation_anchor_id(observation.scope(), observation.observation_id())
                .unwrap(),
        );
        Box::pin(persist_fixture(&runtime, observation, write)).await;
    }
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(501, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (first_progress, first_batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_batch.occurrences().len(), 500);
    assert!(first_batch.copies().is_empty());
    assert_eq!(first_batch.assertions().len(), 499);
    assert_eq!(first_batch.item_count(), 999);
    assert_eq!(first_progress.frontier().committed_through(), 500);
    assert_eq!(first_progress.committed_records(), 999);
    store
        .persist_session_refresh_projection_batch(first_progress, first_batch)
        .await
        .unwrap();

    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (second_progress, second_batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second_batch.occurrences().len(), 1);
    assert!(second_batch.copies().is_empty());
    assert_eq!(second_batch.assertions().len(), 1);
    assert_eq!(second_batch.item_count(), 2);
    assert_eq!(second_progress.frontier().committed_through(), 501);
    assert_eq!(second_progress.committed_records(), 1001);
    store
        .persist_session_refresh_projection_batch(second_progress, second_batch)
        .await
        .unwrap();
}

#[test]
fn assertion_identity_includes_both_anchors() {
    let session_id = fixture_session("session.projector.assertion-identity");
    let (first, _) = fixture_observation(&session_id, 0, None, false);
    let (second, _) = fixture_observation(&session_id, 1, None, false);
    let first_anchor =
        derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
    let second_anchor =
        derive_exact_observation_anchor_id(second.scope(), second.observation_id()).unwrap();
    let first_id = derived_temporal_assertion_id(
        &first_anchor,
        TemporalAssertionKindV1::Supports,
        &first_anchor,
    );
    let second_id = derived_temporal_assertion_id(
        &first_anchor,
        TemporalAssertionKindV1::Supports,
        &second_anchor,
    );
    let third_id = derived_temporal_assertion_id(
        &second_anchor,
        TemporalAssertionKindV1::Supports,
        &first_anchor,
    );
    assert_ne!(first_id, second_id);
    assert_ne!(first_id, third_id);
    assert!(first_id.starts_with("sha256:"));
    assert_eq!(first_id.len(), 71);
}

#[tokio::test]
async fn parent_resolver_rejects_ambiguous_session_message_ids() {
    let mut resolver = ParentMessageResolver::default();
    resolver.register("message.shared", "occurrence.a");
    resolver.register("message.shared", "occurrence.b");
    let error = resolver
        .reject_ambiguity()
        .expect_err("duplicate message ids must be rejected");
    assert!(
        matches!(
            &error,
            SessionStoreError::AmbiguousMessageOccurrence { message_id, occurrences: 2 }
                if message_id == "message.shared"
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn parent_resolver_pages_live_sized_observation_history() {
    let directory = TempDir::new().unwrap();
    let connection = TestConnection::open(&directory.path().join("resolver-pages.db"));
    connection
        .execute_batch(
            "CREATE TABLE observations (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                observation_id TEXT NOT NULL UNIQUE,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE session_temporal_observation_effects (
                observation_id TEXT PRIMARY KEY,
                observation_sequence INTEGER NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                output_count INTEGER NOT NULL
             );
             CREATE INDEX idx_session_temporal_observation_effects_session
                ON session_temporal_observation_effects(session_id, observation_sequence);
             CREATE TABLE observation_projection_dispositions (
                projector_version TEXT NOT NULL,
                observation_id TEXT NOT NULL,
                reason TEXT NOT NULL,
                PRIMARY KEY(projector_version, observation_id)
             );
             CREATE TABLE observation_projection_provenance (
                projector_version TEXT NOT NULL,
                observation_id TEXT NOT NULL,
                output_provider TEXT NOT NULL,
                output_message_id TEXT NOT NULL,
                message_created INTEGER NOT NULL
             );",
        )
        .await
        .unwrap();
    let session_id = fixture_session("session.projector.paged-parent-resolver");
    let (observation, _) = fixture_observation(&session_id, 0, None, false);
    let encoded = serde_json::to_string(&observation).unwrap();
    connection
        .execute(
            "WITH RECURSIVE fixture(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM fixture WHERE value < 10001
             )
             INSERT INTO observations (observation_id, observation_json)
             SELECT printf('observation.%05d', value), ?1 FROM fixture",
            params![encoded],
        )
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_temporal_observation_effects (
                 observation_id, observation_sequence, session_id, output_count
             )
             SELECT observation_id, sequence, ?1, 1 FROM observations",
            params![session_id.as_str()],
        )
        .await
        .unwrap();

    let resolver = canonical_parent_message_resolver(
        &*connection,
        session_id.as_str(),
        0,
        10001,
        "test paged parent resolver",
        None,
        false,
    )
    .await
    .unwrap();

    assert_eq!(
        resolver.resolve("message.projector.0"),
        Some(
            MessageOccurrenceIdV1::derive(
                observation.observation_id(),
                tracedecay_domain::ProjectionOutputOrdinalV1::new(0),
            )
            .as_str()
        )
    );
    assert_eq!(resolver.resolve("message.missing"), None);
}

#[tokio::test]
async fn parent_resolver_has_bounded_cancellable_session_traversal() {
    const UNRELATED_OBSERVATIONS: i64 = 2_048;

    let directory = TempDir::new().unwrap();
    let connection = TestConnection::open(&directory.path().join("resolver-session-bound.db"));
    connection
        .execute_batch(
            "CREATE TABLE observations (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                observation_id TEXT NOT NULL UNIQUE,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE session_temporal_observation_effects (
                observation_id TEXT PRIMARY KEY,
                observation_sequence INTEGER NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                output_count INTEGER NOT NULL
             );
             CREATE INDEX idx_session_temporal_observation_effects_session
                ON session_temporal_observation_effects(session_id, observation_sequence);
             CREATE TABLE observation_projection_dispositions (
                projector_version TEXT NOT NULL,
                observation_id TEXT NOT NULL,
                reason TEXT NOT NULL,
                PRIMARY KEY(projector_version, observation_id)
             );
             CREATE TABLE observation_projection_provenance (
                projector_version TEXT NOT NULL,
                observation_id TEXT NOT NULL,
                output_provider TEXT NOT NULL,
                output_message_id TEXT NOT NULL,
                message_created INTEGER NOT NULL
             );",
        )
        .await
        .unwrap();
    let unrelated_session = fixture_session("session.projector.unrelated-history");
    let (unrelated, _) = fixture_observation(&unrelated_session, 0, None, false);
    let unrelated_json = serde_json::to_string(&unrelated).unwrap();
    connection
        .execute(
            "WITH RECURSIVE fixture(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM fixture WHERE value < ?2
             )
             INSERT INTO observations (observation_id, observation_json)
             SELECT printf('observation.unrelated.%05d', value), ?1 FROM fixture",
            params![unrelated_json, UNRELATED_OBSERVATIONS],
        )
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_temporal_observation_effects (
                 observation_id, observation_sequence, session_id, output_count
             )
             SELECT observation_id, sequence, ?1, 1 FROM observations",
            params![unrelated_session.as_str()],
        )
        .await
        .unwrap();

    let session_id = fixture_session("session.projector.session-bound-parent-resolver");
    let (observation, _) = fixture_observation(&session_id, 0, None, false);
    connection
        .execute(
            "INSERT INTO observations (observation_id, observation_json) VALUES (?1, ?2)",
            params![
                observation.observation_id().as_str(),
                serde_json::to_string(&observation).unwrap(),
            ],
        )
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_temporal_observation_effects (
                 observation_id, observation_sequence, session_id, output_count
             )
             SELECT observation_id, sequence, ?2, 1
             FROM observations WHERE observation_id = ?1",
            params![observation.observation_id().as_str(), session_id.as_str()],
        )
        .await
        .unwrap();

    let counted = QueryCountingConnection::new(&connection);
    let resolver = canonical_parent_message_resolver(
        &counted,
        session_id.as_str(),
        0,
        u64::try_from(UNRELATED_OBSERVATIONS + 1).unwrap(),
        "test session-bounded parent resolver",
        None,
        false,
    )
    .await
    .unwrap();

    assert_eq!(
        resolver.resolve("message.projector.0"),
        Some(
            MessageOccurrenceIdV1::derive(
                observation.observation_id(),
                ProjectionOutputOrdinalV1::new(0),
            )
            .as_str()
        )
    );
    assert_eq!(resolver.resolve("message.missing"), None);
    assert!(
        counted.query_count() <= 2,
        "one relevant history page plus the terminal probe is sufficient, but {} queries visited unrelated profile history",
        counted.query_count()
    );

    let control = ExecutionControl::default().with_work_limit(1);
    let error = canonical_parent_message_resolver(
        &connection,
        session_id.as_str(),
        0,
        u64::try_from(UNRELATED_OBSERVATIONS + 1).unwrap(),
        "test cancellable parent resolver",
        Some(&control),
        false,
    )
    .await
    .expect_err("the resolver must checkpoint while visiting its first row");
    assert!(matches!(error, SessionStoreError::BudgetExceeded { .. }));
}

#[test]
fn parent_resolver_prefers_a_persisted_cross_batch_predecessor() {
    let mut resolver = ParentMessageResolver::default();
    resolver.register("message.reemitted", "occurrence.persisted-predecessor");
    assert_eq!(
        resolver.resolve("message.reemitted"),
        Some("occurrence.persisted-predecessor")
    );

    resolver.register("message.reemitted", "occurrence.aaa-current-effect");
    assert_eq!(
        resolver.resolve("message.reemitted"),
        Some("occurrence.persisted-predecessor")
    );
}

#[tokio::test]
async fn explicit_copy_survives_reconstruction_in_the_native_relation_graph() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.copy-bitemporal");
    let (first, first_write) = fixture_observation(&session_id, 0, None, false);
    let first_anchor =
        derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
    Box::pin(persist_fixture(&runtime, first, first_write)).await;
    let (second, second_write) = fixture_observation(
        &session_id,
        1,
        Some((AnchorProvenanceRelation::CopiedFrom, first_anchor.clone())),
        false,
    );
    Box::pin(persist_fixture(&runtime, second, second_write)).await;
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(2, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    // Explicit copy topology is supplied by the typed projection record and
    // must remain reconstructable from the canonical anchor lineage.
    assert!(batch.copies().is_empty());
    let copy = LogicalCopyRecordV1 {
        occurrence_id: batch.occurrences()[1].occurrence_id.clone(),
        copied_from_occurrence_id: batch.occurrences()[0].occurrence_id.clone(),
        proof: CopyProofV1::ExplicitAnchorAssertion {
            source_occurrence_id: batch.occurrences()[0].occurrence_id.clone(),
            assertion_anchor_id: first_anchor,
        },
        knowledge_at: batch.occurrences()[1].knowledge_at,
        valid_time: batch.occurrences()[1].valid_time,
    };
    let expected_copy = copy.clone();
    let batch = SessionTemporalProjectionBatchV1::new(
        batch.session_id().clone(),
        batch.generation(),
        batch.watermarks().clone(),
        batch.occurrences().to_vec(),
        vec![copy],
        batch.assertions().to_vec(),
    )
    .unwrap()
    .with_checkpoint(
        batch.batch_ordinal(),
        batch.source_through(),
        batch.projection_through(),
    )
    .unwrap();
    let mut coverage = *progress.coverage();
    coverage.visible += 1;
    let source_coverage = progress.source_coverage().cloned();
    let mut progress = SessionRefreshProgressV1::new(
        progress.operation_id().clone(),
        progress.session_id().clone(),
        progress.frontier(),
        coverage,
        progress.committed_batches(),
        progress.committed_records() + 1,
        progress.updated_at(),
    );
    if let Some(source_coverage) = source_coverage {
        progress = progress.with_source_coverage(source_coverage);
    }
    assert_eq!(batch.item_count(), 3);

    store
        .persist_session_refresh_projection_batch(progress, batch.clone())
        .await
        .unwrap();
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let snapshot = database.read_snapshot().await.unwrap();
    let (scope, relation_store) =
        SessionTemporalRegisteredDb::session_relation_store(database).unwrap();
    let projection = super::super::relation_projection::reconstruct_session_relation_projection(
        &snapshot,
        &scope,
        &session_id,
        batch.generation(),
        100,
        100,
        Arc::new(NeverCancelled),
    )
    .await
    .unwrap();
    relation_store.replace(&projection).unwrap();
    let loaded = relation_store
        .load_projection(
            &scope,
            &session_id,
            batch.generation().value(),
            100,
            100,
            Arc::new(NeverCancelled),
        )
        .unwrap();
    assert_eq!(
        loaded.logical_copies,
        vec![crate::relations::LogicalCopyRelation {
            occurrence_id: expected_copy.occurrence_id,
            copied_from_occurrence_id: expected_copy.copied_from_occurrence_id,
            proof: expected_copy.proof,
            knowledge_at: expected_copy.knowledge_at,
            valid_time: expected_copy.valid_time,
        }]
    );
}

#[tokio::test]
async fn multi_batch_refresh_progress_survives_restart_under_guard() {
    const OBSERVATION_COUNT: u64 = 501;

    let tmp = TempDir::new().unwrap();
    let session_id = fixture_session("session.projector.multi-batch-guard");
    let operation_id;
    {
        let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
            .await
            .unwrap();
        let store = temporal_store(&runtime);
        let (first, first_write) = fixture_observation(&session_id, 0, None, false);
        let first_anchor =
            derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
        Box::pin(persist_fixture(&runtime, first, first_write)).await;
        for ordinal in 1..OBSERVATION_COUNT {
            let (observation, write) = fixture_observation(
                &session_id,
                ordinal,
                Some((AnchorProvenanceRelation::Supersedes, first_anchor.clone())),
                false,
            );
            Box::pin(persist_fixture(&runtime, observation, write)).await;
        }
        let begin = store
            .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                SessionRefreshFrontierV1::new(OBSERVATION_COUNT, 0).unwrap(),
            ))
            .await
            .unwrap();
        operation_id = begin.operation_id().clone();
        let recovery = store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap();
        let (progress, batch) = store
            .materialize_session_temporal_refresh_batch_for_test(&recovery)
            .await
            .unwrap()
            .unwrap();
        assert!(batch.item_count() > 0);
        assert!(progress.frontier().committed_through() > 0);
        assert!(progress.frontier().committed_through() < OBSERVATION_COUNT);
        store
            .persist_session_refresh_projection_batch(progress, batch)
            .await
            .unwrap();
    }

    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    match recovery.restart_state() {
        SessionRefreshRestartStateV1::ResumeProjection { .. }
        | SessionRefreshRestartStateV1::ReadyToComplete => {}
        state @ SessionRefreshRestartStateV1::BeginProjection => {
            panic!("unexpected restart state after first batch: {state:?}")
        }
    }
    if let Some((progress, batch)) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
    {
        store
            .persist_session_refresh_projection_batch(progress, batch)
            .await
            .unwrap();
    }
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovery.restart_state(),
        SessionRefreshRestartStateV1::ReadyToComplete
    );
    let progress = recovery.progress().unwrap();
    let receipt = store
        .complete_session_refresh(
            SessionRefreshCompletionRequestV1::new(
                operation_id,
                session_id,
                progress.frontier(),
                *progress.coverage(),
            )
            .unwrap(),
            ExecutionControl::default(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    assert_eq!(
        runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::RefreshProgress,
            )
            .await
            .unwrap(),
        progress.committed_batches() as i64
    );
}

/// Seeds the receipt and observation rows that
/// `session_temporal_observation_effects` requires: its insert guard aborts
/// unless `(observation_id, observation_sequence, receipt_id)` already names a
/// committed observation.
async fn seed_effect_observation(
    conn: &impl crate::handle::SessionTemporalExec,
    observation: &DurableObservationV1,
) -> u64 {
    let receipt = observation.receipt();
    conn.execute(
        "INSERT INTO sanitization_receipts
         (receipt_id, sanitizer_version, payload_digest, receipt_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            receipt.receipt().receipt_id().as_str(),
            receipt.receipt().sanitizer_version().as_str(),
            observation.payload_reference().digest().as_str(),
            serde_json::to_string(receipt).unwrap(),
        ],
    )
    .await
    .unwrap();
    let cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations
         (observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            observation.observation_id().as_str(),
            observation.payload_reference().digest().as_str(),
            receipt.receipt().receipt_id().as_str(),
            serde_json::to_string(observation).unwrap(),
            serde_json::to_string(&cursor).unwrap(),
        ],
    )
    .await
    .unwrap();
    let mut rows = conn
        .query(
            "SELECT sequence FROM observations WHERE observation_id = ?1",
            params![observation.observation_id().as_str()],
        )
        .await
        .unwrap();
    let sequence = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    u64::try_from(sequence).unwrap()
}

async fn recorded_effect(
    conn: &TestConnection,
    observation: &DurableObservationV1,
) -> Option<(i64, String, String, i64)> {
    let mut rows = conn
        .query(
            "SELECT observation_sequence, session_id, effect_digest, output_count
             FROM session_temporal_observation_effects WHERE observation_id = ?1",
            params![observation.observation_id().as_str()],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap()?;
    Some((
        row.get::<i64>(0).unwrap(),
        row.get::<String>(1).unwrap(),
        row.get::<String>(2).unwrap(),
        row.get::<i64>(3).unwrap(),
    ))
}

async fn open_effect_store(name: &str) -> (TempDir, TestConnection) {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join(format!("{name}.db"));
    drop(
        open_registered_test_database_fixture(
            &database_path,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap(),
    );
    (directory, TestConnection::open(&database_path))
}

/// Case 2, idempotent replay. Re-projecting an observation at or below the
/// checkpoint conflicts on the primary key, and the conflict branch's
/// field-by-field comparison must converge instead of erroring.
#[tokio::test]
async fn canonical_effect_replay_converges_on_an_identical_row() {
    let (_directory, connection) = open_effect_store("effect-identical-replay").await;
    let session_id = fixture_session("session.projector.effect-replay");
    let (observation, _) = fixture_observation(&session_id, 0, None, false);
    let sequence = seed_effect_observation(&connection, &observation).await;
    let effect = ObservationProjection::Skipped(ProjectionSkipReason::NonConversationalRecord);

    record_canonical_observation_effect(&connection, sequence, &observation, &effect)
        .await
        .unwrap();
    let first = recorded_effect(&connection, &observation).await.unwrap();

    record_canonical_observation_effect(&connection, sequence, &observation, &effect)
        .await
        .unwrap();

    assert_eq!(
        recorded_effect(&connection, &observation).await,
        Some(first)
    );
}

/// Case 3, conflict with a divergent payload. The durable row satisfies the
/// insert guard (same observation, sequence, and receipt) yet disagrees on the
/// projected effect, so the conflict-only read-back must still reject it.
#[tokio::test]
async fn canonical_effect_replay_rejects_a_divergent_durable_row() {
    let (_directory, connection) = open_effect_store("effect-divergent-row").await;
    let session_id = fixture_session("session.projector.effect-divergent");
    let (observation, _) = fixture_observation(&session_id, 0, None, false);
    let sequence = seed_effect_observation(&connection, &observation).await;
    connection
        .execute(
            "INSERT INTO session_temporal_observation_effects (
                observation_id, observation_sequence, session_id, receipt_id,
                effect_digest, output_count, recorded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
            params![
                observation.observation_id().as_str(),
                i64::try_from(sequence).unwrap(),
                session_id.as_str(),
                observation.receipt().receipt().receipt_id().as_str(),
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                7_i64,
            ],
        )
        .await
        .unwrap();
    let effect = ObservationProjection::Skipped(ProjectionSkipReason::NonConversationalRecord);

    let error = record_canonical_observation_effect(&connection, sequence, &observation, &effect)
        .await
        .expect_err("a divergent durable effect must not be accepted as a replay");

    assert!(
        matches!(error, ProjectionStoreError::ProvenanceCollision),
        "{error:?}"
    );
    assert_eq!(
        recorded_effect(&connection, &observation).await,
        Some((
            i64::try_from(sequence).unwrap(),
            session_id.as_str().to_owned(),
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            7,
        ))
    );
}

const HISTORY_ONLY_EFFECTS: u64 = 24;

const HEAD_GROUPING_SET_SQL: &str = "
    SELECT COUNT(*)
    FROM session_temporal_observation_effects AS effect
    LEFT JOIN session_refresh_operations AS running
      ON running.session_id = effect.session_id
     AND running.state = 'running'
    WHERE running.session_id IS NULL
";

const FILTERED_DISCOVERY_SQL: &str = "
    WITH active AS (
        SELECT session_id, frozen_watermarks_json
        FROM session_temporal_generations
        WHERE state = 'active'
    )
    SELECT COUNT(*)
    FROM session_temporal_observation_effects AS effect
    LEFT JOIN active ON active.session_id = effect.session_id
    LEFT JOIN session_refresh_operations AS running
      ON running.session_id = effect.session_id
     AND running.state = 'running'
    WHERE running.session_id IS NULL
      AND effect.output_count > 0
      AND effect.observation_sequence > COALESCE(
            CAST(json_extract(
                active.frozen_watermarks_json,
                '$.projection_frontier'
            ) AS INTEGER),
            0
      )
";

async fn count_effects(runtime: &HostAdmissionTestRuntimeV1, sql: &str) -> u64 {
    let snapshot = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("profile registered database")
        .read_snapshot()
        .await
        .expect("effect-count snapshot");
    let mut rows = snapshot.query(sql, ()).await.expect("effect-count query");
    let value: i64 = rows
        .next()
        .await
        .expect("effect-count row")
        .expect("effect-count missing row")
        .get(0)
        .expect("effect-count column");
    u64::try_from(value).expect("effect-count fits u64")
}

async fn seed_output_session_with_history_only_effects(
    runtime: &HostAdmissionTestRuntimeV1,
    session_id: &SessionId,
    history_only: u64,
) {
    let (observation, write) = fixture_observation(session_id, 0, None, false);
    Box::pin(persist_fixture(runtime, observation, write)).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("profile registered database");
    let transaction = db
        .begin_write_transaction()
        .await
        .expect("history-only effect transaction");
    for ordinal in 1..=history_only {
        let (observation, _) = fixture_observation(session_id, ordinal, None, false);
        let sequence = seed_effect_observation(&transaction, &observation).await;
        record_canonical_observation_effect(
            &transaction,
            sequence,
            &observation,
            &ObservationProjection::Skipped(ProjectionSkipReason::NonConversationalRecord),
        )
        .await
        .expect("history-only effect");
    }
    transaction
        .commit()
        .await
        .expect("commit history-only effects");
}

/// HEAD grouped every historical effect for a pending session. Discovery must
/// visit only output-producing rows past the frontier, not the history-only
/// prefix on the same session.
#[tokio::test]
async fn explicit_discovery_visits_only_output_effects_past_frontier() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let session_id = fixture_session("session.projector.filtered-discovery");
    Box::pin(seed_output_session_with_history_only_effects(
        &runtime,
        &session_id,
        HISTORY_ONLY_EFFECTS,
    ))
    .await;
    let grouping_set = count_effects(&runtime, HEAD_GROUPING_SET_SQL).await;
    let filtered = count_effects(&runtime, FILTERED_DISCOVERY_SQL).await;
    assert!(
        grouping_set >= filtered.saturating_add(HISTORY_ONLY_EFFECTS),
        "HEAD grouping set {grouping_set} must include the {HISTORY_ONLY_EFFECTS} history-only rows plus filtered {filtered}"
    );
    assert_eq!(filtered, 1, "one output-producing effect is pending");

    let pending = SessionTemporalAccess::new(
        runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("profile registered database"),
    )
    .pending_session_temporal_refresh_page_result(
        128,
        1,
        &SessionTemporalRefreshDiscoveryCursor::default(),
    )
    .await
    .unwrap()
    .into_parts()
    .0;

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].session_id(), &session_id);
}

/// Discovery reads only effects past its cursor, so an effect that commits
/// while its session's refresh is running must be rediscovered once that
/// refresh ends, and an unchanged store must yield no request.
#[tokio::test]
async fn discovery_cursor_rediscovers_effects_that_arrived_during_a_refresh() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let db = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("profile registered database");
    let session_id = fixture_session("session.projector.cursor-running");
    let discover = |cursor: SessionTemporalRefreshDiscoveryCursor| async move {
        SessionTemporalAccess::new(db)
            .pending_session_temporal_refresh_page_result(8, 1, &cursor)
            .await
            .unwrap()
            .into_parts()
    };
    let frontiers = |requests: &[tracedecay_store::SessionRefreshBeginOrJoinRequestV1]| {
        requests
            .iter()
            .map(|request| {
                (
                    request.session_id().as_str().to_owned(),
                    request.target_frontier().committed_through(),
                    request.target_frontier().observed_through(),
                )
            })
            .collect::<Vec<_>>()
    };

    let (observation, write) = fixture_observation(&session_id, 0, None, false);
    Box::pin(persist_fixture(&runtime, observation, write)).await;
    let (requests, cursor, _) = discover(SessionTemporalRefreshDiscoveryCursor::default()).await;
    let first = frontiers(&requests);
    assert_eq!(first.len(), 1, "{first:?}");
    let first_observed = first[0].2;
    assert_eq!(
        first[0],
        (session_id.as_str().to_owned(), 0, first_observed)
    );
    let store = temporal_store(&runtime);
    store
        .begin_or_join_session_refresh(requests.into_iter().next().unwrap())
        .await
        .unwrap();

    let (observation, write) = fixture_observation(&session_id, 1, None, false);
    Box::pin(persist_fixture(&runtime, observation, write)).await;
    let (requests, cursor, _) = discover(cursor).await;
    assert!(
        requests.is_empty(),
        "a running session is not offered again: {:?}",
        frontiers(&requests)
    );

    store
        .complete_running_session_refresh_for_test(&session_id)
        .await
        .unwrap();
    let (requests, cursor, _) = discover(cursor).await;
    let resumed = frontiers(&requests);
    assert_eq!(resumed.len(), 1, "{resumed:?}");
    assert_eq!(resumed[0].0, session_id.as_str());
    assert_eq!(
        resumed[0].1, first_observed,
        "the completed refresh committed the first effect"
    );
    assert!(
        resumed[0].2 > first_observed,
        "the effect that arrived during the refresh is rediscovered: {resumed:?}"
    );

    store
        .begin_or_join_session_refresh(requests.into_iter().next().unwrap())
        .await
        .unwrap();
    store
        .complete_running_session_refresh_for_test(&session_id)
        .await
        .unwrap();
    let (requests, _, has_more) = discover(cursor).await;
    assert!(requests.is_empty(), "{:?}", frontiers(&requests));
    assert!(
        !has_more,
        "an unchanged, swept store has nothing more to discover"
    );
}

#[tokio::test]
async fn explicit_discovery_rediscovery_is_bounded_and_non_mutating() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let healthy_session_id = fixture_session("session.projector.missing-native-relation.0-healthy");
    let session_id = fixture_session("session.projector.missing-native-relation.a");
    let second_session_id = fixture_session("session.projector.missing-native-relation.b");
    for (session, ordinal) in [
        (&healthy_session_id, 5_000),
        (&session_id, 10_000),
        (&second_session_id, 20_000),
    ] {
        let (observation, write) = fixture_observation(session, ordinal, None, false);
        Box::pin(persist_fixture(&runtime, observation, write)).await;
        temporal_store(&runtime)
            .materialize_pending_session_refresh_for_test(session)
            .await
            .expect("seed active temporal generation");
    }

    let db = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("profile registered database");
    let transaction = db
        .begin_write_transaction()
        .await
        .expect("missing relation receipt fixture transaction");
    assert_eq!(
        transaction
            .execute(
                "DELETE FROM session_relation_receipts WHERE session_id IN (?1, ?2)",
                params![session_id.as_str(), second_session_id.as_str()],
            )
            .await
            .expect("remove relation receipt"),
        2
    );
    transaction
        .commit()
        .await
        .expect("commit missing relation receipt fixture");
    let pending_session_id = fixture_session("session.projector.pending-during-relation-repair");
    let (pending_observation, pending_write) =
        fixture_observation(&pending_session_id, 30_000, None, false);
    Box::pin(persist_fixture(
        &runtime,
        pending_observation,
        pending_write,
    ))
    .await;

    let mut cursor = SessionTemporalRefreshDiscoveryCursor::default();
    let mut discovered = Vec::new();
    let mut pending_pages = 0usize;
    let mut active_rows_scanned = 0usize;
    let mut pages = 0usize;
    loop {
        let page = SessionTemporalAccess::new(db)
            .pending_session_temporal_refresh_page_result(2, 1, &cursor)
            .await
            .expect("discover missing native relation projection");
        active_rows_scanned = active_rows_scanned.saturating_add(page.active_rows_scanned());
        let (requests, next_cursor, has_more) = page.into_parts();
        pages = pages.saturating_add(1);
        for request in requests {
            if request.session_id() == &pending_session_id {
                pending_pages = pending_pages.saturating_add(1);
                continue;
            }
            assert!(
                request.target_frontier().is_complete(),
                "relation repair rebuilds the committed generation without fabricating source work"
            );
            discovered.push(request.session_id().clone());
        }
        if !has_more {
            break;
        }
        cursor = next_cursor;
    }

    assert_eq!(
        discovered,
        [session_id.clone(), second_session_id.clone()],
        "the healthy prefix is skipped once while both missing receipts are discovered in order"
    );
    assert_eq!(
        pages, 4,
        "three bounded pages visit the rows and one empty page proves the wrapped end"
    );
    assert_eq!(
        active_rows_scanned, 3,
        "cursor paging must visit each active row exactly once per sweep"
    );
    assert_eq!(
        pending_pages, 1,
        "the pending lane reports its session once, beside the reserved active scan slot"
    );
    let snapshot = db.read_snapshot().await.expect("relation receipt snapshot");
    let mut rows = snapshot
        .query(
            "SELECT
                 (SELECT COUNT(*) FROM session_temporal_generations
                  WHERE session_id IN (?1, ?2, ?3) AND state = 'active'),
                 (SELECT COUNT(*) FROM session_relation_receipts
                  WHERE session_id IN (?1, ?2, ?3))",
            params![
                healthy_session_id.as_str(),
                session_id.as_str(),
                second_session_id.as_str()
            ],
        )
        .await
        .expect("query rediscovery state");
    let row = rows
        .next()
        .await
        .expect("read rediscovery state")
        .expect("rediscovery state row");
    assert_eq!(row.get::<i64>(0).expect("active generation count"), 3);
    assert_eq!(
        row.get::<i64>(1).expect("relation receipt count"),
        1,
        "read-only discovery must not fabricate an applied relation receipt"
    );

    let mut plan_rows = snapshot
        .query(
            "EXPLAIN QUERY PLAN
             SELECT active.session_id
             FROM session_temporal_generations AS active
             WHERE active.state = 'active'
               AND active.session_id > ?1
             ORDER BY active.session_id
             LIMIT ?2",
            params!["", 2_i64],
        )
        .await
        .expect("plan bounded missing-relation discovery");
    let mut plan = Vec::new();
    while let Some(row) = plan_rows.next().await.expect("missing-relation plan row") {
        plan.push(
            row.get::<String>(3)
                .expect("missing-relation plan detail")
                .to_ascii_uppercase(),
        );
    }
    assert!(
        plan.iter()
            .any(|detail| detail.contains("IDX_SESSION_TEMPORAL_GENERATIONS_ONE_ACTIVE")),
        "repair discovery must walk active sessions in indexed key order: {plan:?}"
    );
    assert!(
        plan.iter()
            .all(|detail| !detail.contains("USE TEMP B-TREE FOR ORDER BY")),
        "repair discovery must stop at the arm-local limit without sorting all active sessions: \
         {plan:?}"
    );
}

/// Completes one refresh of `session_id` through `observed_through`,
/// persisting every materialized batch.
async fn refresh_through(
    store: &SessionTemporalStore<'_, RegisteredGlobalDb>,
    session_id: &SessionId,
    observed_through: u64,
    committed_through: u64,
) -> tracedecay_domain::SessionProjectionGenerationV1 {
    let begin = store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(observed_through, committed_through).unwrap(),
        ))
        .await
        .unwrap();
    loop {
        let recovery = store
            .session_refresh_recovery(session_id)
            .await
            .unwrap()
            .unwrap();
        let Some((progress, batch)) = store
            .materialize_session_temporal_refresh_batch_for_test(&recovery)
            .await
            .unwrap()
        else {
            let progress = recovery.progress().unwrap();
            store
                .complete_session_refresh(
                    SessionRefreshCompletionRequestV1::new(
                        begin.operation_id().clone(),
                        session_id.clone(),
                        progress.frontier(),
                        *progress.coverage(),
                    )
                    .unwrap(),
                    ExecutionControl::default(),
                )
                .await
                .unwrap();
            return recovery.candidate_generation();
        };
        store
            .persist_session_refresh_projection_batch(progress, batch)
            .await
            .unwrap();
    }
}

/// One observation of a live session: the thread changes every third
/// message, each message replies to the previous one, and every fourth
/// message supersedes the one before it.
fn live_observation(
    session_id: &SessionId,
    ordinal: u64,
    previous_anchor: Option<RetrievalAnchorId>,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    let mut relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_thread_id(ObservationId::new(format!("thread.live.{}", ordinal / 3)).unwrap())
        .with_message_id(ObservationId::new(format!("message.live.{ordinal}")).unwrap())
        .with_agent_id(ObservationId::new(format!("agent.live.{}", ordinal % 2)).unwrap());
    if ordinal > 0 {
        relations = relations
            .with_parent_message_id(
                ObservationId::new(format!("message.live.{}", ordinal - 1)).unwrap(),
            )
            .with_parent_agent_id(ObservationId::new("agent.live.root").unwrap());
    }
    fixture_observation_from_facts(
        session_id,
        ordinal,
        ProviderId::new(format!("live-{ordinal}")).unwrap(),
        ObservationId::new(format!("record.live.{ordinal}")).unwrap(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": format!("live message {ordinal}")}),
            model: Some("model.live".to_owned()),
            timestamp: Some(1_750_000_000 + i64::try_from(ordinal).unwrap()),
        }],
        previous_anchor
            .filter(|_| ordinal % 4 == 3)
            .map(|anchor| (AnchorProvenanceRelation::Supersedes, anchor)),
    )
}

#[tokio::test]
async fn incremental_refresh_receipts_and_relations_equal_a_from_scratch_recomputation() {
    const MESSAGES: u64 = 13;
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let (scope, relation_store) =
        SessionTemporalRegisteredDb::session_relation_store(database).unwrap();
    let session_id = fixture_session("session.projector.live-extension");
    let mut previous_anchor = None;
    let mut generations = Vec::new();
    for ordinal in 0..MESSAGES {
        let (observation, write) = live_observation(&session_id, ordinal, previous_anchor.clone());
        previous_anchor = Some(
            derive_exact_observation_anchor_id(observation.scope(), observation.observation_id())
                .unwrap(),
        );
        Box::pin(persist_fixture(&runtime, observation, write)).await;
        let generation = refresh_through(&store, &session_id, ordinal + 1, ordinal).await;
        generations.push(generation);

        let snapshot = database.read_snapshot().await.unwrap();
        let reconstructed =
            super::super::relation_projection::reconstruct_session_relation_projection(
                &snapshot,
                &scope,
                &session_id,
                generation,
                1_000,
                1_000,
                Arc::new(NeverCancelled),
            )
            .await
            .unwrap();
        let applied = relation_store
            .load_projection(
                &scope,
                &session_id,
                generation.value(),
                1_000,
                1_000,
                Arc::new(NeverCancelled),
            )
            .unwrap();
        assert_eq!(
            applied, reconstructed,
            "message {ordinal}: the extended relation projection must equal a from-scratch \
             reconstruction"
        );
        let recomputed = super::full_projection_coverage(
            &snapshot,
            &session_id,
            generation,
            &reconstructed.logical_copies,
            &ExecutionControl::default(),
        )
        .await
        .unwrap();
        let receipted = super::receipts::base_projection_coverage(
            &snapshot,
            &session_id,
            i64::try_from(generation.value()).unwrap() + 1,
        )
        .await
        .unwrap();
        assert_eq!(
            receipted, recomputed,
            "message {ordinal}: the incremental receipt must equal a digest recomputed over \
             every row the generation reads"
        );
        assert_eq!(
            recomputed.record_count(),
            usize::try_from(ordinal + 1 + ordinal.saturating_add(1) / 4).unwrap(),
            "message {ordinal}: occurrences plus supersession assertions"
        );
    }
    assert_eq!(
        generations
            .iter()
            .map(|generation| generation.value())
            .collect::<Vec<_>>(),
        (2..MESSAGES + 2).collect::<Vec<_>>()
    );

    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT
                 (SELECT COUNT(*) FROM session_occurrences WHERE session_id = ?1),
                 (SELECT COUNT(*) FROM session_derived_evidence
                  WHERE session_id = ?1 AND evidence_kind = 'burst'),
                 (SELECT COUNT(*) FROM session_derived_evidence_members
                  WHERE session_id = ?1 AND evidence_kind = 'burst'),
                 (SELECT COUNT(*) FROM session_threads WHERE session_id = ?1)",
            params![session_id.as_str()],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(
        (
            row.get::<i64>(0).unwrap(),
            row.get::<i64>(1).unwrap(),
            row.get::<i64>(2).unwrap(),
            row.get::<i64>(3).unwrap(),
        ),
        (13, 5, 13, 5),
        "each message is stored once, and the superseded burst versions and \
         re-roled members are retired when their successor activates"
    );
}

async fn page_occurrence_ids(
    store: &SessionTemporalStore<'_, RegisteredGlobalDb>,
    snapshot: tracedecay_store::SessionTemporalSnapshotV1,
) -> SessionStoreResult<Vec<String>> {
    let session_id = snapshot.session_id().clone();
    let page = store
        .retrieve_session_temporal_page(
            tracedecay_store::SessionTemporalRetrievalRequestV1::new(
                session_id,
                tracedecay_domain::TemporalModeV1::Evolution,
                tracedecay_domain::RetrievalGrainV1::Occurrence,
                snapshot,
                64,
                None,
                ExecutionControl::default(),
            )
            .unwrap(),
        )
        .await?;
    Ok(page
        .occurrences()
        .iter()
        .map(|occurrence| occurrence.occurrence_id.as_str().to_owned())
        .collect())
}

#[tokio::test]
async fn readers_see_whole_generations_while_an_append_builds_and_after_it_is_cancelled() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.append-isolation");
    let mut occurrence_ids = Vec::new();
    for ordinal in 0..3 {
        let (observation, write) = live_observation(&session_id, ordinal, None);
        occurrence_ids.push(
            MessageOccurrenceIdV1::derive(
                observation.observation_id(),
                ProjectionOutputOrdinalV1::new(0),
            )
            .as_str()
            .to_owned(),
        );
        Box::pin(persist_fixture(&runtime, observation, write)).await;
        refresh_through(&store, &session_id, ordinal + 1, ordinal).await;
    }
    let freeze = || {
        store.freeze_session_temporal_snapshot(
            tracedecay_store::SessionTemporalSnapshotRequestV1::new(session_id.clone()),
        )
    };
    let settled = freeze().await.unwrap();
    let settled_ids = {
        let mut ids = occurrence_ids.clone();
        ids.sort_unstable();
        ids
    };
    let mut page = page_occurrence_ids(&store, settled.clone()).await.unwrap();
    page.sort_unstable();
    assert_eq!(page, settled_ids);

    // A fourth message builds a candidate that shares the settled rows.
    let (observation, write) = live_observation(&session_id, 3, None);
    let appended_id = MessageOccurrenceIdV1::derive(
        observation.observation_id(),
        ProjectionOutputOrdinalV1::new(0),
    )
    .as_str()
    .to_owned();
    Box::pin(persist_fixture(&runtime, observation, write)).await;
    let begin = store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(4, 3).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    store
        .persist_session_refresh_projection_batch(progress.clone(), batch)
        .await
        .unwrap();
    let mut during = page_occurrence_ids(&store, settled.clone()).await.unwrap();
    during.sort_unstable();
    assert_eq!(
        during, settled_ids,
        "a reader of the active generation must not see the building candidate's rows"
    );
    assert_eq!(
        freeze().await.unwrap().watermarks().active_generation(),
        settled.watermarks().active_generation()
    );

    // Cancelling the candidate removes its rows; the settled generation is intact.
    store
        .cancel_session_refresh(tracedecay_store::SessionRefreshCancellationRequestV1::new(
            begin.operation_id().clone(),
            session_id.clone(),
            progress.frontier(),
            *progress.coverage(),
        ))
        .await
        .unwrap();
    let mut after_cancel = page_occurrence_ids(&store, settled.clone()).await.unwrap();
    after_cancel.sort_unstable();
    assert_eq!(after_cancel, settled_ids);
    assert_eq!(
        runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::Occurrences,
            )
            .await
            .unwrap(),
        3,
        "the cancelled candidate's occurrence must be deleted with it"
    );

    // A fresh append activates; the old snapshot is refused rather than
    // silently mixing generations, and the new one reads every row once.
    let appended = refresh_through(&store, &session_id, 4, 3).await;
    assert!(
        page_occurrence_ids(&store, settled).await.is_err(),
        "a snapshot of the superseded generation must be refused"
    );
    let current = freeze().await.unwrap();
    assert_eq!(current.watermarks().active_generation(), appended);
    let mut after = page_occurrence_ids(&store, current).await.unwrap();
    after.sort_unstable();
    let mut expected = settled_ids.clone();
    expected.push(appended_id);
    expected.sort_unstable();
    assert_eq!(after, expected);
}

fn fixture_observation_with_text(
    session_id: &SessionId,
    unique: u64,
    text: String,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    fixture_observation_from_facts(
        session_id,
        0,
        ProviderId::new(format!("projector-test-{unique}")).unwrap(),
        ObservationId::new(format!("record.projector.{unique}")).unwrap(),
        CanonicalObservationRelationsV1::new(session_id.clone())
            .with_thread_id(ObservationId::new(format!("thread.projector.{unique}")).unwrap())
            .with_turn_id(ObservationId::new(format!("turn.projector.{unique}")).unwrap())
            .with_message_id(ObservationId::new(format!("message.projector.{unique}")).unwrap())
            .with_agent_id(ObservationId::new(format!("agent.projector.{unique}")).unwrap()),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": text}),
            model: Some("model.projector".to_owned()),
            timestamp: Some(1_750_000_000 + i64::try_from(unique).unwrap()),
        }],
        None,
    )
}

fn sqlite_family_bytes(path: &std::path::Path) -> u64 {
    ["", "-wal", "-shm"].iter().fold(0u64, |total, suffix| {
        let member = if suffix.is_empty() {
            path.to_path_buf()
        } else {
            let mut name = path.as_os_str().to_os_string();
            name.push(*suffix);
            std::path::PathBuf::from(name)
        };
        total.saturating_add(
            std::fs::metadata(member)
                .map(|meta| meta.len())
                .unwrap_or(0),
        )
    })
}

#[tokio::test]
async fn persist_caps_occurrence_index_text_and_measures_user_sessions_per_n() {
    const PAYLOAD_CHARS: usize = 80_000;
    const SESSION_COUNTS: [usize; 3] = [4, 8, 16];
    let payload_stem = "m".repeat(PAYLOAD_CHARS);
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let derived = derived_text_for_index(&format!("payload-00-{payload_stem}"));
    assert!(
        derived.len() < PAYLOAD_CHARS,
        "the synthetic payload must exceed the derived index budget"
    );

    let mut next_ordinal = 0u64;
    let mut previous_family_bytes = 0u64;
    for n in SESSION_COUNTS {
        while next_ordinal < n as u64 {
            let session_id = fixture_session(&format!("session.user-sessions.n{next_ordinal}"));
            let text = format!("payload-{next_ordinal:02}-{payload_stem}");
            let (observation, write) =
                fixture_observation_with_text(&session_id, next_ordinal, text);
            Box::pin(persist_fixture(&runtime, observation, write)).await;
            refresh_through(&store, &session_id, 1, 0).await;
            next_ordinal += 1;
        }

        let snapshot = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("profile registered database")
            .read_snapshot()
            .await
            .expect("user-sessions size snapshot");
        let mut rows = snapshot
            .query(
                "SELECT SUM(length(index_text)), MAX(length(index_text)), COUNT(*)
                 FROM session_occurrences",
                (),
            )
            .await
            .expect("index_text size query");
        let row = rows
            .next()
            .await
            .expect("index_text size row")
            .expect("index_text size missing row");
        let stored_index_bytes: i64 = row.get(0).expect("sum index_text");
        let max_index_bytes: i64 = row.get(1).expect("max index_text");
        let occurrence_count: i64 = row.get(2).expect("occurrence count");
        let expected_index_bytes = i64::try_from(n * derived.len()).unwrap();
        let uncapped_index_bytes =
            i64::try_from(n * format!("payload-00-{payload_stem}").len()).unwrap();
        assert_eq!(occurrence_count, i64::try_from(n).unwrap());
        assert_eq!(
            stored_index_bytes, expected_index_bytes,
            "N={n}: index_text must store the derived budget, not the full body"
        );
        assert_eq!(
            max_index_bytes,
            i64::try_from(derived.len()).unwrap(),
            "N={n}: no occurrence may store more than the derived budget"
        );
        assert!(
            stored_index_bytes < uncapped_index_bytes,
            "N={n}: capped index_text ({stored_index_bytes}) must be smaller than the full body ({uncapped_index_bytes})"
        );

        let family_bytes = sqlite_family_bytes(&user_sessions_db_path(tmp.path()));
        assert!(
            family_bytes > previous_family_bytes,
            "N={n}: user-sessions family must grow with sessions ({family_bytes} vs {previous_family_bytes})"
        );
        println!(
            "user-sessions ingest N={n} family_bytes={family_bytes} stored_index_bytes={stored_index_bytes} uncapped_index_bytes={uncapped_index_bytes} unique_payload_bytes={uncapped_index_bytes}"
        );
        previous_family_bytes = family_bytes;
    }
}
