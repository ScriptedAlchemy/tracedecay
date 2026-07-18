use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::store::{GlobalDbObservationStore, GlobalDbSessionTemporalStore};
use tracedecay_domain::{
    AnchorProvenanceRelationV2, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CopyProofV1, DurableObservationV1, MessageId, MessageOccurrenceIdV1, MessageOccurrenceRecordV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId,
    ProjectionOutputOrdinalV1, ProviderId, RetentionClass, RetrievalAnchorId,
    RetrievalAnchorRecordV2, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId,
    SessionProjectionGenerationV1, TemporalAssertionKindV1, TemporalAssertionRecordV1,
    TemporalValidityV1, UtcMicros, derive_exact_observation_anchor_id,
};
use tracedecay_store::{
    AnchoredObservationWrite, MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS,
    ObservationProjectionStore, ObservationStore, ObservationWrite, SessionFrozenWatermarksV1,
    SessionGenerationActivationRequestV1, SessionGenerationRebuildDispositionV1,
    SessionGenerationRebuildRequestV1, SessionStoreError, SessionTemporalCapabilitiesV1,
    SessionTemporalCapabilityV1, SessionTemporalProjectionBatchDispositionV1,
    SessionTemporalProjectionBatchV1, SessionTemporalProjectionStore, SessionTemporalSnapshotV1,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use crate::common::{isolated_lcm_db_path, open_lcm_db};

fn session(value: &str) -> SessionId {
    SessionId::new(value).unwrap()
}

fn generation(value: u64) -> SessionProjectionGenerationV1 {
    SessionProjectionGenerationV1::new(value).unwrap()
}

fn watermarks(active_generation: u64, source_frontier: u64) -> SessionFrozenWatermarksV1 {
    SessionFrozenWatermarksV1::new(
        generation(active_generation),
        source_frontier,
        source_frontier,
        0,
    )
}

fn snapshot(
    session_id: &SessionId,
    active_generation: u64,
    source_frontier: u64,
) -> SessionTemporalSnapshotV1 {
    SessionTemporalSnapshotV1::new(
        session_id.clone(),
        UtcMicros(100),
        watermarks(active_generation, source_frontier),
        SessionTemporalCapabilitiesV1::new([
            SessionTemporalCapabilityV1::FrozenWatermarks,
            SessionTemporalCapabilityV1::GenerationRebuild,
        ]),
    )
}

fn receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.temporal-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn observation(session_id: &SessionId, ordinal: u64, text: &str) -> DurableObservationV1 {
    observation_with_message_ids(
        session_id,
        ordinal,
        text,
        &format!("message.temporal.{ordinal}"),
        (ordinal > 0).then(|| format!("message.temporal.{}", ordinal - 1)),
    )
}

fn observation_with_message_ids(
    session_id: &SessionId,
    ordinal: u64,
    text: &str,
    message_id: &str,
    parent_message_id: Option<String>,
) -> DurableObservationV1 {
    let provider = ProviderId::new(format!("temporal-test-{ordinal}")).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
    let record_id = ObservationId::new(format!("record.temporal.{ordinal}")).unwrap();
    let mut relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_thread_id(ObservationId::new("thread.temporal").unwrap())
        .with_turn_id(ObservationId::new("turn.temporal").unwrap())
        .with_message_id(ObservationId::new(message_id).unwrap())
        .with_agent_id(ObservationId::new("agent.temporal").unwrap());
    if let Some(parent_message_id) = parent_message_id {
        relations =
            relations.with_parent_message_id(ObservationId::new(parent_message_id).unwrap());
    }
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": text}),
            model: Some("model.temporal".to_owned()),
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
    DurableObservationV1::new(
        identity,
        receipt(&format!("receipt.temporal.{ordinal}"), &payload),
        RetentionClass::new("retention.temporal-test").unwrap(),
        payload,
    )
    .unwrap()
}

fn anchored_write(observation: DurableObservationV1) -> AnchoredObservationWrite {
    anchored_write_with_lineage(observation, None, None)
}

fn anchored_write_with_lineage(
    observation: DurableObservationV1,
    lineage: Option<(AnchorProvenanceRelationV2, RetrievalAnchorId)>,
    occurred_at: Option<i64>,
) -> AnchoredObservationWrite {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, None, next_cursor).unwrap();
    let projection_generation = ProjectionGenerationId::new("projection.temporal-test.v1").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "temporal-test")
            .unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
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
    if let Some(valid_at) = occurred_at {
        anchor_json["occurred_at"] = json!({
            "start": valid_at,
            "end": valid_at,
        });
    }
    let anchor: RetrievalAnchorRecordV2 = serde_json::from_value(anchor_json).unwrap();
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

