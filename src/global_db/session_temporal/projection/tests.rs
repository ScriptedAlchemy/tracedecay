use libsql::params;
use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{
    AnchorProvenanceRelationV2, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CopyProofV1, DurableObservationV1, MessageOccurrenceIdV1, ObservationId,
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
    SessionRefreshFrontierV1, SessionRefreshTerminalStateV1, SessionTemporalProjectionBatchV1,
};

use super::super::refresh::SessionRefreshRestartStateV1;
use super::materialize::*;
use crate::global_db::GlobalDb;
use crate::store::GlobalDbObservationStore;

fn fixture_session(value: &str) -> SessionId {
    SessionId::new(value).unwrap()
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

async fn persist_fixture(
    db: &GlobalDb,
    observation: DurableObservationV1,
    anchored: AnchoredObservationWrite,
) {
    let store = GlobalDbObservationStore::new(db);
    store.persist_observation(anchored).await.unwrap();
    store
        .project_observation(observation.observation_id())
        .await
        .unwrap();
}

async fn scalar(db: &GlobalDb, sql: &str) -> i64 {
    let mut rows = db.read_connection().query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

#[tokio::test]
async fn relation_batch_persists_restarts_and_completes_without_duplicates() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("global.db");
    let session_id = fixture_session("session.projector.relation-restart");
    let operation_id;
    {
        let db = GlobalDb::open_at(&path).await.unwrap();
        let (first, first_write) = fixture_observation(&session_id, 0, None, false);
        let first_anchor =
            derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
        Box::pin(persist_fixture(&db, first, first_write)).await;
        let (second, second_write) = fixture_observation(
            &session_id,
            1,
            Some((AnchorProvenanceRelationV2::Supersedes, first_anchor)),
            true,
        );
        Box::pin(persist_fixture(&db, second, second_write)).await;
        let begin = db
            .begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                SessionRefreshFrontierV1::new(2, 0).unwrap(),
            ))
            .await
            .unwrap();
        operation_id = begin.operation_id().clone();
        let recovery = db
            .session_refresh_recovery_result(&session_id)
            .await
            .unwrap()
            .unwrap();
        let (progress, batch) = db
            .materialize_session_temporal_refresh_batch_result(&recovery)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(batch.occurrences().len(), 2);
        assert_eq!(batch.copies().len(), 1);
        assert_eq!(batch.assertions().len(), 1);
        assert_eq!(batch.item_count(), 4);
        assert_eq!(progress.committed_records(), 4);
        assert_eq!(progress.coverage().visible, 4);
        db.persist_session_refresh_projection_batch_result(progress, batch)
            .await
            .unwrap();
    }

    let db = GlobalDb::open_at(&path).await.unwrap();
    let recovery = db
        .session_refresh_recovery_result(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovery.restart_state(),
        SessionRefreshRestartStateV1::ReadyToComplete
    );
    assert!(
        db.materialize_session_temporal_refresh_batch_result(&recovery)
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
    let receipt = db
        .complete_session_refresh_result(request.clone())
        .await
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    assert_eq!(
        db.complete_session_refresh_result(request).await.unwrap(),
        receipt
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM session_occurrences").await,
        2
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM session_logical_copy_edges").await,
        1
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM session_assertions").await,
        1
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM session_refresh_receipts").await,
        1
    );
}

