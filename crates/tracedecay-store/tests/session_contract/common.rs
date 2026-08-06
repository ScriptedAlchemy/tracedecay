use super::*;

pub(super) fn session(value: &str) -> SessionId {
    SessionId::new(value).unwrap()
}

pub(super) fn generation(value: u64) -> SessionProjectionGenerationV1 {
    SessionProjectionGenerationV1::new(value).unwrap()
}

pub(super) fn capabilities(
    values: impl IntoIterator<Item = SessionTemporalCapabilityV1>,
) -> SessionTemporalCapabilitiesV1 {
    SessionTemporalCapabilitiesV1::new(values)
}

pub(super) fn snapshot_for(
    session_id: SessionId,
    active_generation: u64,
) -> SessionTemporalSnapshotV1 {
    SessionTemporalSnapshotV1::new(
        session_id,
        UtcMicros(99),
        SessionFrozenWatermarksV1::new(generation(active_generation), 51, 47, 43),
        capabilities([
            SessionTemporalCapabilityV1::FrozenWatermarks,
            SessionTemporalCapabilityV1::GenerationRebuild,
            SessionTemporalCapabilityV1::ImmutableSummaryPublication,
            SessionTemporalCapabilityV1::RefreshJoin,
            SessionTemporalCapabilityV1::RefreshProgressPersistence,
            SessionTemporalCapabilityV1::RefreshCancellation,
        ]),
    )
}

pub(super) fn snapshot_with_capabilities(
    session_id: SessionId,
    values: impl IntoIterator<Item = SessionTemporalCapabilityV1>,
) -> SessionTemporalSnapshotV1 {
    SessionTemporalSnapshotV1::new(
        session_id,
        UtcMicros(99),
        SessionFrozenWatermarksV1::new(generation(7), 51, 47, 43),
        capabilities(values),
    )
}

pub(super) fn observation_id() -> CanonicalObservationIdV1 {
    CanonicalObservationIdV1::new(format!("sha256:{}", "1".repeat(64))).unwrap()
}

pub(super) fn occurrence_id(ordinal: u32) -> MessageOccurrenceIdV1 {
    MessageOccurrenceIdV1::derive(&observation_id(), ProjectionOutputOrdinalV1::new(ordinal))
}

pub(super) fn evidence_wire() -> serde_json::Value {
    json!({
        "authority": "provider_native",
        "evidence_class": "provider_declared",
        "source_anchor_id": "anchor.evidence",
        "sanitization_receipt": {
            "receipt_id": "receipt.fixture",
            "sanitizer_version": "sanitizer.fixture"
        }
    })
}

pub(super) fn occurrence_record(session_id: &SessionId, ordinal: u32) -> MessageOccurrenceRecordV1 {
    serde_json::from_value(json!({
        "occurrence_id": occurrence_id(ordinal),
        "source_observation_id": observation_id(),
        "projection_output_ordinal": ordinal,
        "retrieval_anchor_id": format!("anchor.occurrence.{ordinal}"),
        "session_id": session_id,
        "thread_id": "thread.fixture",
        "thread_grouping": {"kind": "provider_native"},
        "turn_id": "turn.fixture",
        "turn_grouping": {"kind": "provider_native"},
        "message_id": format!("message.fixture.{ordinal}"),
        "agent_id": "agent.fixture",
        "role": "user",
        "knowledge_at": 50,
        "valid_time": {"kind": "known", "valid_at": 40},
        "evidence": evidence_wire()
    }))
    .unwrap()
}

pub(super) fn copy_record(source_ordinal: u32, target_ordinal: u32) -> LogicalCopyRecordV1 {
    let source = occurrence_id(source_ordinal);
    LogicalCopyRecordV1 {
        occurrence_id: occurrence_id(target_ordinal),
        copied_from_occurrence_id: source.clone(),
        proof: CopyProofV1::ProviderLinkage {
            source_occurrence_id: source,
            provider_record_id: ObservationId::new("provider.copy.fixture").unwrap(),
        },
        knowledge_at: UtcMicros(50),
        valid_time: TemporalValidityV1::Unknown,
    }
}