async fn persist_observation(
    db: &GlobalDb,
    session_id: &SessionId,
    ordinal: u64,
    text: &str,
) -> DurableObservationV1 {
    let observation = observation(session_id, ordinal, text);
    let store = GlobalDbObservationStore::new(db);
    store
        .persist_observation(anchored_write(observation.clone()))
        .await
        .unwrap();
    store
        .project_observation(observation.observation_id())
        .await
        .unwrap();
    observation
}

async fn persist_custom_observation(
    db: &GlobalDb,
    observation: DurableObservationV1,
) -> DurableObservationV1 {
    let store = GlobalDbObservationStore::new(db);
    store
        .persist_observation(anchored_write(observation.clone()))
        .await
        .unwrap();
    store
        .project_observation(observation.observation_id())
        .await
        .unwrap();
    observation
}

async fn persist_custom_observation_with_lineage(
    db: &GlobalDb,
    observation: DurableObservationV1,
    relation: AnchorProvenanceRelationV2,
    object_anchor_id: RetrievalAnchorId,
) -> DurableObservationV1 {
    let store = GlobalDbObservationStore::new(db);
    store
        .persist_observation(anchored_write_with_lineage(
            observation.clone(),
            Some((relation, object_anchor_id)),
            None,
        ))
        .await
        .unwrap();
    store
        .project_observation(observation.observation_id())
        .await
        .unwrap();
    observation
}

async fn persist_observation_with_lineage(
    db: &GlobalDb,
    session_id: &SessionId,
    ordinal: u64,
    text: &str,
    relation: AnchorProvenanceRelationV2,
    object_anchor_id: RetrievalAnchorId,
    valid_at: Option<i64>,
) -> DurableObservationV1 {
    let observation = observation(session_id, ordinal, text);
    let store = GlobalDbObservationStore::new(db);
    store
        .persist_observation(anchored_write_with_lineage(
            observation.clone(),
            Some((relation, object_anchor_id)),
            valid_at,
        ))
        .await
        .unwrap();
    store
        .project_observation(observation.observation_id())
        .await
        .unwrap();
    observation
}

fn occurrence(
    session_id: &SessionId,
    observation: &DurableObservationV1,
) -> MessageOccurrenceRecordV1 {
    let output_ordinal = ProjectionOutputOrdinalV1::new(0);
    serde_json::from_value(json!({
        "occurrence_id": MessageOccurrenceIdV1::derive(
            observation.observation_id(),
            output_ordinal,
        ),
        "source_observation_id": observation.observation_id(),
        "projection_output_ordinal": output_ordinal,
        "retrieval_anchor_id": derive_exact_observation_anchor_id(
            observation.scope(),
            observation.observation_id(),
        ).unwrap(),
        "session_id": session_id,
        "thread_id": "thread.temporal",
        "thread_grouping": {"kind": "provider_native"},
        "turn_id": "turn.temporal",
        "turn_grouping": {"kind": "provider_native"},
        "message_id": format!(
            "message.temporal.{}",
            observation.identity().position().start()
        ),
        "agent_id": "agent.temporal",
        "role": "assistant",
        "knowledge_at": 1,
        "valid_time": {"kind": "unknown"},
        "evidence": {
            "authority": "canonical_observation",
            "evidence_class": "observed",
            "source_anchor_id": derive_exact_observation_anchor_id(
                observation.scope(),
                observation.observation_id(),
            ).unwrap(),
            "sanitization_receipt": observation.receipt().receipt()
        }
    }))
    .unwrap()
}

fn occurrence_with_message_id(
    session_id: &SessionId,
    observation: &DurableObservationV1,
    message_id: &str,
) -> MessageOccurrenceRecordV1 {
    let mut occurrence = occurrence(session_id, observation);
    occurrence.message_id = Some(MessageId::new(message_id).unwrap());
    occurrence
}

fn copy(
    target: &MessageOccurrenceRecordV1,
    source: &MessageOccurrenceRecordV1,
) -> tracedecay_domain::LogicalCopyRecordV1 {
    tracedecay_domain::LogicalCopyRecordV1 {
        occurrence_id: target.occurrence_id.clone(),
        copied_from_occurrence_id: source.occurrence_id.clone(),
        proof: CopyProofV1::ProviderLinkage {
            source_occurrence_id: source.occurrence_id.clone(),
            provider_record_id: ObservationId::new(format!(
                "record.temporal.{}",
                source.projection_output_ordinal.value()
            ))
            .unwrap(),
        },
    }
}

