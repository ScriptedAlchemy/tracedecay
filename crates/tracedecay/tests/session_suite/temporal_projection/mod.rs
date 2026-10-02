use std::fmt::Write as _;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_domain::{
    AnchorProvenanceRelation, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CopyProofV1, DurableObservationV1, MessageId, MessageOccurrenceIdV1, MessageOccurrenceRecordV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId,
    ProjectionOutputOrdinalV1, ProviderId, RetentionClass, RetrievalAnchorId,
    RetrievalAnchorRecord, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, SessionProjectionGenerationV1,
    TemporalAssertionKindV1, TemporalAssertionRecordV1, TemporalValidityV1, UtcMicros,
    derive_exact_observation_anchor_id,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_session_temporal_store::{SessionRefreshRecoveryV1, SessionTemporalStore};
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_store::{
    AnchoredObservationWrite, MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS,
    ObservationProjectionStore, ObservationStore, ObservationWrite, SessionFrozenWatermarksV1,
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCancellationRequestV1,
    SessionRefreshCompletionRequestV1, SessionRefreshFailureRequestV1, SessionRefreshFrontierV1,
    SessionRefreshProgressRequestV1, SessionRefreshProgressV1, SessionRefreshStore,
    SessionStoreError, SessionStoreResult, SessionTemporalProjectionBatchDispositionV1,
    SessionTemporalProjectionBatchReceiptV1, SessionTemporalProjectionBatchV1,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor,
};
use tracedecay_temporal_query::execution::ExecutionControl;

pub(crate) type TemporalStore<'a> = SessionTemporalStore<'a, RegisteredGlobalDb>;

pub(crate) async fn profile_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap()
}

pub(crate) fn session(value: &str) -> SessionId {
    SessionId::new(value).unwrap()
}

pub(crate) fn generation(value: u64) -> SessionProjectionGenerationV1 {
    SessionProjectionGenerationV1::new(value).unwrap()
}

pub(crate) fn watermarks(
    active_generation: u64,
    source_frontier: u64,
) -> SessionFrozenWatermarksV1 {
    SessionFrozenWatermarksV1::new(
        generation(active_generation),
        source_frontier,
        source_frontier,
        0,
    )
}

pub(crate) fn receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
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

pub(crate) fn observation(
    session_id: &SessionId,
    ordinal: u64,
    text: &str,
) -> DurableObservationV1 {
    observation_with_message_ids(
        session_id,
        ordinal,
        text,
        &format!("message.temporal.{}.{}", session_id.as_str(), ordinal),
        (ordinal > 0).then(|| format!("message.temporal.{}.{}", session_id.as_str(), ordinal - 1)),
    )
}

pub(crate) fn observation_with_message_ids(
    session_id: &SessionId,
    ordinal: u64,
    text: &str,
    message_id: &str,
    parent_message_id: Option<String>,
) -> DurableObservationV1 {
    // Namespace provider/record/receipt by session so multi-session suites do not
    // collide on global sanitization-receipt identity.
    let provider =
        ProviderId::new(format!("temporal-test.{}.{}", session_id.as_str(), ordinal)).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
    let record_id = ObservationId::new(format!(
        "record.temporal.{}.{}",
        session_id.as_str(),
        ordinal
    ))
    .unwrap();
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
        receipt(
            &format!("receipt.temporal.{}.{}", session_id.as_str(), ordinal),
            &payload,
        ),
        RetentionClass::new("retention.temporal-test").unwrap(),
        payload,
    )
    .unwrap()
}

pub(crate) fn anchored_write(observation: DurableObservationV1) -> AnchoredObservationWrite {
    anchored_write_with_lineage(observation, None, None)
}

pub(crate) fn anchored_write_with_lineage(
    observation: DurableObservationV1,
    lineage: Option<(AnchorProvenanceRelation, RetrievalAnchorId)>,
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
    let anchor = build_observation_retrieval_anchor(
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
            "end": valid_at.saturating_add(1),
        });
    }
    let anchor: RetrievalAnchorRecord = serde_json::from_value(anchor_json).unwrap();
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