pub(super) fn assertion_record(
    subject_ordinal: u32,
    object_ordinal: u32,
) -> TemporalAssertionRecordV1 {
    serde_json::from_value(json!({
        "assertion_id": format!("assertion.{subject_ordinal}.{object_ordinal}"),
        "kind": "supports",
        "subject_anchor_id": format!("anchor.occurrence.{subject_ordinal}"),
        "object_anchor_id": format!("anchor.occurrence.{object_ordinal}"),
        "knowledge_at": 50,
        "valid_time": {"kind": "known", "valid_at": 40},
        "evidence": evidence_wire()
    }))
    .unwrap()
}

pub(super) fn summary(
    session_id: &SessionId,
    summary_id: &str,
    source_count: usize,
) -> SessionSummaryRecordV1 {
    SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new(summary_id).unwrap(),
        session_id.clone(),
        RetrievalAnchorId::new(format!("anchor.{summary_id}")).unwrap(),
        (0..source_count)
            .map(|index| RetrievalAnchorId::new(format!("anchor.source.{index}")).unwrap())
            .collect(),
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(50),
            valid_through: Some(UtcMicros(40)),
        },
        UtcMicros(60),
    )
    .unwrap()
}

pub(super) fn projection_batch(session_id: &SessionId) -> SessionTemporalProjectionBatchV1 {
    SessionTemporalProjectionBatchV1::new(
        session_id.clone(),
        generation(8),
        SessionFrozenWatermarksV1::new(generation(7), 51, 47, 43),
        vec![
            occurrence_record(session_id, 0),
            occurrence_record(session_id, 1),
        ],
        vec![copy_record(0, 1)],
        vec![assertion_record(0, 1)],
    )
    .unwrap()
}

pub(super) fn coverage() -> TemporalCoverageCountsV1 {
    TemporalCoverageCountsV1 {
        visible: 8,
        hidden: 2,
        unknown: 1,
        redacted: 1,
    }
}

pub(super) fn operation_id() -> SessionRefreshOperationIdV1 {
    SessionRefreshOperationIdV1::new("refresh.fixture").unwrap()
}

pub(super) fn temporal_digest(value: char) -> SessionTemporalDigestV1 {
    SessionTemporalDigestV1::new(format!("sha256:{}", value.to_string().repeat(64))).unwrap()
}

pub(super) fn ready<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    for _ in 0..8 {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
    panic!("contract future did not become ready")
}

pub(super) fn yields_then_ready<F>(future: F) -> F::Output
where
    F: Future + Send,
{
    let mut context = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("yielding contract future did not resume"),
    }
}

#[test]
fn semantic_store_errors_are_typed_and_non_storage() {
    let errors = [
        SessionStoreError::MissingGeneration {
            generation: generation(8),
        },
        SessionStoreError::StaleGeneration {
            expected: generation(8),
            actual: generation(7),
        },
        SessionStoreError::InvalidRefreshState {
            operation_id: operation_id(),
            state: SessionRefreshStateV1::Complete,
        },
        SessionStoreError::Cancelled,
        SessionStoreError::DeadlineExceeded,
        SessionStoreError::BudgetExceeded {
            resource: "work units",
        },
    ];
    assert!(
        errors
            .iter()
            .all(|error| !matches!(error, SessionStoreError::Storage { .. }))
    );
}

#[test]
fn adapter_failures_map_to_storage_without_erasing_semantic_errors() {
    let storage =
        SessionStoreError::storage("freeze session snapshot", std::io::Error::other("offline"));
    assert!(storage.is_storage());
    assert!(std::error::Error::source(&storage).is_some());

    let semantic = SessionStoreError::SessionMismatch {
        context: "typed mapping",
    };
    assert!(!semantic.is_storage());
}