fn parent_message_copy(
    target: &MessageOccurrenceRecordV1,
    source: &MessageOccurrenceRecordV1,
) -> tracedecay_domain::LogicalCopyRecordV1 {
    tracedecay_domain::LogicalCopyRecordV1 {
        occurrence_id: target.occurrence_id.clone(),
        copied_from_occurrence_id: source.occurrence_id.clone(),
        proof: CopyProofV1::ParentMessageLinkage {
            source_occurrence_id: source.occurrence_id.clone(),
            parent_message_id: source.message_id.clone().expect("source message id"),
        },
    }
}

fn explicit_anchor_copy(
    target: &MessageOccurrenceRecordV1,
    source: &MessageOccurrenceRecordV1,
) -> tracedecay_domain::LogicalCopyRecordV1 {
    tracedecay_domain::LogicalCopyRecordV1 {
        occurrence_id: target.occurrence_id.clone(),
        copied_from_occurrence_id: source.occurrence_id.clone(),
        proof: CopyProofV1::ExplicitAnchorAssertion {
            source_occurrence_id: source.occurrence_id.clone(),
            assertion_anchor_id: source.retrieval_anchor_id.clone(),
        },
    }
}

fn assertion(
    subject: &MessageOccurrenceRecordV1,
    object: &MessageOccurrenceRecordV1,
) -> TemporalAssertionRecordV1 {
    assertion_with_kind(TemporalAssertionKindV1::Supersedes, subject, object)
}

fn assertion_with_kind(
    kind: TemporalAssertionKindV1,
    subject: &MessageOccurrenceRecordV1,
    object: &MessageOccurrenceRecordV1,
) -> TemporalAssertionRecordV1 {
    serde_json::from_value(json!({
        "assertion_id": format!("assertion.{}", subject.occurrence_id),
        "kind": kind.as_str(),
        "subject_anchor_id": subject.retrieval_anchor_id,
        "object_anchor_id": object.retrieval_anchor_id,
        "knowledge_at": subject.knowledge_at,
        "valid_time": subject.valid_time,
        "evidence": {
            "authority": "explicit_anchor_assertion",
            "evidence_class": subject.evidence.evidence_class,
            "source_anchor_id": subject.retrieval_anchor_id,
            "sanitization_receipt": subject.evidence.sanitization_receipt
        }
    }))
    .unwrap()
}

fn batch(
    session_id: &SessionId,
    candidate_generation: u64,
    source_frontier: u64,
    occurrences: Vec<MessageOccurrenceRecordV1>,
    copies: Vec<tracedecay_domain::LogicalCopyRecordV1>,
    assertions: Vec<TemporalAssertionRecordV1>,
) -> SessionTemporalProjectionBatchV1 {
    SessionTemporalProjectionBatchV1::new(
        session_id.clone(),
        generation(candidate_generation),
        watermarks(1, source_frontier),
        occurrences,
        copies,
        assertions,
    )
    .unwrap()
}

async fn scalar(path: &std::path::Path, sql: &str) -> i64 {
    let raw_db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn rows(path: &std::path::Path, sql: &str) -> Vec<String> {
    let raw_db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    let mut result = Vec::new();
    let mut rows = conn.query(sql, ()).await.unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        result.push(row.get(0).unwrap());
    }
    result
}

async fn begin_candidate(
    store: &GlobalDbSessionTemporalStore<'_>,
    session_id: &SessionId,
    candidate_generation: u64,
    source_frontier: u64,
) -> SessionGenerationRebuildDispositionV1 {
    store
        .begin_session_generation_rebuild(
            SessionGenerationRebuildRequestV1::new(
                session_id.clone(),
                generation(candidate_generation),
                snapshot(session_id, 1, source_frontier),
            )
            .unwrap(),
        )
        .await
        .unwrap()
        .disposition()
}

#[tokio::test]
async fn first_session_rebuild_bootstraps_active_generation_under_writer_authority() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.bootstrap");
    let store = GlobalDbSessionTemporalStore::new(&db);

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

