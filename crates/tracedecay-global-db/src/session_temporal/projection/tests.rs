use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{
    AnchorProvenanceRelationV2, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CopyProofV1, DurableObservationV1, LogicalCopyRecordV1, MessageOccurrenceIdV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId, ProviderId,
    RetentionClass, RetrievalAnchorId, RetrievalAnchorRecordV2, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId, TemporalAssertionKindV1, TemporalValidityV1, UtcMicros,
    derive_exact_observation_anchor_id,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCompletionRequestV1,
    SessionRefreshFrontierV1, SessionRefreshProgressV1, SessionRefreshStore,
    SessionRefreshTerminalStateV1, SessionTemporalProjectionBatchV1,
};

use super::super::refresh::SessionRefreshRestartStateV1;
use super::materialize::*;
use crate::session_temporal::GlobalDbSessionTemporalStore;
use crate::tests::harness::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, SessionTemporalFixtureCountV1,
};
use tracedecay_runtime_core::db::engine::{Executor, TestConnection, params};

fn fixture_session(value: &str) -> SessionId {
    SessionId::new(value).unwrap()
}

fn temporal_store(runtime: &HostAdmissionTestRuntimeV1) -> GlobalDbSessionTemporalStore<'_> {
    runtime
        .session_temporal_store_for_test(HostAdmissionScope::Profile)
        .expect("registered profile session-temporal store")
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
    lineage: Option<(AnchorProvenanceRelationV2, RetrievalAnchorId)>,
    include_parent: bool,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    let provider = ProviderId::new(format!("projector-test-{ordinal}")).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
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
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": format!("projector {ordinal}")}),
            model: Some("model.projector".to_owned()),
            timestamp: Some(1_750_000_000 + i64::try_from(ordinal).unwrap()),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
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
        fixture_receipt(&format!("receipt.projector.{ordinal}"), &payload),
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
    let anchor = tracedecay_store::build_observation_retrieval_anchor_v2(
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
    let anchor: RetrievalAnchorRecordV2 = serde_json::from_value(anchor_json).unwrap();
    let anchored = AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap();
    (observation, anchored)
}