#[test]
fn temporal_digests_are_bounded_and_canonical() {
    let digest = temporal_digest('a');
    assert_eq!(digest.as_str(), format!("sha256:{}", "a".repeat(64)));

    for (value, reason) in [
        (
            format!("sha256:{}", "a".repeat(63)),
            SessionTemporalDigestInvalidReasonV1::Malformed,
        ),
        (
            format!("sha256:{}", "A".repeat(64)),
            SessionTemporalDigestInvalidReasonV1::Malformed,
        ),
        (
            "x".repeat(SessionTemporalDigestV1::MAX_LEN + 1),
            SessionTemporalDigestInvalidReasonV1::TooLong,
        ),
    ] {
        assert!(matches!(
            SessionTemporalDigestV1::new(value),
            Err(SessionStoreError::InvalidTemporalDigest {
                reason: actual_reason
            }) if actual_reason == reason
        ));
    }
}

#[test]
fn session_contract_dtos_are_public_for_schema_and_kernel_adapters() {
    let _ = (
        std::mem::size_of::<SessionTemporalProjectionBatchV1>(),
        std::mem::size_of::<SessionTemporalProjectionBatchReceiptV1>(),
        std::mem::size_of::<SessionGenerationRebuildRequestV1>(),
        std::mem::size_of::<SessionGenerationRebuildReceiptV1>(),
        std::mem::size_of::<SessionGenerationActivationRequestV1>(),
        std::mem::size_of::<SessionGenerationActivationReceiptV1>(),
        std::mem::size_of::<SessionRetrievalPageV1>(),
        std::mem::size_of::<SessionSummaryPublicationRequestV1>(),
        std::mem::size_of::<SessionRefreshBeginOrJoinRequestV1>(),
        std::mem::size_of::<SessionRefreshBeginOrJoinReceiptV1>(),
        std::mem::size_of::<SessionRefreshProgressRequestV1>(),
        std::mem::size_of::<SessionRefreshProgressV1>(),
        std::mem::size_of::<SessionRefreshCompletionRequestV1>(),
        std::mem::size_of::<SessionRefreshFailureRequestV1>(),
        std::mem::size_of::<SessionRefreshCancellationRequestV1>(),
        std::mem::size_of::<SessionRefreshReceiptV1>(),
    );
}

#[derive(Default)]
pub(super) struct InMemorySessionState {
    pub(super) rebuild: Option<SessionGenerationRebuildReceiptV1>,
    pub(super) projection: Option<SessionTemporalProjectionBatchReceiptV1>,
    pub(super) refresh_request: Option<SessionRefreshBeginOrJoinRequestV1>,
    pub(super) refresh_progress: Option<SessionRefreshProgressV1>,
    pub(super) refresh_receipt: Option<SessionRefreshReceiptV1>,
}

#[derive(Default)]
pub(super) struct InMemorySessionPorts {
    pub(super) state: Mutex<InMemorySessionState>,
}

pub(super) async fn yield_once() {
    let mut yielded = false;
    std::future::poll_fn(move |context| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            context.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}

impl SessionTemporalCapabilityProvider for InMemorySessionPorts {
    fn session_temporal_capabilities(&self) -> &SessionTemporalCapabilitiesV1 {
        static CAPABILITIES: std::sync::LazyLock<SessionTemporalCapabilitiesV1> =
            std::sync::LazyLock::new(|| {
                capabilities([
                    SessionTemporalCapabilityV1::FrozenWatermarks,
                    SessionTemporalCapabilityV1::GenerationRebuild,
                    SessionTemporalCapabilityV1::ImmutableSummaryPublication,
                    SessionTemporalCapabilityV1::RefreshJoin,
                    SessionTemporalCapabilityV1::RefreshProgressPersistence,
                    SessionTemporalCapabilityV1::RefreshCancellation,
                ])
            });
        &CAPABILITIES
    }
}