#[tokio::test]
async fn batch_receipts_require_contiguous_ordinals_and_replay_exactly() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.receipts");
    let persisted = persist_observation(&db, &session_id, 0, "receipt").await;
    let projected = occurrence(&session_id, &persisted);
    let store = GlobalDbSessionTemporalStore::new(&db);
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
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.untrusted");
    let first = persist_observation(&db, &session_id, 0, "first").await;
    let second = persist_observation(&db, &session_id, 1, "second").await;
    let canonical = occurrence(&session_id, &first);
    let store = GlobalDbSessionTemporalStore::new(&db);
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
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.atomic");
    let persisted = persist_observation(&db, &session_id, 0, "atomic").await;
    let persisted_occurrence = occurrence(&session_id, &persisted);
    let missing = occurrence(&session_id, &observation(&session_id, 99, "not persisted"));
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 1).await;

    let result = store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            1,
            vec![persisted_occurrence.clone()],
            vec![copy(&persisted_occurrence, &missing)],
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
        "session_agent_hierarchy_edges",
        "session_logical_copy_edges",
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
async fn only_explicit_typed_copy_proof_persists_copy_edges() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.copy");
    let first = persist_observation(&db, &session_id, 0, "same text").await;
    let second = persist_observation(&db, &session_id, 1, "same text").await;
    let first = occurrence(&session_id, &first);
    let second = occurrence(&session_id, &second);
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 2).await;

    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone(), second.clone()],
            vec![],
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_logical_copy_edges").await,
        0
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_occurrences_fts
             WHERE session_occurrences_fts MATCH 'same'"
        )
        .await,
        2
    );

    let mut forged = copy(&second, &first);
    forged.proof = CopyProofV1::ProviderLinkage {
        source_occurrence_id: first.occurrence_id.clone(),
        provider_record_id: ObservationId::new("provider.copy.nonexistent").unwrap(),
    };
    assert!(
        store
            .persist_session_temporal_projection_batch(
                batch(&session_id, 2, 2, vec![], vec![forged], vec![])
                    .with_checkpoint(1, 2, 2)
                    .unwrap(),
            )
            .await
            .is_err()
    );
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_logical_copy_edges").await,
        0
    );

    store
        .persist_session_temporal_projection_batch(
            batch(
                &session_id,
                2,
                2,
                vec![],
                vec![copy(&second, &first)],
                vec![],
            )
            .with_checkpoint(1, 2, 2)
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_logical_copy_edges").await,
        1
    );
}

#[tokio::test]
async fn each_typed_assertion_relation_authorizes_only_its_matching_kind() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.typed-assertions");
    let mut occurrences = Vec::new();
    let mut assertions = Vec::new();
    for (index, (kind, relation)) in [
        (
            TemporalAssertionKindV1::Corrects,
            AnchorProvenanceRelationV2::Corrects,
        ),
        (
            TemporalAssertionKindV1::Contradicts,
            AnchorProvenanceRelationV2::Contradicts,
        ),
        (
            TemporalAssertionKindV1::Supersedes,
            AnchorProvenanceRelationV2::Supersedes,
        ),
        (
            TemporalAssertionKindV1::Supports,
            AnchorProvenanceRelationV2::Supports,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let object_ordinal = u64::try_from(index * 2).unwrap();
        let subject_ordinal = object_ordinal + 1;
        let object_observation =
            persist_observation(&db, &session_id, object_ordinal, "object").await;
        let object = occurrence(&session_id, &object_observation);
        let subject_observation = persist_observation_with_lineage(
            &db,
            &session_id,
            subject_ordinal,
            "subject",
            relation,
            object.retrieval_anchor_id.clone(),
            None,
        )
        .await;
        let subject = occurrence(&session_id, &subject_observation);
        assertions.push(assertion_with_kind(kind, &subject, &object));
        occurrences.extend([object, subject]);
    }
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 8).await;

    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            8,
            occurrences,
            vec![],
            assertions,
        ))
        .await
        .unwrap();

    assert_eq!(
        rows(
            &path,
            "SELECT assertion_kind FROM session_assertions ORDER BY assertion_kind"
        )
        .await,
        vec!["contradicts", "corrects", "supersedes", "supports"]
    );
}

#[tokio::test]
async fn mismatched_typed_assertion_relation_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.mismatched-assertion");
    let object_observation = persist_observation(&db, &session_id, 0, "object").await;
    let object = occurrence(&session_id, &object_observation);
    let subject_observation = persist_observation_with_lineage(
        &db,
        &session_id,
        1,
        "subject",
        AnchorProvenanceRelationV2::Supports,
        object.retrieval_anchor_id.clone(),
        None,
    )
    .await;
    let subject = occurrence(&session_id, &subject_observation);
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 2).await;

    assert!(matches!(
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                2,
                2,
                vec![object.clone(), subject.clone()],
                vec![],
                vec![assertion_with_kind(
                    TemporalAssertionKindV1::Contradicts,
                    &subject,
                    &object,
                )],
            ))
            .await,
        Err(SessionStoreError::Storage { .. })
    ));
}