pub(crate) async fn persist_observation<S>(
    store: &S,
    session_id: &SessionId,
    ordinal: u64,
    text: &str,
) -> DurableObservationV1
where
    S: ObservationStore + ObservationProjectionStore,
{
    let observation = observation(session_id, ordinal, text);
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

pub(crate) async fn persist_custom_observation<S>(
    store: &S,
    observation: DurableObservationV1,
) -> DurableObservationV1
where
    S: ObservationStore + ObservationProjectionStore,
{
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

async fn persist_custom_observation_with_lineage<S>(
    store: &S,
    observation: DurableObservationV1,
    relation: AnchorProvenanceRelation,
    object_anchor_id: RetrievalAnchorId,
) -> DurableObservationV1
where
    S: ObservationStore + ObservationProjectionStore,
{
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

pub(crate) async fn persist_observation_with_lineage<S>(
    store: &S,
    session_id: &SessionId,
    ordinal: u64,
    text: &str,
    relation: AnchorProvenanceRelation,
    object_anchor_id: RetrievalAnchorId,
    valid_at: Option<i64>,
) -> DurableObservationV1
where
    S: ObservationStore + ObservationProjectionStore,
{
    let observation = observation(session_id, ordinal, text);
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

/// Persists a `first` observation and a `second` whose anchor supersedes it,
/// returning their canonical occurrences.
pub(crate) async fn persist_superseding_pair<S>(
    store: &S,
    session_id: &SessionId,
) -> (MessageOccurrenceRecordV1, MessageOccurrenceRecordV1)
where
    S: ObservationStore + ObservationProjectionStore,
{
    let first = occurrence(
        session_id,
        &persist_observation(store, session_id, 0, "first").await,
    );
    let second = occurrence(
        session_id,
        &persist_observation_with_lineage(
            store,
            session_id,
            1,
            "second",
            AnchorProvenanceRelation::Supersedes,
            first.retrieval_anchor_id.clone(),
            None,
        )
        .await,
    );
    (first, second)
}

pub(crate) fn occurrence(
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
            "message.temporal.{}.{}",
            session_id.as_str(),
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

pub(crate) fn occurrence_with_message_id(
    session_id: &SessionId,
    observation: &DurableObservationV1,
    message_id: &str,
) -> MessageOccurrenceRecordV1 {
    let mut occurrence = occurrence(session_id, observation);
    occurrence.message_id = Some(MessageId::new(message_id).unwrap());
    occurrence
}

pub(crate) fn parent_message_copy(
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
        knowledge_at: target.knowledge_at,
        valid_time: target.valid_time,
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
        knowledge_at: target.knowledge_at,
        valid_time: target.valid_time,
    }
}

pub(crate) fn assertion(
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
    let mut hasher = Sha256::new();
    hasher.update(
        format!(
            "session-temporal-assertion-v1\0{}\0{}\0{}",
            subject.retrieval_anchor_id.as_str(),
            kind.as_str(),
            object.retrieval_anchor_id.as_str()
        )
        .as_bytes(),
    );
    let mut assertion_id = String::with_capacity(71);
    assertion_id.push_str("sha256:");
    for byte in hasher.finalize() {
        write!(&mut assertion_id, "{byte:02x}").expect("writing to a String cannot fail");
    }
    serde_json::from_value(json!({
        "assertion_id": assertion_id,
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

/// A batch bound to the running refresh candidate's generation and frozen
/// watermarks, checkpointed through the candidate's whole frontier.
pub(crate) fn batch(
    candidate: &SessionRefreshRecoveryV1,
    occurrences: Vec<MessageOccurrenceRecordV1>,
    copies: Vec<tracedecay_domain::LogicalCopyRecordV1>,
    assertions: Vec<TemporalAssertionRecordV1>,
) -> SessionTemporalProjectionBatchV1 {
    SessionTemporalProjectionBatchV1::new(
        candidate.session_id().clone(),
        candidate.candidate_generation(),
        candidate.frozen_watermarks().clone(),
        occurrences,
        copies,
        assertions,
    )
    .unwrap()
}

/// Begins a refresh of `session_id` through `source_frontier` and returns the
/// candidate it allocated.
pub(crate) async fn begin_candidate(
    store: &TemporalStore<'_>,
    session_id: &SessionId,
    source_frontier: u64,
) -> SessionRefreshRecoveryV1 {
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(source_frontier, 0).unwrap(),
        ))
        .await
        .unwrap();
    store
        .session_refresh_recovery(session_id)
        .await
        .unwrap()
        .expect("begun refresh must be running")
}

async fn committed_progress(
    store: &TemporalStore<'_>,
    candidate: &SessionRefreshRecoveryV1,
) -> SessionStoreResult<Option<SessionRefreshProgressV1>> {
    store
        .session_refresh_progress(SessionRefreshProgressRequestV1::new(
            candidate.operation_id().clone(),
            candidate.session_id().clone(),
        ))
        .await
}

/// Persists `batch` through the candidate's refresh with the progress a
/// projector reports for it. Resubmitting the latest committed batch replays
/// its recorded progress, as a restarted projector does.
pub(crate) async fn persist_batch(
    store: &TemporalStore<'_>,
    candidate: &SessionRefreshRecoveryV1,
    batch: SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<SessionTemporalProjectionBatchReceiptV1> {
    let previous = committed_progress(store, candidate).await?;
    let progress = match previous {
        Some(previous) if previous.committed_batches() == batch.batch_ordinal() + 1 => previous,
        previous => {
            let records = previous
                .as_ref()
                .map_or(0, SessionRefreshProgressV1::committed_records)
                + u64::try_from(batch.item_count()).unwrap();
            let now = UtcMicros(
                i64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_micros(),
                )
                .unwrap(),
            );
            let updated_at = previous.as_ref().map_or(now, |previous| {
                UtcMicros(previous.updated_at().0 + 1).max(now)
            });
            SessionRefreshProgressV1::new(
                candidate.operation_id().clone(),
                candidate.session_id().clone(),
                SessionRefreshFrontierV1::new(
                    candidate.target_frontier().observed_through(),
                    batch.source_through(),
                )
                .unwrap(),
                tracedecay_domain::TemporalCoverageCountsV1 {
                    visible: records,
                    hidden: 0,
                    unknown: 0,
                    redacted: 0,
                },
                batch.batch_ordinal() + 1,
                records,
                updated_at,
            )
            .with_source_coverage(candidate.source_coverage(batch.source_through()).unwrap())
        }
    };
    store
        .persist_session_refresh_projection_batch(progress, batch)
        .await
        .map(|(_, receipt)| receipt)
}

/// Completes the candidate's refresh, activating its generation.
pub(crate) async fn complete_candidate(
    store: &TemporalStore<'_>,
    candidate: &SessionRefreshRecoveryV1,
) -> SessionStoreResult<()> {
    let progress = committed_progress(store, candidate)
        .await?
        .expect("completion requires projected progress");
    let mut request = SessionRefreshCompletionRequestV1::new(
        candidate.operation_id().clone(),
        candidate.session_id().clone(),
        progress.frontier(),
        *progress.coverage(),
    )?;
    if let Some(source_coverage) = progress.source_coverage() {
        request = request.with_source_coverage(source_coverage.clone());
    }
    store
        .complete_session_refresh(request, ExecutionControl::default())
        .await
        .map(|_| ())
}

/// Retires the candidate's refresh as failed, as the scheduler does after a
/// deterministic refusal, so the session can admit a new refresh.
pub(crate) async fn fail_candidate(
    store: &TemporalStore<'_>,
    candidate: &SessionRefreshRecoveryV1,
) {
    let progress = committed_progress(store, candidate)
        .await
        .unwrap()
        .expect("failure follows projected progress");
    let mut request = SessionRefreshFailureRequestV1::new(
        candidate.operation_id().clone(),
        candidate.session_id().clone(),
        progress.frontier(),
        *progress.coverage(),
        "temporal_test_refused",
    )
    .unwrap();
    if let Some(source_coverage) = progress.source_coverage() {
        request = request.with_source_coverage(source_coverage.clone());
    }
    store.fail_session_refresh(request).await.unwrap();
}

pub(crate) async fn scalar(path: &std::path::Path, sql: &str) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row(sql, (), |row| row.get(0))
        .unwrap()
}

pub(crate) async fn rows(path: &std::path::Path, sql: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(path).unwrap();
    let mut statement = conn.prepare(sql).unwrap();
    let mapped = statement.query_map((), |row| row.get(0)).unwrap();
    mapped.collect::<Result<Vec<_>, _>>().unwrap()
}

pub(crate) async fn scalar_runtime(runtime: &HostAdmissionTestRuntimeV1, sql: &str) -> i64 {
    let snapshot = TempDir::new().unwrap();
    let path = snapshot.path().join("sessions.db");
    runtime
        .snapshot_session_database_for_test(HostAdmissionScope::Profile, &path)
        .await
        .unwrap();
    scalar(&path, sql).await
}

pub(crate) async fn rows_runtime(runtime: &HostAdmissionTestRuntimeV1, sql: &str) -> Vec<String> {
    let snapshot = TempDir::new().unwrap();
    let path = snapshot.path().join("sessions.db");
    runtime
        .snapshot_session_database_for_test(HostAdmissionScope::Profile, &path)
        .await
        .unwrap();
    rows(&path, sql).await
}

mod activation;
mod batch_commit;
mod lineage;
mod refresh_lifecycle;
