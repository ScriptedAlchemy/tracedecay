use super::common::*;
use super::*;

#[test]
fn projection_and_nested_summary_bounds_are_enforced_deeply() {
    let session_id = session("session.fixture");
    let occurrence = occurrence_record(&session_id, 0);
    assert!(matches!(
        SessionTemporalProjectionBatchV1::new(
            session_id.clone(),
            generation(8),
            SessionFrozenWatermarksV1::new(generation(7), 51, 47, 43),
            vec![occurrence; MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS + 1],
            vec![],
            vec![],
        ),
        Err(SessionStoreError::BatchLimitExceeded { .. })
    ));

    let oversized_summary = summary(
        &session_id,
        "summary.oversized",
        MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE,
    );
    assert!(matches!(
        SessionRetrievalPageV1::new(
            snapshot_for(session_id.clone(), 7),
            vec![],
            vec![],
            vec![],
            vec![oversized_summary],
            coverage(),
            None,
        ),
        Err(SessionStoreError::BatchLimitExceeded {
            field: "session temporal retrieval page",
            ..
        })
    ));

    let empty_projection = SessionTemporalProjectionBatchV1::new(
        session_id.clone(),
        generation(8),
        SessionFrozenWatermarksV1::new(generation(7), 51, 47, 43),
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    let oversized_summary = summary(
        &session_id,
        "summary.migration-oversized",
        MAX_SESSION_TEMPORAL_MIGRATION_BATCH_ITEMS,
    );
    assert!(matches!(
        SessionTemporalMigrationBatchV1::new(
            session_id,
            digest(),
            generation(8),
            0,
            empty_projection.watermarks().clone(),
            empty_projection,
            vec![oversized_summary],
        ),
        Err(SessionStoreError::BatchLimitExceeded {
            field: "session temporal migration batch",
            ..
        })
    ));
}

#[test]
fn immutable_summary_replay_distinguishes_exact_replay_from_conflict() {
    let session_id = session("session.fixture");
    let original = summary(&session_id, "summary.fixture", 1);
    let request = SessionSummaryPublicationRequestV1::new(
        original.clone(),
        snapshot_for(session_id.clone(), 7),
    )
    .unwrap();
    assert_eq!(
        request.replay_disposition(&original).unwrap(),
        SessionSummaryPublicationDispositionV1::ExactReplay
    );

    let conflicting = summary(&session_id, "summary.fixture", 2);
    assert!(matches!(
        request.replay_disposition(&conflicting),
        Err(SessionStoreError::ImmutableSummaryConflict { summary_id })
            if summary_id == *original.summary_id()
    ));
}

#[test]
fn immutable_summary_publication_rejects_cross_session_and_missing_capability() {
    let session_id = session("session.fixture");
    let summary = summary(&session_id, "summary.fixture", 1);
    assert!(matches!(
        SessionSummaryPublicationRequestV1::new(
            summary.clone(),
            snapshot_for(session("session.other"), 7),
        ),
        Err(SessionStoreError::SessionMismatch {
            context: "summary publication"
        })
    ));
    assert!(matches!(
        SessionSummaryPublicationRequestV1::new(
            summary,
            snapshot_with_capabilities(session_id, [SessionTemporalCapabilityV1::FrozenWatermarks]),
        ),
        Err(SessionStoreError::UnsupportedCapability {
            capability: SessionTemporalCapabilityV1::ImmutableSummaryPublication
        })
    ));
}

#[test]
fn summary_source_limit_accepts_max_minus_one_and_max_but_rejects_max_plus_one() {
    let session_id = session("session.summary-limits");
    let snapshot = snapshot_for(session_id.clone(), 7);
    for count in [
        MAX_SESSION_SUMMARY_SOURCE_ANCHORS - 1,
        MAX_SESSION_SUMMARY_SOURCE_ANCHORS,
    ] {
        assert!(
            SessionSummaryPublicationRequestV1::new(
                summary(&session_id, &format!("summary.{count}"), count),
                snapshot.clone(),
            )
            .is_ok()
        );
    }
    assert!(matches!(
        SessionSummaryPublicationRequestV1::new(
            summary(
                &session_id,
                "summary.over-limit",
                MAX_SESSION_SUMMARY_SOURCE_ANCHORS + 1,
            ),
            snapshot,
        ),
        Err(SessionStoreError::BatchLimitExceeded {
            field: "session summary source anchors",
            count,
            max: MAX_SESSION_SUMMARY_SOURCE_ANCHORS,
        }) if count == MAX_SESSION_SUMMARY_SOURCE_ANCHORS + 1
    ));
}

impl SessionSummaryStore for InMemorySessionPorts {
    fn publish_immutable_session_summary_supported(
        &self,
        _permit: SessionSummaryPublishOrReplayPermit,
        request: SessionSummaryPublicationRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionSummaryPublicationReceiptV1>> + Send {
        async move {
            yield_once().await;
            let mut state = self.state.lock().unwrap();
            let receipt = match &state.summary {
                Some(existing) => SessionSummaryPublicationReceiptV1::exact_replay(
                    &request,
                    existing,
                    UtcMicros(104),
                )?,
                None => SessionSummaryPublicationReceiptV1::published(&request, UtcMicros(104)),
            };
            state.summary = Some(request.summary().clone());
            Ok(receipt)
        }
    }
}