#[tokio::test]
async fn parent_message_without_typed_assertion_lineage_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.parent-only-assertion");
    let object = occurrence(
        &session_id,
        &persist_observation(&db, &session_id, 0, "object").await,
    );
    let subject = occurrence(
        &session_id,
        &persist_observation(&db, &session_id, 1, "subject").await,
    );
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 2).await;

    assert!(matches!(
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                2,
                2,
                vec![object.clone(), subject.clone()],
                vec![],
                vec![assertion_with_kind(
                    TemporalAssertionKindV1::Corrects,
                    &subject,
                    &object,
                )],
            ))
            .await,
        Err(SessionStoreError::Storage { .. })
    ));
}

#[tokio::test]
async fn batches_reject_cross_session_and_cross_generation_ownership() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.owner");
    let observation = persist_observation(&db, &session_id, 0, "owner").await;
    let store = GlobalDbSessionTemporalStore::new(&db);
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
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.replay");
    let observation = persist_observation(&db, &session_id, 0, "replay").await;
    let occurrence = occurrence(&session_id, &observation);
    let store = GlobalDbSessionTemporalStore::new(&db);
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
async fn cancelled_candidates_and_stale_source_frontiers_reject_writes() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.cancelled");
    let first = persist_observation(&db, &session_id, 0, "within frontier").await;
    let stale = persist_observation(&db, &session_id, 1, "past frontier").await;
    let store = GlobalDbSessionTemporalStore::new(&db);
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

    let raw_db = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'cancelled', completed_at = created_at
         WHERE session_id = ?1 AND generation = 2",
        libsql::params![session_id.as_str()],
    )
    .await
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
async fn incremental_and_one_shot_rebuilds_have_identical_bytes_and_order() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.parity");
    let first = occurrence(
        &session_id,
        &persist_observation(&db, &session_id, 0, "first").await,
    );
    let second = occurrence(
        &session_id,
        &persist_observation_with_lineage(
            &db,
            &session_id,
            1,
            "second",
            AnchorProvenanceRelationV2::Supersedes,
            first.retrieval_anchor_id.clone(),
            None,
        )
        .await,
    );
    let edge = copy(&second, &first);
    let assertion = assertion(&second, &first);
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 2).await;
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
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            3,
            2,
            vec![first],
            vec![],
            vec![],
        ))
        .await
        .unwrap();
    store
        .persist_session_temporal_projection_batch(
            batch(&session_id, 3, 2, vec![second], vec![edge], vec![assertion])
                .with_checkpoint(1, 2, 2)
                .unwrap(),
        )
        .await
        .unwrap();

    let canonical_rows = |generation: u64| {
        format!(
            "SELECT json_object(
                'occurrence_id', occurrence_id,
                'source_observation_id', source_observation_id,
                'projection_output_ordinal', projection_output_ordinal,
                'retrieval_anchor_id', retrieval_anchor_id,
                'thread_id', thread_id,
                'thread_grouping_json', json(thread_grouping_json),
                'turn_id', turn_id,
                'turn_grouping_json', json(turn_grouping_json),
                'message_id', message_id,
                'agent_id', agent_id,
                'role', role,
                'knowledge_at', knowledge_at,
                'valid_time_json', json(valid_time_json),
                'evidence_json', json(evidence_json),
                'snippet_text', snippet_text,
                'index_text', index_text
             )
             FROM session_occurrences
             WHERE generation = {generation}
             ORDER BY knowledge_at, occurrence_id"
        )
    };
    assert_eq!(
        rows(&path, &canonical_rows(2)).await,
        rows(&path, &canonical_rows(3)).await
    );
    for projection in [
        "SELECT occurrence_id || ':' || copied_from_occurrence_id || ':' ||
                proof_json || ':' || created_at
         FROM session_logical_copy_edges
         WHERE generation = {generation}
         ORDER BY occurrence_id, copied_from_occurrence_id",
        "SELECT assertion_id || ':' || assertion_kind || ':' ||
                subject_anchor_id || ':' || object_anchor_id || ':' ||
                valid_time_json || ':' || evidence_json
         FROM session_assertions
         WHERE generation = {generation}
         ORDER BY assertion_id",
        "SELECT entity_kind || ':' || entity_id || ':' ||
                COALESCE(current_assertion_id, '') || ':' ||
                COALESCE(current_occurrence_id, '') || ':' || coverage_json
         FROM session_current_entities
         WHERE generation = {generation}
         ORDER BY entity_kind, entity_id",
        "SELECT turn_id || ':' || occurrence_id || ':' || ordinal
         FROM session_turn_members
         WHERE generation = {generation}
         ORDER BY turn_id, ordinal, occurrence_id",
        "SELECT thread_id || ':' || grouping_provenance || ':' || created_at
         FROM session_threads
         WHERE generation = {generation}
         ORDER BY thread_id",
        "SELECT turn_id || ':' || ordinal || ':' || grouping_provenance || ':' || created_at
         FROM session_turns
         WHERE generation = {generation}
         ORDER BY turn_id",
        "SELECT agent_id || ':' || agent_json || ':' || created_at
         FROM session_agents
         WHERE generation = {generation}
         ORDER BY agent_id",
        "SELECT parent_agent_id || ':' || child_agent_id || ':' || ordinal
         FROM session_agent_hierarchy_edges
         WHERE generation = {generation}
         ORDER BY parent_agent_id, child_agent_id",
        "SELECT superseded_assertion_id || ':' || superseding_assertion_id || ':' || created_at
         FROM session_assertion_supersession
         WHERE generation = {generation}
         ORDER BY superseded_assertion_id, superseding_assertion_id",
        "SELECT occurrence.occurrence_id || ':' || fts.index_text || ':' || fts.snippet_text
         FROM session_occurrences AS occurrence
         JOIN session_occurrences_fts AS fts ON fts.rowid = occurrence.rowid
         WHERE occurrence.generation = {generation}
         ORDER BY occurrence.occurrence_id",
    ] {
        assert_eq!(
            rows(&path, &projection.replace("{generation}", "2")).await,
            rows(&path, &projection.replace("{generation}", "3")).await
        );
    }
}