fn fixture_goal_observation() -> (DurableObservationV1, AnchoredObservationWrite) {
    let record_id = ObservationId::new("record.goal.fixture").unwrap();
    let encoded = include_str!(
        "../../../../../tests/fixtures/provider_normalization/codex/thread_goal_updated.expected_envelope.json"
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
    let anchor = tracedecay_store::build_observation_retrieval_anchor_v2(
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
            valid_at: UtcMicros(1_783_500_569)
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
            Some((AnchorProvenanceRelationV2::Supersedes, first_anchor)),
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
        .complete_session_refresh(request.clone())
        .await
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    assert_eq!(
        store.complete_session_refresh(request).await.unwrap(),
        receipt
    );
    for (kind, expected) in [
        (SessionTemporalFixtureCountV1::ProjectionReceipts, 1),
        (SessionTemporalFixtureCountV1::Occurrences, 2),
        (SessionTemporalFixtureCountV1::LogicalCopyEdges, 0),
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
        Some((AnchorProvenanceRelationV2::CopiedFrom, first_anchor)),
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
            .map(|anchor| (AnchorProvenanceRelationV2::Supersedes, anchor));
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
fn assertion_identity_includes_the_object_anchor() {
    let session_id = fixture_session("session.projector.assertion-identity");
    let (first, _) = fixture_observation(&session_id, 0, None, false);
    let (second, _) = fixture_observation(&session_id, 1, None, false);
    let occurrence_id = MessageOccurrenceIdV1::derive(
        first.observation_id(),
        tracedecay_domain::ProjectionOutputOrdinalV1::new(0),
    );
    let first_anchor =
        derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
    let second_anchor =
        derive_exact_observation_anchor_id(second.scope(), second.observation_id()).unwrap();
    let first_id = derived_temporal_assertion_id(
        &occurrence_id,
        TemporalAssertionKindV1::Supports,
        &first_anchor,
    );
    let second_id = derived_temporal_assertion_id(
        &occurrence_id,
        TemporalAssertionKindV1::Supports,
        &second_anchor,
    );
    assert_ne!(first_id, second_id);
    assert!(first_id.starts_with("sha256:"));
    assert_eq!(first_id.len(), 71);
}

#[tokio::test]
async fn parent_resolver_rejects_ambiguous_session_message_ids() {
    let mut resolver = ParentMessageResolver::default();
    resolver.register("message.shared", "occurrence.a");
    resolver.register("message.shared", "occurrence.b");
    let error = resolver
        .reject_ambiguity("test parent ambiguity")
        .expect_err("duplicate message ids must be rejected");
    let detail = format!("{error:?}");
    assert!(
        detail.contains("message.shared") || detail.contains("resolves to 2 occurrences"),
        "{detail}"
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

    let resolver = canonical_parent_message_resolver(
        &*connection,
        session_id.as_str(),
        10001,
        "test paged parent resolver",
    )
    .await
    .unwrap();

    assert!(resolver.resolve("message.projector.0").is_some());
}

#[tokio::test]
async fn persisted_copy_edge_retains_bitemporality_and_rejects_forged_assertion_ids() {
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
        Some((AnchorProvenanceRelationV2::Supersedes, first_anchor)),
        true,
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
    // The materializer no longer derives a copy edge from a parent-message
    // reply — only a re-emission of the same logical message is a logical copy.
    // Explicit typed copy records stay the persist-side authority, so this test
    // drives the retained-copy persist path with the canonical parent-message
    // proof for the retained pair.
    assert!(batch.copies().is_empty());
    let copy = LogicalCopyRecordV1 {
        occurrence_id: batch.occurrences()[1].occurrence_id.clone(),
        copied_from_occurrence_id: batch.occurrences()[0].occurrence_id.clone(),
        proof: CopyProofV1::ParentMessageLinkage {
            source_occurrence_id: batch.occurrences()[0].occurrence_id.clone(),
            parent_message_id: tracedecay_domain::MessageId::new("message.projector.0").unwrap(),
        },
        knowledge_at: batch.occurrences()[1].knowledge_at,
        valid_time: batch.occurrences()[1].valid_time,
    };
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
    assert_eq!(batch.item_count(), 4);

    let mut forged = batch.assertions()[0].clone();
    forged.assertion_id =
        tracedecay_domain::TemporalAssertionIdV1::new("assertion.forged").unwrap();
    let forged_batch = SessionTemporalProjectionBatchV1::new(
        batch.session_id().clone(),
        batch.generation(),
        batch.watermarks().clone(),
        batch.occurrences().to_vec(),
        batch.copies().to_vec(),
        vec![forged],
    )
    .unwrap()
    .with_checkpoint(
        batch.batch_ordinal(),
        batch.source_through(),
        batch.projection_through(),
    )
    .unwrap();
    let forged_error = store
        .persist_session_refresh_projection_batch(progress.clone(), forged_batch)
        .await
        .expect_err("forged assertion ids must be rejected");
    let forged_detail = format!("{forged_error:?}");
    assert!(
        forged_detail.contains("not canonical") || forged_detail.contains("assertion temporal"),
        "{forged_detail}"
    );

    store
        .persist_session_refresh_projection_batch(progress, batch.clone())
        .await
        .unwrap();
    let (knowledge_at, valid_time) = runtime
        .session_temporal_copy_edge_for_test(HostAdmissionScope::Profile, &session_id)
        .await
        .unwrap()
        .expect("copy edge");
    assert_eq!(knowledge_at, batch.copies()[0].knowledge_at.0);
    assert_eq!(valid_time, batch.copies()[0].valid_time);
}

#[tokio::test]
async fn multi_batch_refresh_progress_survives_restart_under_guard() {
    let tmp = TempDir::new().unwrap();
    let session_id = fixture_session("session.projector.multi-batch-guard");
    let operation_id;
    {
        let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
            .await
            .unwrap();
        let store = temporal_store(&runtime);
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