#[tokio::test]
async fn copied_from_lineage_is_not_auto_emitted_by_materializer() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("global.db");
    let db = GlobalDb::open_at(&path).await.unwrap();
    let session_id = fixture_session("session.projector.copied-from");
    let (first, first_write) = fixture_observation(&session_id, 0, None, false);
    let first_anchor =
        derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
    Box::pin(persist_fixture(&db, first, first_write)).await;
    let (second, second_write) = fixture_observation(
        &session_id,
        1,
        Some((AnchorProvenanceRelationV2::CopiedFrom, first_anchor)),
        false,
    );
    Box::pin(persist_fixture(&db, second, second_write)).await;
    db.begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
        session_id.clone(),
        SessionRefreshFrontierV1::new(2, 0).unwrap(),
    ))
    .await
    .unwrap();
    let recovery = db
        .session_refresh_recovery_result(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = db
        .materialize_session_temporal_refresh_batch_result(&recovery)
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
    let path = tmp.path().join("global.db");
    let db = GlobalDb::open_at(&path).await.unwrap();
    let session_id = fixture_session("session.projector.derived-limit");
    for ordinal in 0..501 {
        let (observation, write) = fixture_observation(&session_id, ordinal, None, ordinal > 0);
        Box::pin(persist_fixture(&db, observation, write)).await;
    }
    db.begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
        session_id.clone(),
        SessionRefreshFrontierV1::new(501, 0).unwrap(),
    ))
    .await
    .unwrap();
    let recovery = db
        .session_refresh_recovery_result(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (first_progress, first_batch) = db
        .materialize_session_temporal_refresh_batch_result(&recovery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_batch.occurrences().len(), 500);
    assert_eq!(first_batch.copies().len(), 499);
    assert_eq!(first_batch.item_count(), 999);
    assert_eq!(first_progress.frontier().committed_through(), 500);
    assert_eq!(first_progress.committed_records(), 999);
    db.persist_session_refresh_projection_batch_result(first_progress, first_batch)
        .await
        .unwrap();

    let recovery = db
        .session_refresh_recovery_result(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (second_progress, second_batch) = db
        .materialize_session_temporal_refresh_batch_result(&recovery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second_batch.occurrences().len(), 1);
    assert_eq!(second_batch.copies().len(), 1);
    assert_eq!(second_batch.item_count(), 2);
    assert_eq!(second_progress.frontier().committed_through(), 501);
    assert_eq!(second_progress.committed_records(), 1001);
    db.persist_session_refresh_projection_batch_result(second_progress, second_batch)
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
async fn materialize_persists_copy_bitemporality_and_rejects_forged_assertion_ids() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("global.db");
    let db = GlobalDb::open_at(&path).await.unwrap();
    let session_id = fixture_session("session.projector.copy-bitemporal");
    let (first, first_write) = fixture_observation(&session_id, 0, None, false);
    let first_anchor =
        derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
    Box::pin(persist_fixture(&db, first, first_write)).await;
    let (second, second_write) = fixture_observation(
        &session_id,
        1,
        Some((AnchorProvenanceRelationV2::Supersedes, first_anchor)),
        true,
    );
    Box::pin(persist_fixture(&db, second, second_write)).await;
    db.begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
        session_id.clone(),
        SessionRefreshFrontierV1::new(2, 0).unwrap(),
    ))
    .await
    .unwrap();
    let recovery = db
        .session_refresh_recovery_result(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = db
        .materialize_session_temporal_refresh_batch_result(&recovery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(batch.copies().len(), 1);
    assert_eq!(
        batch.copies()[0].valid_time,
        batch.occurrences()[1].valid_time
    );
    assert_eq!(
        batch.copies()[0].knowledge_at,
        batch.occurrences()[1].knowledge_at
    );
    assert!(matches!(
        batch.copies()[0].proof,
        CopyProofV1::ParentMessageLinkage { .. }
    ));

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
    let forged_error = db
        .persist_session_refresh_projection_batch_result(progress.clone(), forged_batch)
        .await
        .expect_err("forged assertion ids must be rejected");
    let forged_detail = format!("{forged_error:?}");
    assert!(
        forged_detail.contains("not canonical") || forged_detail.contains("assertion temporal"),
        "{forged_detail}"
    );

    db.persist_session_refresh_projection_batch_result(progress, batch.clone())
        .await
        .unwrap();
    let mut rows = db
        .read_connection()
        .query(
            "SELECT knowledge_at, valid_time_json FROM session_logical_copy_edges
                 WHERE session_id = ?1",
            params![session_id.as_str()],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let knowledge_at: i64 = row.get(0).unwrap();
    let valid_time: String = row.get(1).unwrap();
    assert_eq!(knowledge_at, batch.copies()[0].knowledge_at.0);
    assert_eq!(
        serde_json::from_str::<TemporalValidityV1>(&valid_time).unwrap(),
        batch.copies()[0].valid_time
    );
}

#[tokio::test]
async fn multi_batch_refresh_progress_survives_restart_under_guard() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("global.db");
    let session_id = fixture_session("session.projector.multi-batch-guard");
    let operation_id;
    {
        let db = GlobalDb::open_at(&path).await.unwrap();
        for ordinal in 0..3 {
            let (observation, write) = fixture_observation(&session_id, ordinal, None, ordinal > 0);
            Box::pin(persist_fixture(&db, observation, write)).await;
        }
        let begin = db
            .begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                SessionRefreshFrontierV1::new(3, 0).unwrap(),
            ))
            .await
            .unwrap();
        operation_id = begin.operation_id().clone();
        let recovery = db
            .session_refresh_recovery_result(&session_id)
            .await
            .unwrap()
            .unwrap();
        let (progress, batch) = db
            .materialize_session_temporal_refresh_batch_result(&recovery)
            .await
            .unwrap()
            .unwrap();
        assert!(batch.item_count() > 0);
        assert!(progress.frontier().committed_through() > 0);
        db.persist_session_refresh_projection_batch_result(progress, batch)
            .await
            .unwrap();
    }

    let db = GlobalDb::open_at(&path).await.unwrap();
    let recovery = db
        .session_refresh_recovery_result(&session_id)
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
    if let Some((progress, batch)) = db
        .materialize_session_temporal_refresh_batch_result(&recovery)
        .await
        .unwrap()
    {
        db.persist_session_refresh_projection_batch_result(progress, batch)
            .await
            .unwrap();
    }
    let recovery = db
        .session_refresh_recovery_result(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovery.restart_state(),
        SessionRefreshRestartStateV1::ReadyToComplete
    );
    let progress = recovery.progress().unwrap();
    let receipt = db
        .complete_session_refresh_result(
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
        scalar(&db, "SELECT COUNT(*) FROM session_refresh_progress").await,
        progress.committed_batches() as i64
    );
}