#[tokio::test]
async fn activation_rejects_omitted_canonical_assertion_lineage() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.omitted-relations");
    let first = occurrence(
        &session_id,
        &persist_observation(&db, &session_id, 0, "first").await,
    );
    let second = occurrence(
        &session_id,
        &persist_observation_with_lineage(
            &db,
            &session_id,
            1,
            "second",
            AnchorProvenanceRelationV2::Supersedes,
            first.retrieval_anchor_id.clone(),
            None,
        )
        .await,
    );
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone(), second.clone()],
            vec![copy(&second, &first)],
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
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.complete");
    let first = occurrence(
        &session_id,
        &persist_observation(&db, &session_id, 0, "first").await,
    );
    let second = occurrence(
        &session_id,
        &persist_observation_with_lineage(
            &db,
            &session_id,
            1,
            "second",
            AnchorProvenanceRelationV2::Supersedes,
            first.retrieval_anchor_id.clone(),
            None,
        )
        .await,
    );
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone(), second.clone()],
            vec![copy(&second, &first)],
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
            )
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        rows(
            &path,
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
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.transitive-supersession");
    let first = occurrence(
        &session_id,
        &persist_observation(&db, &session_id, 0, "first").await,
    );
    let mut second = occurrence(
        &session_id,
        &persist_observation_with_lineage(
            &db,
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
            &db,
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
            &db,
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
    let store = GlobalDbSessionTemporalStore::new(&db);
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
            &path,
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
            &path,
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
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.activation-failure");
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 0).await;

    assert!(
        store
            .activate_session_temporal_generation(
                SessionGenerationActivationRequestV1::new(
                    session_id.clone(),
                    generation(2),
                    snapshot(&session_id, 1, 0),
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

#[tokio::test]
async fn restart_resumes_one_existing_candidate_instead_of_duplicating_it() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let session_id = session("session.temporal.restart");
    let request = SessionGenerationRebuildRequestV1::new(
        session_id.clone(),
        generation(2),
        snapshot(&session_id, 1, 0),
    )
    .unwrap();
    {
        let db = open_lcm_db(&tmp).await;
        let store = GlobalDbSessionTemporalStore::new(&db);
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
        let db = open_lcm_db(&tmp).await;
        let store = GlobalDbSessionTemporalStore::new(&db);
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
async fn activation_is_pinned_to_the_snapshot_active_generation() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.pinning");
    let observation = persist_observation(&db, &session_id, 0, "pinning").await;
    let store = GlobalDbSessionTemporalStore::new(&db);
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

    let raw_db = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
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
    .await
    .unwrap();

    assert!(matches!(
        store
            .activate_session_temporal_generation(
                SessionGenerationActivationRequestV1::new(
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
            "SELECT generation || ':' || state
             FROM session_temporal_generations
             WHERE state = 'active'"
        )
        .await,
        vec!["3:active"]
    );
}

#[tokio::test]
async fn parent_message_linkage_copy_proof_requires_exact_parent_id() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.parent-linkage");
    let first = persist_observation(&db, &session_id, 0, "parent").await;
    let second = persist_observation(&db, &session_id, 1, "child").await;
    let first = occurrence(&session_id, &first);
    let second = occurrence(&session_id, &second);
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone(), second.clone()],
            vec![],
            vec![],
        ))
        .await
        .unwrap();

    let mut mismatched = parent_message_copy(&second, &first);
    mismatched.proof = CopyProofV1::ParentMessageLinkage {
        source_occurrence_id: first.occurrence_id.clone(),
        parent_message_id: MessageId::new("message.temporal.forged").unwrap(),
    };
    assert!(
        store
            .persist_session_temporal_projection_batch(
                batch(&session_id, 2, 2, vec![], vec![mismatched], vec![])
                    .with_checkpoint(1, 2, 2)
                    .unwrap(),
            )
            .await
            .is_err()
    );
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_logical_copy_edges").await,
        0
    );

    store
        .persist_session_temporal_projection_batch(
            batch(
                &session_id,
                2,
                2,
                vec![],
                vec![parent_message_copy(&second, &first)],
                vec![],
            )
            .with_checkpoint(1, 2, 2)
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_logical_copy_edges").await,
        1
    );
}

#[tokio::test]
async fn begin_rejects_watermark_mismatch_and_stale_pin_after_activation() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.begin-complete");
    let store = GlobalDbSessionTemporalStore::new(&db);
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
    let observation = persist_observation(&db, &session_id, 0, "complete").await;
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
async fn activation_rejects_incomplete_frontier_and_receipt_digest_mismatch() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.frontier-digest");
    let first = persist_observation(&db, &session_id, 0, "one").await;
    let second = persist_observation(&db, &session_id, 1, "two").await;
    let first = occurrence(&session_id, &first);
    let second = occurrence(&session_id, &second);
    let store = GlobalDbSessionTemporalStore::new(&db);
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
    let raw_db = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "UPDATE session_occurrences
         SET snippet_text = 'tampered'
         WHERE session_id = ?1 AND generation = 3",
        libsql::params![session_id.as_str()],
    )
    .await
    .unwrap();
    assert!(
        store
            .activate_session_temporal_generation(
                SessionGenerationActivationRequestV1::new(
                    session_id.clone(),
                    generation(3),
                    snapshot(&session_id, 1, 2),
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

#[tokio::test]
async fn mid_batch_abort_preserves_prior_receipt_frontier_for_resume() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.mid-batch-abort");
    let first = persist_observation(&db, &session_id, 0, "stable").await;
    let second = persist_observation(&db, &session_id, 1, "pending").await;
    let first = occurrence(&session_id, &first);
    let second = occurrence(&session_id, &second);
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone(), second.clone()],
            vec![],
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts
             WHERE session_id = 'session.temporal.mid-batch-abort'"
        )
        .await,
        1
    );

    let raw_db = libsql::Builder::new_local(&path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch(
        "CREATE TRIGGER abort_copy_insert
         BEFORE INSERT ON session_logical_copy_edges
         BEGIN
             SELECT RAISE(ABORT, 'forced mid-batch projector failure');
         END;",
    )
    .await
    .unwrap();
    assert!(
        store
            .persist_session_temporal_projection_batch(
                batch(
                    &session_id,
                    2,
                    2,
                    vec![],
                    vec![parent_message_copy(&second, &first)],
                    vec![],
                )
                .with_checkpoint(1, 2, 2)
                .unwrap(),
            )
            .await
            .is_err()
    );
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_logical_copy_edges").await,
        0
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts
             WHERE session_id = 'session.temporal.mid-batch-abort'"
        )
        .await,
        1
    );
    assert_eq!(
        rows(
            &path,
            "SELECT generation || ':' || state
             FROM session_temporal_generations
             WHERE session_id = 'session.temporal.mid-batch-abort'
             ORDER BY generation"
        )
        .await,
        vec!["1:active", "2:building"]
    );

    conn.execute("DROP TRIGGER abort_copy_insert", ())
        .await
        .unwrap();
    drop(conn);
    drop(raw_db);

    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
    assert_eq!(
        begin_candidate(&store, &session_id, 2, 2).await,
        SessionGenerationRebuildDispositionV1::Resumed
    );
    assert_eq!(
        store
            .persist_session_temporal_projection_batch(
                batch(
                    &session_id,
                    2,
                    2,
                    vec![],
                    vec![parent_message_copy(&second, &first)],
                    vec![],
                )
                .with_checkpoint(1, 2, 2)
                .unwrap(),
            )
            .await
            .unwrap()
            .disposition(),
        SessionTemporalProjectionBatchDispositionV1::Applied
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts
             WHERE session_id = 'session.temporal.mid-batch-abort'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn interrupted_rebuild_resumes_building_then_activates_ready_to_active() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let session_id = session("session.temporal.interrupted-activate");
    let request = SessionGenerationRebuildRequestV1::new(
        session_id.clone(),
        generation(2),
        snapshot(&session_id, 1, 1),
    )
    .unwrap();
    let observation = {
        let db = open_lcm_db(&tmp).await;
        let observation = persist_observation(&db, &session_id, 0, "resume-activate").await;
        let store = GlobalDbSessionTemporalStore::new(&db);
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
        let db = open_lcm_db(&tmp).await;
        let store = GlobalDbSessionTemporalStore::new(&db);
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
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.duplicate-within");
    let duplicate = "message.temporal.duplicate";
    let first = persist_custom_observation(
        &db,
        observation_with_message_ids(&session_id, 0, "first", duplicate, None),
    )
    .await;
    let second = persist_custom_observation(
        &db,
        observation_with_message_ids(&session_id, 1, "second", duplicate, None),
    )
    .await;
    let store = GlobalDbSessionTemporalStore::new(&db);
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
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.duplicate-across");
    let duplicate = "message.temporal.duplicate";
    let first = persist_custom_observation(
        &db,
        observation_with_message_ids(&session_id, 0, "first", duplicate, None),
    )
    .await;
    let second = persist_custom_observation(
        &db,
        observation_with_message_ids(&session_id, 1, "second", duplicate, None),
    )
    .await;
    let store = GlobalDbSessionTemporalStore::new(&db);
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
        let db = open_lcm_db(&tmp).await;
        let first = persist_custom_observation(
            &db,
            observation_with_message_ids(&session_id, 0, "first", duplicate, None),
        )
        .await;
        let second = persist_custom_observation(
            &db,
            observation_with_message_ids(&session_id, 1, "second", duplicate, None),
        )
        .await;
        let store = GlobalDbSessionTemporalStore::new(&db);
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
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
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
            )
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:?}").contains("resolves to 2 occurrences"));
}

