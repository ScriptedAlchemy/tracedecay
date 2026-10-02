use super::common::*;
use super::*;

#[test]
fn projection_batches_call_every_domain_validator() {
    let session_id = session("session.fixture");
    let watermarks = SessionFrozenWatermarksV1::new(generation(7), 51, 47, 43);

    let mut invalid_occurrence = occurrence_record(&session_id, 0);
    invalid_occurrence.occurrence_id = occurrence_id(1);
    assert!(matches!(
        SessionTemporalProjectionBatchV1::new(
            session_id.clone(),
            generation(8),
            watermarks.clone(),
            vec![invalid_occurrence],
            vec![],
            vec![],
        ),
        Err(SessionStoreError::Contract(
            SessionContractError::OccurrenceIdentityMismatch
        ))
    ));

    let mut invalid_copy = copy_record(0, 1);
    invalid_copy.copied_from_occurrence_id = invalid_copy.occurrence_id.clone();
    assert!(matches!(
        SessionTemporalProjectionBatchV1::new(
            session_id.clone(),
            generation(8),
            watermarks.clone(),
            vec![
                occurrence_record(&session_id, 0),
                occurrence_record(&session_id, 1)
            ],
            vec![invalid_copy],
            vec![],
        ),
        Err(SessionStoreError::Contract(
            SessionContractError::CopySelfReference
        ))
    ));

    let mut invalid_assertion = assertion_record(0, 1);
    invalid_assertion.object_anchor_id = invalid_assertion.subject_anchor_id.clone();
    assert!(matches!(
        SessionTemporalProjectionBatchV1::new(
            session_id.clone(),
            generation(8),
            watermarks,
            vec![
                occurrence_record(&session_id, 0),
                occurrence_record(&session_id, 1)
            ],
            vec![],
            vec![invalid_assertion],
        ),
        Err(SessionStoreError::Contract(
            SessionContractError::AssertionSelfReference
        ))
    ));
}

#[test]
fn projection_batches_enforce_record_session_ownership() {
    let session_id = session("session.fixture");
    let watermarks = SessionFrozenWatermarksV1::new(generation(7), 51, 47, 43);
    assert!(matches!(
        SessionTemporalProjectionBatchV1::new(
            session_id.clone(),
            generation(8),
            watermarks.clone(),
            vec![occurrence_record(&session("session.other"), 0)],
            vec![],
            vec![],
        ),
        Err(SessionStoreError::SessionMismatch {
            context: "projection occurrence"
        })
    ));
}
#[test]
fn projection_batches_bind_explicit_contiguous_checkpoint_identity() {
    let session_id = session("session.fixture");
    let batch = SessionTemporalProjectionBatchV1::new(
        session_id,
        generation(8),
        SessionFrozenWatermarksV1::new(generation(7), 51, 47, 43),
        vec![],
        vec![],
        vec![],
    )
    .unwrap()
    .with_checkpoint(3, 41, 37)
    .unwrap();

    assert_eq!(batch.batch_ordinal(), 3);
    assert_eq!(batch.source_through(), 41);
    assert_eq!(batch.projection_through(), 37);
    assert!(matches!(
        batch.clone().with_checkpoint(4, 52, 37),
        Err(SessionStoreError::FrozenWatermarkMismatch)
    ));
    assert!(matches!(
        batch.with_checkpoint(4, 41, 48),
        Err(SessionStoreError::FrozenWatermarkMismatch)
    ));
}
