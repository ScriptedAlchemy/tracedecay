use super::*;

#[test]
fn encrypted_frame_transfer_preserves_exact_frame_and_is_idempotent() {
    let source_fixture = fixture();
    let destination_fixture = fixture();
    let source = storage(&source_fixture);
    let destination = storage(&destination_fixture);
    let capture = admitted();
    let captured = source.capture_pending(&capture).unwrap();
    let transfer = source
        .export_frame_transfer(&captured.event_id, 100)
        .expect("source can export its exact encrypted pending frame");

    let first = destination
        .transfer_pending(&transfer)
        .expect("destination accepts the exact encrypted frame");
    assert_eq!(
        first.disposition,
        RemoteFrameTransferDispositionV1::TransferredPending
    );
    assert_eq!(
        destination
            .load_replay_frame(&captured.event_id)
            .unwrap()
            .capture,
        capture
    );
    assert_eq!(
        destination.transfer_pending(&transfer).unwrap().disposition,
        RemoteFrameTransferDispositionV1::AlreadyTransferred
    );

    let mut tampered = transfer;
    tampered.ciphertext[0] ^= 0x01;
    assert_eq!(
        destination.transfer_pending(&tampered),
        Err(RemoteFrameTransferErrorV1::Corruption)
    );
}

#[test]
fn transferred_frames_cannot_exceed_the_registered_spool_limits() {
    let registered_limits = RemoteSpoolLimitsV1::default();
    assert_eq!(registered_limits.maximum_events, 4_096);
    assert_eq!(registered_limits.maximum_ciphertext_bytes, 64 * 1024 * 1024);

    let source_fixture = fixture();
    let source = storage(&source_fixture);
    let first_capture = admitted();
    let first_receipt = source.capture_pending(&first_capture).unwrap();
    let mut second_capture = admitted();
    second_capture.sequence = RemoteCaptureSequenceV1 {
        sequence: 2,
        previous_event_id: Some(first_receipt.event_id.clone()),
    };
    let second_receipt = source.capture_pending(&second_capture).unwrap();
    let first_transfer = source
        .export_frame_transfer(&first_receipt.event_id, 100)
        .unwrap();
    let second_transfer = source
        .export_frame_transfer(&second_receipt.event_id, 100)
        .unwrap();

    for limits in [
        RemoteSpoolLimitsV1::new(1, u64::MAX).unwrap(),
        RemoteSpoolLimitsV1::new(2, first_transfer.ciphertext.len() as u64).unwrap(),
    ] {
        let destination_fixture = fixture();
        let destination = RemoteSqliteStorageV1::from_registered_with_limits(
            destination_fixture.handle.clone(),
            destination_fixture.binding.clone(),
            Arc::new(TestKeyring(Arc::new(
                RemoteSpoolKeyV1::from_secret_bytes(7, vec![7; 32]).unwrap(),
            ))),
            limits,
        )
        .unwrap();

        destination.transfer_pending(&first_transfer).unwrap();
        assert_eq!(
            destination.transfer_pending(&second_transfer),
            Err(RemoteFrameTransferErrorV1::Overflow)
        );
        assert_eq!(spool_frame_count(&destination_fixture), 1);
        let rows = query(
            &destination_fixture.handle,
            "SELECT COUNT(*) FROM remote_spool_frames WHERE event_id = ?1",
            vec![text(&second_transfer.event_id)],
        )
        .unwrap();
        assert_eq!(row_u64(&rows.rows[0], 0).unwrap(), 0);
        assert_eq!(
            destination
                .transfer_pending(&first_transfer)
                .unwrap()
                .disposition,
            RemoteFrameTransferDispositionV1::AlreadyTransferred
        );
    }
}