#[tokio::test]
async fn copied_from_requires_explicit_typed_copy_record() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_id = session("session.temporal.copied-from-explicit");
    let first = occurrence(
        &session_id,
        &persist_observation(&db, &session_id, 0, "source").await,
    );
    let second_observation = persist_custom_observation_with_lineage(
        &db,
        observation_with_message_ids(&session_id, 1, "copy", "message.temporal.copy", None),
        AnchorProvenanceRelationV2::CopiedFrom,
        first.retrieval_anchor_id.clone(),
    )
    .await;
    let second =
        occurrence_with_message_id(&session_id, &second_observation, "message.temporal.copy");
    let store = GlobalDbSessionTemporalStore::new(&db);
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone(), second.clone()],
            vec![],
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_logical_copy_edges").await,
        0,
        "CopiedFrom lineage alone must not synthesize a copy edge"
    );
    store
        .persist_session_temporal_projection_batch(
            batch(
                &session_id,
                2,
                2,
                vec![],
                vec![explicit_anchor_copy(&second, &first)],
                vec![],
            )
            .with_checkpoint(1, 2, 2)
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        scalar(&path, "SELECT COUNT(*) FROM session_logical_copy_edges").await,
        1
    );
    store
        .activate_session_temporal_generation(
            SessionGenerationActivationRequestV1::new(
                session_id.clone(),
                generation(2),
                snapshot(&session_id, 1, 2),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        rows(
            &path,
            "SELECT generation || ':' || state
             FROM session_temporal_generations
             WHERE session_id = 'session.temporal.copied-from-explicit'
             ORDER BY generation"
        )
        .await,
        vec!["1:superseded", "2:active"]
    );
}
