use super::common::*;
use super::*;
use tracedecay_temporal_query::ports::ExecutionControl;

#[test]
fn frozen_snapshots_preserve_exact_session_and_reject_cross_session_reads() {
    let session_a = session("session.a");
    let snapshot = snapshot_for(session_a.clone(), 7);
    assert_eq!(snapshot.session_id(), &session_a);

    let error = SessionTemporalRetrievalRequestV1::new(
        session("session.b"),
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        snapshot,
        3,
        None,
        ExecutionControl::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        SessionStoreError::SessionMismatch {
            context: "temporal retrieval request"
        }
    ));
}

#[test]
fn retrieval_requires_frozen_watermark_capability_and_enforces_page_bounds() {
    let session_id = session("session.fixture");
    let snapshot = snapshot_with_capabilities(session_id.clone(), []);
    assert!(matches!(
        SessionTemporalRetrievalRequestV1::new(
            session_id.clone(),
            TemporalModeV1::Current,
            RetrievalGrainV1::Occurrence,
            snapshot,
            3,
            None,
            ExecutionControl::default(),
        ),
        Err(SessionStoreError::UnsupportedCapability {
            capability: SessionTemporalCapabilityV1::FrozenWatermarks
        })
    ));

    for invalid_limit in [0, MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE + 1] {
        assert!(matches!(
            SessionTemporalRetrievalRequestV1::new(
                session_id.clone(),
                TemporalModeV1::Current,
                RetrievalGrainV1::Occurrence,
                snapshot_for(session_id.clone(), 7),
                invalid_limit,
                None,
                ExecutionControl::default(),
            ),
            Err(SessionStoreError::InvalidPageLimit { .. })
        ));
    }
}

#[test]
fn retrieval_pages_validate_record_sessions_and_domain_records() {
    let session_id = session("session.fixture");
    let mut invalid_occurrence = occurrence_record(&session_id, 0);
    invalid_occurrence.occurrence_id = occurrence_id(1);
    assert!(matches!(
        SessionRetrievalPageV1::new(
            snapshot_for(session_id.clone(), 7),
            vec![invalid_occurrence],
            vec![],
            vec![],
            vec![],
            coverage(),
            None,
        ),
        Err(SessionStoreError::Contract(
            SessionContractError::OccurrenceIdentityMismatch
        ))
    ));
    assert!(matches!(
        SessionRetrievalPageV1::new(
            snapshot_for(session_id, 7),
            vec![occurrence_record(&session("session.other"), 0)],
            vec![],
            vec![],
            vec![],
            coverage(),
            None,
        ),
        Err(SessionStoreError::SessionMismatch {
            context: "retrieval occurrence"
        })
    ));
}

#[test]
fn retrieval_rejects_cross_session_summaries() {
    let session_id = session("session.retrieval");
    assert!(matches!(
        SessionRetrievalPageV1::new(
            snapshot_for(session_id, 7),
            vec![],
            vec![],
            vec![],
            vec![summary(
                &session("session.other"),
                "summary.other-session",
                1
            )],
            coverage(),
            None,
        ),
        Err(SessionStoreError::SessionMismatch {
            context: "retrieval summary"
        })
    ));
}

#[test]
fn cursor_pagination_requires_a_key_frozen_with_the_watermarks() {
    let session_id = session("session.cursor");
    let snapshot = snapshot_for(session_id.clone(), 7);
    assert!(matches!(
        SessionTemporalRetrievalRequestV1::new(
            session_id.clone(),
            TemporalModeV1::Current,
            RetrievalGrainV1::Occurrence,
            snapshot.clone(),
            1,
            Some(occurrence_id(0)),
            ExecutionControl::default(),
        ),
        Err(SessionStoreError::CursorKeyRequired)
    ));
    assert!(matches!(
        SessionRetrievalPageV1::new(
            snapshot,
            vec![],
            vec![],
            vec![],
            vec![],
            coverage(),
            Some(occurrence_id(0)),
        ),
        Err(SessionStoreError::CursorKeyRequired)
    ));
}
