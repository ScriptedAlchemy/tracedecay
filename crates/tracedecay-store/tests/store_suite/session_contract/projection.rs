use super::common::*;
use super::*;
use tracedecay_temporal_query::ports::ExecutionControl;

#[test]
fn rebuild_and_activation_validate_session_capability_and_generation_transition() {
    let session_id = session("session.fixture");
    let snapshot = snapshot_for(session_id.clone(), 7);

    for result in [
        SessionGenerationRebuildRequestV1::new(
            session("session.other"),
            generation(8),
            snapshot.clone(),
        )
        .map(|_| ()),
        SessionGenerationActivationRequestV1::new(
            session("session.other"),
            generation(8),
            snapshot.clone(),
            ExecutionControl::default(),
        )
        .map(|_| ()),
    ] {
        assert!(matches!(
            result,
            Err(SessionStoreError::SessionMismatch { .. })
        ));
    }

    for result in [
        SessionGenerationRebuildRequestV1::new(session_id.clone(), generation(7), snapshot.clone())
            .map(|_| ()),
        SessionGenerationActivationRequestV1::new(
            session_id.clone(),
            generation(7),
            snapshot.clone(),
            ExecutionControl::default(),
        )
        .map(|_| ()),
    ] {
        assert!(matches!(
            result,
            Err(SessionStoreError::StaleGeneration { .. })
        ));
    }

    let unsupported = snapshot_with_capabilities(
        session_id.clone(),
        [SessionTemporalCapabilityV1::FrozenWatermarks],
    );
    assert!(matches!(
        SessionGenerationRebuildRequestV1::new(
            session_id.clone(),
            generation(8),
            unsupported.clone(),
        ),
        Err(SessionStoreError::UnsupportedCapability {
            capability: SessionTemporalCapabilityV1::GenerationRebuild
        })
    ));
    assert!(matches!(
        SessionGenerationActivationRequestV1::new(
            session_id,
            generation(8),
            unsupported,
            ExecutionControl::default(),
        ),
        Err(SessionStoreError::UnsupportedCapability {
            capability: SessionTemporalCapabilityV1::GenerationRebuild
        })
    ));
}
#[test]
fn generation_receipts_derive_identity_and_reject_activation_mismatch() {
    let session_id = session("session.fixture");
    let snapshot = snapshot_for(session_id.clone(), 7);
    let rebuild_request =
        SessionGenerationRebuildRequestV1::new(session_id.clone(), generation(8), snapshot.clone())
            .unwrap();
    let rebuild = SessionGenerationRebuildReceiptV1::new(
        &rebuild_request,
        SessionGenerationRebuildDispositionV1::Started,
        UtcMicros(100),
    )
    .unwrap();
    assert_eq!(rebuild.session_id(), &session_id);
    assert_eq!(rebuild.generation(), generation(8));

    let activated_watermarks = SessionFrozenWatermarksV1::new(generation(8), 51, 47, 43);
    let activation_request = SessionGenerationActivationRequestV1::new(
        session_id.clone(),
        generation(8),
        snapshot,
        ExecutionControl::default(),
    )
    .unwrap();
    assert!(matches!(
        SessionGenerationActivationReceiptV1::new(
            &activation_request,
            SessionFrozenWatermarksV1::new(generation(8), 52, 47, 43),
            UtcMicros(100),
        ),
        Err(SessionStoreError::ReceiptIdentityMismatch {
            context: "generation activation"
        })
    ));
    let receipt = SessionGenerationActivationReceiptV1::new(
        &activation_request,
        activated_watermarks,
        UtcMicros(100),
    )
    .unwrap();
    assert_eq!(receipt.generation(), generation(8));
    assert_eq!(receipt.previous_generation(), Some(generation(7)));
}

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

#[test]
fn rebuild_dispositions_form_a_monotonic_state_machine() {
    let session_id = session("session.rebuild-state");
    let request = SessionGenerationRebuildRequestV1::new(
        session_id,
        generation(8),
        snapshot_for(session("session.rebuild-state"), 7),
    )
    .unwrap();
    let started = SessionGenerationRebuildReceiptV1::new(
        &request,
        SessionGenerationRebuildDispositionV1::Started,
        UtcMicros(100),
    )
    .unwrap();
    let complete = SessionGenerationRebuildReceiptV1::new(
        &request,
        SessionGenerationRebuildDispositionV1::Complete,
        UtcMicros(101),
    )
    .unwrap();
    let resumed = SessionGenerationRebuildReceiptV1::new(
        &request,
        SessionGenerationRebuildDispositionV1::Resumed,
        UtcMicros(102),
    )
    .unwrap();
    assert!(started.validate_successor(&complete).is_ok());
    assert!(matches!(
        complete.validate_successor(&resumed),
        Err(SessionStoreError::InvalidStateTransition {
            context: "generation rebuild successor"
        })
    ));
}
