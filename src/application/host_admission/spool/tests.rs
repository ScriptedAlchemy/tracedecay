use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::super::HostAdmissionOutcome;
use super::*;

fn bounds() -> SpoolBounds {
    SpoolBounds::new(64, 16, 1024, 4)
}

fn open_temp() -> (tempfile::TempDir, HostAdmissionSpool) {
    let temp = tempfile::tempdir().unwrap();
    let (spool, _) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    (temp, spool)
}

fn write_frames(path: &Path, sequences: &[u64]) {
    let mut bytes = Vec::new();
    for seq in sequences {
        bytes.extend_from_slice(&encode_frame(*seq, b"a", b"x").unwrap());
    }
    fs::write(path, bytes).unwrap();
}

#[test]
fn frame_encoding_is_deterministic_and_checksummed() {
    let frame = encode_frame(7, b"cursor", b"{\"event\":1}").unwrap();
    assert_eq!(frame, encode_frame(7, b"cursor", b"{\"event\":1}").unwrap());
    assert_eq!(&frame[0..4], FRAME_MAGIC);
    let checksum_at = frame.len() - CHECKSUM_BYTES;
    assert_eq!(
        &frame[checksum_at..],
        Sha256::digest(&frame[..checksum_at]).as_slice()
    );
}

#[test]
fn production_defaults_reserve_capacity_across_sources() {
    let bounds = SpoolBounds::default();
    assert!(bounds.max_records_per_source < bounds.max_records);
    assert!(bounds.max_spool_bytes_per_source < bounds.max_spool_bytes);
    assert!(bounds.max_record_bytes <= bounds.max_spool_bytes_per_source);
}

#[test]
fn append_reopen_ack_and_reopen_are_exact() {
    let (temp, mut spool) = open_temp();
    let first = spool.append("a", b"one").unwrap();
    let second = spool.append("b", b"two").unwrap();
    assert_eq!((first.seq, second.seq), (1, 2));
    drop(spool);

    let (mut spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(report.pending_records, 2);
    assert_eq!(spool.ack(1).unwrap().payload, b"one");
    drop(spool);

    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(report.committed_through, 1);
    assert_eq!(spool.pending_records().len(), 1);
    assert_eq!(spool.pending_records()[0].seq, 2);
    assert_eq!(spool.pending_records()[0].payload, b"two");
}

#[test]
fn partial_tail_is_truncated_but_mid_file_checksum_failure_is_corruption() {
    let (temp, mut spool) = open_temp();
    let first = spool.append("a", b"one").unwrap();
    let records = temp.path().join(RECORDS_FILE);
    let unpublished = encode_frame(2, b"a", b"partial").unwrap();
    let mut crash_meta = spool.meta.clone();
    crash_meta.append_intent = Some(AppendIntentV1::new(
        2,
        first.framed_len as u64,
        &unpublished,
    ));
    write_meta_atomic(&spool.meta_path, &crash_meta).unwrap();
    drop(spool);
    let mut bytes = fs::read(&records).unwrap();
    bytes.extend_from_slice(&unpublished[..=FRAME_HEADER_BYTES]);
    fs::write(&records, bytes).unwrap();
    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(
        report.truncated_partial_tail_bytes,
        (FRAME_HEADER_BYTES + 1) as u64
    );
    assert_eq!(spool.integrity(), &SpoolIntegrity::Healthy);
    assert_eq!(file_len(&records).unwrap(), first.framed_len as u64);
    drop(spool);

    let mut bytes = fs::read(&records).unwrap();
    bytes[first.framed_len - 1] ^= 1;
    let forensic = bytes.clone();
    fs::write(&records, &bytes).unwrap();
    let (mut spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(report.integrity, SpoolIntegrity::Corrupted { at_offset: 0 });
    assert_eq!(report.truncated_partial_tail_bytes, 0);
    assert_eq!(spool.pending_count(), 0);
    assert_eq!(fs::read(&records).unwrap(), forensic);
    assert!(matches!(
        spool.append("a", b"blocked"),
        Err(SpoolError::Corrupted { .. })
    ));
}

#[test]
fn torn_active_header_recovers_and_retry_remains_gap_free_across_restart() {
    let (temp, mut spool) = open_temp();
    let first = spool.append("a", b"one").unwrap();
    let records = temp.path().join(RECORDS_FILE);
    let unpublished = encode_frame(2, b"a", b"torn").unwrap();
    let mut crash_meta = spool.meta.clone();
    crash_meta.append_intent = Some(AppendIntentV1::new(
        2,
        first.framed_len as u64,
        &unpublished,
    ));
    write_meta_atomic(&spool.meta_path, &crash_meta).unwrap();
    drop(spool);
    let mut output = OpenOptions::new().append(true).open(&records).unwrap();
    output.write_all(&unpublished[..3]).unwrap();
    output.sync_all().unwrap();
    drop(output);
    sync_parent_directory(&records).unwrap();

    let (mut spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(report.integrity, SpoolIntegrity::Healthy);
    assert_eq!(report.truncated_partial_tail_bytes, 3);
    assert_eq!(file_len(&records).unwrap(), first.framed_len as u64);
    let retried = spool.append("a", b"torn").unwrap();
    assert_eq!(retried.seq, 2);
    drop(spool);

    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(report.next_seq, 3);
    assert!(spool.meta.append_intent.is_none());
    assert_eq!(
        spool
            .pending_records()
            .iter()
            .map(|record| record.seq)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[test]
fn append_intent_without_frame_is_cleared_and_sequence_is_reused() {
    let (temp, spool) = open_temp();
    let frame = encode_frame(1, b"a", b"retry").unwrap();
    let mut crash_meta = spool.meta.clone();
    crash_meta.append_intent = Some(AppendIntentV1::new(1, 0, &frame));
    write_meta_atomic(&spool.meta_path, &crash_meta).unwrap();
    drop(spool);

    let (mut spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(report.next_seq, 1);
    assert!(spool.meta.append_intent.is_none());
    assert_eq!(spool.append("a", b"retry").unwrap().seq, 1);
}

#[test]
fn ambiguous_short_magic_prefix_without_intent_is_forensic_corruption() {
    let (temp, mut spool) = open_temp();
    let first = spool.append("a", b"one").unwrap();
    drop(spool);
    let records = temp.path().join(RECORDS_FILE);
    let mut forensic = fs::read(&records).unwrap();
    forensic.extend_from_slice(b"TDH");
    fs::write(&records, &forensic).unwrap();

    let (mut spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(
        report.integrity,
        SpoolIntegrity::Corrupted {
            at_offset: first.framed_len as u64
        }
    );
    assert_eq!(report.truncated_partial_tail_bytes, 0);
    assert_eq!(fs::read(&records).unwrap(), forensic);
    assert!(matches!(
        spool.append("a", b"blocked"),
        Err(SpoolError::Corrupted { .. })
    ));
}

#[test]
fn partial_active_frame_must_match_metadata_next_sequence() {
    let (temp, mut spool) = open_temp();
    spool.append("a", b"one").unwrap();
    drop(spool);
    let records = temp.path().join(RECORDS_FILE);
    let mut forensic = fs::read(&records).unwrap();
    let wrong_next = encode_frame(3, b"a", b"unpublished").unwrap();
    forensic.extend_from_slice(&wrong_next[..=FRAME_HEADER_BYTES]);
    fs::write(&records, &forensic).unwrap();

    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(
        report.integrity,
        SpoolIntegrity::Corrupted {
            at_offset: spool.pending_records()[0].framed_len as u64
        }
    );
    assert_eq!(report.truncated_partial_tail_bytes, 0);
    assert_eq!(fs::read(records).unwrap(), forensic);
}

#[test]
fn mid_file_corruption_preserves_forensic_bytes_and_valid_prefix() {
    let (temp, mut spool) = open_temp();
    let first = spool.append("a", b"keep").unwrap();
    let second = spool.append("b", b"corrupt").unwrap();
    drop(spool);
    let records = temp.path().join(RECORDS_FILE);
    let mut bytes = fs::read(&records).unwrap();
    bytes[second.file_offset as usize + second.framed_len - 1] ^= 1;
    let forensic = bytes.clone();
    fs::write(&records, &bytes).unwrap();

    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(
        report.integrity,
        SpoolIntegrity::Corrupted {
            at_offset: first.framed_len as u64
        }
    );
    assert_eq!(report.truncated_partial_tail_bytes, 0);
    assert_eq!(spool.pending_count(), 1);
    assert_eq!(spool.pending_records()[0].payload, b"keep");
    assert_eq!(fs::read(&records).unwrap(), forensic);
    assert_eq!(file_len(&records).unwrap(), forensic.len() as u64);
}

#[test]
fn mid_file_corruption_survives_restart_without_byte_loss() {
    let (temp, mut spool) = open_temp();
    let first = spool.append("a", b"keep").unwrap();
    let second = spool.append("b", b"corrupt").unwrap();
    drop(spool);
    let records = temp.path().join(RECORDS_FILE);
    let mut bytes = fs::read(&records).unwrap();
    bytes[second.file_offset as usize + second.framed_len - 1] ^= 1;
    let forensic = bytes.clone();
    fs::write(&records, &bytes).unwrap();

    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(
        report.integrity,
        SpoolIntegrity::Corrupted {
            at_offset: first.framed_len as u64
        }
    );
    assert_eq!(fs::read(&records).unwrap(), forensic);
    drop(spool);

    let (mut spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(
        report.integrity,
        SpoolIntegrity::Corrupted {
            at_offset: first.framed_len as u64
        }
    );
    assert_eq!(report.truncated_partial_tail_bytes, 0);
    assert_eq!(spool.pending_count(), 1);
    assert_eq!(spool.pending_records()[0].payload, b"keep");
    assert_eq!(fs::read(&records).unwrap(), forensic);
    assert!(matches!(
        spool.ack(first.seq),
        Err(SpoolError::Corrupted { .. })
    ));
}

#[test]
fn oversized_recovery_is_rejected_before_reading() {
    let temp = tempfile::tempdir().unwrap();
    let bounded = SpoolBounds::new(32, 8, 256, 4);
    fs::write(temp.path().join(RECORDS_FILE), vec![0u8; 257]).unwrap();
    assert_eq!(
        HostAdmissionSpool::open(temp.path(), bounded).unwrap_err(),
        SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes)
    );
}

#[test]
fn untrusted_header_length_is_rejected_before_allocation() {
    let temp = tempfile::tempdir().unwrap();
    let mut header = [0u8; FRAME_HEADER_BYTES];
    header[0..4].copy_from_slice(FRAME_MAGIC);
    header[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&1u16.to_le_bytes());
    header[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    header[12..20].copy_from_slice(&1u64.to_le_bytes());
    fs::write(temp.path().join(RECORDS_FILE), header).unwrap();
    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(spool.pending_count(), 0);
    assert_eq!(report.integrity, SpoolIntegrity::Corrupted { at_offset: 0 });
}

#[test]
fn record_count_is_enforced_during_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let bounded = SpoolBounds::new(32, 8, 1024, 2);
    write_frames(&temp.path().join(RECORDS_FILE), &[1, 2, 3]);
    assert_eq!(
        HostAdmissionSpool::open(temp.path(), bounded).unwrap_err(),
        SpoolError::Overflow(SpoolOverflowDisposition::MaxRecords)
    );
}

#[test]
fn duplicate_regressing_and_gapped_sequences_are_corruption() {
    for sequences in [&[1, 1][..], &[1, 3][..]] {
        let temp = tempfile::tempdir().unwrap();
        write_frames(&temp.path().join(RECORDS_FILE), sequences);
        let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
        assert!(matches!(report.integrity, SpoolIntegrity::Corrupted { .. }));
        assert_eq!(spool.pending_count(), 1);
        assert_eq!(spool.pending_records()[0].seq, sequences[0]);
    }
    let temp = tempfile::tempdir().unwrap();
    write_frames(&temp.path().join(RECORDS_FILE), &[2, 1]);
    assert!(matches!(
        HostAdmissionSpool::open(temp.path(), bounds()),
        Err(SpoolError::Corrupted { .. })
    ));
}

#[test]
fn impossible_watermark_is_explicit_corruption() {
    let (temp, mut spool) = open_temp();
    spool.append("a", b"one").unwrap();
    drop(spool);
    write_meta_atomic(
        &temp.path().join(META_FILE),
        &SpoolMetaV1 {
            version: FORMAT_VERSION,
            committed_through: 0,
            next_seq: 9,
            integrity: SpoolIntegrity::Healthy,
            append_intent: None,
        },
    )
    .unwrap();
    let error = HostAdmissionSpool::open(temp.path(), bounds()).unwrap_err();
    assert!(
        matches!(
            error,
            SpoolError::Corrupted { .. } | SpoolError::MetadataCorrupted
        ),
        "impossible watermark must fail closed, got {error:?}"
    );
}

#[test]
fn malformed_empty_and_oversized_metadata_are_typed() {
    let cases = vec![
        b"{".to_vec(),
        Vec::new(),
        vec![b'x'; MAX_META_BYTES as usize + 1],
    ];
    for bytes in cases {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(META_FILE), &bytes).unwrap();
        let error = HostAdmissionSpool::open(temp.path(), bounds()).unwrap_err();
        assert_eq!(error, SpoolError::MetadataCorrupted);
        assert_eq!(error.to_outcome(), HostAdmissionOutcome::spool_corrupted());
    }
}

#[test]
fn unknown_metadata_version_is_typed() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(META_FILE),
        br#"{"version":2,"committed_through":0,"next_seq":1,"integrity":"healthy"}"#,
    )
    .unwrap();
    let error = HostAdmissionSpool::open(temp.path(), bounds()).unwrap_err();
    assert_eq!(error, SpoolError::UnsupportedVersion(2));
    assert_eq!(
        error.to_outcome(),
        HostAdmissionOutcome::spool_unsupported_version()
    );
}

#[test]
fn unknown_frame_version_is_typed() {
    let temp = tempfile::tempdir().unwrap();
    let mut frame = encode_frame(1, b"a", b"x").unwrap();
    frame[4..6].copy_from_slice(&2u16.to_le_bytes());
    fs::write(temp.path().join(RECORDS_FILE), frame).unwrap();
    let error = HostAdmissionSpool::open(temp.path(), bounds()).unwrap_err();
    assert_eq!(error, SpoolError::UnsupportedVersion(2));
    assert_eq!(
        error.to_outcome(),
        HostAdmissionOutcome::spool_unsupported_version()
    );
}

#[test]
fn per_source_durable_limits_preserve_capacity_for_other_sources() {
    let frame_len = encode_frame(1, b"a", b"one").unwrap().len();
    let bounded = SpoolBounds::new(64, 16, frame_len * 4, 4).with_source_limits(frame_len * 2, 2);
    let temp = tempfile::tempdir().unwrap();
    let (mut spool, _) = HostAdmissionSpool::open(temp.path(), bounded).unwrap();

    spool.append("a", b"one").unwrap();
    spool.append("a", b"two").unwrap();
    assert_eq!(
        spool.append("a", b"three"),
        Err(SpoolError::Overflow(
            SpoolOverflowDisposition::MaxRecordsPerSource
        ))
    );
    assert!(spool.append("b", b"one").is_ok());
    assert_eq!(spool.pending_count(), 3);
}

#[test]
fn per_source_durable_byte_limit_is_independent_of_global_capacity() {
    let frame_len = encode_frame(1, b"a", b"one").unwrap().len();
    let bounded = SpoolBounds::new(64, 16, frame_len * 4, 4).with_source_limits(frame_len, 4);
    let temp = tempfile::tempdir().unwrap();
    let (mut spool, _) = HostAdmissionSpool::open(temp.path(), bounded).unwrap();

    spool.append("a", b"one").unwrap();
    assert_eq!(
        spool.append("a", b"two"),
        Err(SpoolError::Overflow(
            SpoolOverflowDisposition::MaxBytesPerSource
        ))
    );
    assert!(spool.append("b", b"one").is_ok());
}

#[test]
fn recovery_enforces_per_source_record_limit() {
    let temp = tempfile::tempdir().unwrap();
    let bounded = SpoolBounds::new(64, 16, 1024, 4).with_source_limits(1024, 2);
    write_frames(&temp.path().join(RECORDS_FILE), &[1, 2, 3]);
    write_meta_atomic(
        &temp.path().join(META_FILE),
        &SpoolMetaV1 {
            version: FORMAT_VERSION,
            committed_through: 0,
            next_seq: 4,
            integrity: SpoolIntegrity::Healthy,
            append_intent: None,
        },
    )
    .unwrap();

    assert_eq!(
        HostAdmissionSpool::open(temp.path(), bounded).unwrap_err(),
        SpoolError::Overflow(SpoolOverflowDisposition::MaxRecordsPerSource)
    );
}

#[test]
fn frame_sync_before_metadata_write_recovers_append_once() {
    let (temp, spool) = open_temp();
    let frame = encode_frame(1, b"a", b"crash-window").unwrap();
    let mut crash_meta = spool.meta.clone();
    crash_meta.append_intent = Some(AppendIntentV1::new(1, 0, &frame));
    write_meta_atomic(&spool.meta_path, &crash_meta).unwrap();
    drop(spool);
    append_frame_durable(&temp.path().join(RECORDS_FILE), &frame).unwrap();
    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(report.next_seq, 2);
    assert!(spool.meta.append_intent.is_none());
    assert_eq!(spool.pending_count(), 1);
    assert_eq!(spool.pending_records()[0].payload, b"crash-window");
}

#[test]
fn durable_ack_watermark_hides_retained_physical_prefix() {
    let (temp, mut spool) = open_temp();
    spool.append("a", b"one").unwrap();
    spool.append("b", b"two").unwrap();
    let records = temp.path().join(RECORDS_FILE);
    let before_ack = fs::read(&records).unwrap();
    spool.ack(1).unwrap();
    // Model crash after metadata watermark while retained physical prefix
    // still contains the acknowledged frame (lazy compaction / failed compact).
    fs::write(&records, before_ack).unwrap();
    drop(spool);

    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(report.committed_through, 1);
    assert_eq!(spool.pending_count(), 1);
    assert_eq!(spool.pending_records()[0].seq, 2);
}

#[test]
fn repeated_meta_and_compaction_replacement_is_portable() {
    let (temp, mut spool) = open_temp();
    spool.append("a", b"one").unwrap();
    spool.append("b", b"two").unwrap();
    spool.ack(1).unwrap();
    spool.append("c", b"three").unwrap();
    drop(spool);

    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(report.committed_through, 1);
    assert_eq!(
        spool
            .pending_records()
            .iter()
            .map(|record| record.seq)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[test]
fn overflow_and_ack_dispositions_are_stable() {
    let (temp, mut spool) = open_temp();
    assert_eq!(
        spool.append("source-name-longer-than-limit", b"x"),
        Err(SpoolError::Overflow(
            SpoolOverflowDisposition::SourceTooLarge
        ))
    );
    assert_eq!(
        spool.append("a", &[0u8; 65]),
        Err(SpoolError::Overflow(
            SpoolOverflowDisposition::RecordTooLarge
        ))
    );
    spool.append("a", b"one").unwrap();
    spool.append("b", b"two").unwrap();
    assert_eq!(
        spool.ack(2),
        Err(SpoolError::AckOutOfOrder {
            expected: 1,
            got: 2
        })
    );
    drop(temp);
}

#[test]
fn append_record_and_byte_backpressure_never_grows_pending_state() {
    let temp = tempfile::tempdir().unwrap();
    let count_bounds = SpoolBounds::new(16, 8, 1024, 1);
    let (mut spool, _) = HostAdmissionSpool::open(temp.path(), count_bounds).unwrap();
    spool.append("a", b"one").unwrap();
    assert_eq!(
        spool.append("b", b"two"),
        Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxRecords))
    );
    assert_eq!(spool.pending_count(), 1);

    let temp = tempfile::tempdir().unwrap();
    let one_frame = encode_frame(1, b"a", b"one").unwrap().len();
    let byte_bounds = SpoolBounds::new(16, 8, one_frame, 4);
    let (mut spool, _) = HostAdmissionSpool::open(temp.path(), byte_bounds).unwrap();
    spool.append("a", b"one").unwrap();
    assert_eq!(
        spool.append("b", b"two"),
        Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes))
    );
    assert_eq!(spool.pending_count(), 1);
}

#[test]
fn ambiguous_append_failure_blocks_every_mutation_until_reopen() {
    let (temp, mut spool) = open_temp();
    spool.append("a", b"one").unwrap();
    let records_path = temp.path().join(RECORDS_FILE);
    let meta_path = temp.path().join(META_FILE);
    let before_second = fs::read(&records_path).unwrap();
    *FAIL_META_WRITE_FOR.lock().unwrap() = Some((meta_path.clone(), 1));

    assert_eq!(spool.append("b", b"two"), Err(SpoolError::Io));
    assert!(spool.recovery_required());
    let ambiguous_bytes = fs::read(&records_path).unwrap();
    assert!(ambiguous_bytes.len() > before_second.len());
    let persisted: SpoolMetaV1 = serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    assert_eq!(
        persisted.append_intent.as_ref().map(|intent| intent.seq),
        Some(2)
    );
    assert_eq!(spool.ack(1), Err(SpoolError::AppendRecoveryRequired));
    assert_eq!(
        spool.ack_through(1),
        Err(SpoolError::AppendRecoveryRequired)
    );
    assert_eq!(
        spool.append("c", b"three"),
        Err(SpoolError::AppendRecoveryRequired)
    );
    assert_eq!(fs::read(&records_path).unwrap(), ambiguous_bytes);
    drop(spool);

    let (mut spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(report.next_seq, 3);
    assert_eq!(
        spool
            .pending_records()
            .iter()
            .map(|record| record.seq)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(spool.ack_through(2).unwrap(), 2);
    assert_eq!(spool.committed_through(), 2);
    assert_eq!(spool.pending_count(), 0);
}

#[test]
fn ack_through_validates_tail_then_publishes_once() {
    let (temp, mut spool) = open_temp();
    spool.append("a", b"one").unwrap();
    spool.append("b", b"two").unwrap();
    spool.append("c", b"three").unwrap();
    let records_path = temp.path().join(RECORDS_FILE);
    let before = fs::read(&records_path).unwrap();

    assert_eq!(spool.ack_through(4), Err(SpoolError::AckUnknown { seq: 4 }));
    assert_eq!(spool.committed_through(), 0);
    assert_eq!(spool.pending_count(), 3);
    assert_eq!(fs::read(&records_path).unwrap(), before);

    assert_eq!(spool.ack_through(2).unwrap(), 2);
    assert_eq!(spool.committed_through(), 2);
    assert_eq!(spool.pending_records()[0].seq, 3);
    assert_eq!(spool.ack_through(1).unwrap(), 0);
    assert_eq!(spool.committed_through(), 2);
    drop(spool);

    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
    assert_eq!(report.committed_through, 2);
    assert_eq!(spool.pending_count(), 1);
    assert_eq!(spool.pending_records()[0].seq, 3);
}

#[test]
fn ack_watermark_defers_full_rewrite_until_waste_threshold() {
    // Every append and every ack publish is a durable `sync_all`, so this
    // test's wall time is `(N + N/2) x ambient fsync latency`. The
    // waste-threshold behavior under test is ratio-based (compact fires
    // when waste crosses the multiplier at N/2), so a modest N proves the
    // same contract; 4096 turned this into a multi-minute fsync storm on
    // a loaded disk.
    const N: usize = 256;
    let frame_len = encode_frame(1, b"s", b"").unwrap().len();
    let bounds = SpoolBounds::new(16, 8, frame_len.saturating_mul(N), N);
    let temp = tempfile::tempdir().unwrap();
    let (mut spool, _) = HostAdmissionSpool::open(temp.path(), bounds).unwrap();
    for _ in 0..N {
        spool.append("s", b"").unwrap();
    }
    let records = temp.path().join(RECORDS_FILE);
    let physical_after_append = file_len(&records).unwrap();
    assert_eq!(physical_after_append, (frame_len * N) as u64);

    // First half of per-seq acks keep waste at or below 2x live pending, so
    // each publish is metadata-only (no full active-file rewrite).
    let half = (N / 2) as u64;
    for seq in 1..=half {
        assert_eq!(spool.ack_through(seq).unwrap(), 1);
        assert_eq!(file_len(&records).unwrap(), physical_after_append);
    }
    assert_eq!(spool.pending_count(), N / 2);
    assert_eq!(spool.committed_through(), half);

    // Crossing the waste multiplier triggers one batched compact.
    assert_eq!(spool.ack_through(half + 1).unwrap(), 1);
    let after_batch = file_len(&records).unwrap();
    assert!(after_batch < physical_after_append);
    assert_eq!(after_batch, (frame_len * (N / 2 - 1)) as u64);
    assert_eq!(spool.pending_count(), N / 2 - 1);

    // Drain remaining live records; empty pending must reclaim to zero.
    assert_eq!(spool.ack_through(N as u64).unwrap(), N / 2 - 1);
    assert_eq!(spool.pending_count(), 0);
    assert_eq!(file_len(&records).unwrap(), 0);
    assert_eq!(spool.committed_through(), N as u64);
}

#[test]
fn terminal_quarantine_preserves_exact_frame_and_reclaims_active_capacity() {
    let temp = tempfile::tempdir().unwrap();
    let exact_frame = encode_frame(1, b"secret-source", b"secret-payload").unwrap();
    let bounded = SpoolBounds::new(64, 16, exact_frame.len(), 1);
    let (mut spool, _) = HostAdmissionSpool::open(temp.path(), bounded).unwrap();
    let terminal = spool.append("secret-source", b"secret-payload").unwrap();

    spool
        .quarantine(terminal.seq, TerminalReason::MalformedPayload)
        .unwrap();

    assert_eq!(spool.pending_count(), 0);
    assert_eq!(spool.quarantine_count(), 1);
    assert_eq!(
        spool.quarantined_record(terminal.seq),
        Some((TerminalReason::MalformedPayload, exact_frame.as_slice()))
    );
    assert_eq!(file_len(&temp.path().join(RECORDS_FILE)).unwrap(), 0);
    assert!(!format!("{spool:?}").contains("secret-payload"));
    assert!(
        spool.append("n", b"x").is_ok(),
        "quarantined records must not consume active byte or record capacity"
    );
}

#[test]
fn quarantine_full_is_typed_and_keeps_terminal_record_active() {
    let temp = tempfile::tempdir().unwrap();
    let bounded = SpoolBounds::new(64, 16, 1024, 1).with_quarantine_limits(1024, 1);
    let (mut spool, _) = HostAdmissionSpool::open(temp.path(), bounded).unwrap();
    let first = spool.append("a", b"first-secret").unwrap();
    spool
        .quarantine(first.seq, TerminalReason::MalformedPayload)
        .unwrap();
    let second = spool.append("b", b"second-secret").unwrap();

    let error = spool
        .quarantine(second.seq, TerminalReason::StaleBranchAuthorization)
        .unwrap_err();

    assert_eq!(error, SpoolError::QuarantineFull);
    assert_eq!(error.to_outcome(), HostAdmissionOutcome::quarantine_full());
    assert_eq!(spool.pending_count(), 1);
    assert_eq!(spool.pending_records()[0].seq, second.seq);
    assert_eq!(spool.quarantine_count(), 1);
    let rendered = format!("{error:?} {:?}", error.to_outcome());
    assert!(!rendered.contains("second-secret"));
}

#[test]
fn quarantine_byte_bound_fails_closed_without_releasing_active_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let bounded = SpoolBounds::new(64, 16, 1024, 4).with_quarantine_limits(1, 4);
    let (mut spool, _) = HostAdmissionSpool::open(temp.path(), bounded).unwrap();
    let terminal = spool.append("a", b"byte-bound-secret").unwrap();
    let active_bytes = spool.pending_bytes;

    assert_eq!(
        spool.quarantine(terminal.seq, TerminalReason::MalformedPayload),
        Err(SpoolError::QuarantineFull)
    );
    assert_eq!(spool.pending_count(), 1);
    assert_eq!(spool.pending_bytes, active_bytes);
    assert_eq!(spool.quarantine_count(), 0);
}

#[test]
fn quarantine_checksum_corruption_is_explicit_on_reopen() {
    let (temp, mut spool) = open_temp();
    let terminal = spool.append("a", b"private-terminal").unwrap();
    spool
        .quarantine(terminal.seq, TerminalReason::MalformedPayload)
        .unwrap();
    drop(spool);

    let quarantine_path = temp.path().join(QUARANTINE_FILE);
    let mut bytes = fs::read(&quarantine_path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&quarantine_path, bytes).unwrap();

    let error = HostAdmissionSpool::open(temp.path(), bounds()).unwrap_err();
    assert!(matches!(error, SpoolError::QuarantineCorrupted { .. }));
    assert_eq!(
        error.to_outcome(),
        HostAdmissionOutcome::quarantine_corrupted()
    );
}

#[test]
fn unproven_quarantine_tail_is_preserved_and_rejected() {
    let (temp, mut spool) = open_temp();
    spool.append("a", b"retry-after-partial").unwrap();
    drop(spool);
    let quarantine = temp.path().join(QUARANTINE_FILE);
    fs::write(&quarantine, b"TDH").unwrap();

    let error = HostAdmissionSpool::open(temp.path(), bounds()).unwrap_err();
    assert_eq!(error, SpoolError::QuarantineCorrupted { at_offset: 0 });
    assert_eq!(fs::read(quarantine).unwrap(), b"TDH");
}

#[test]
fn proven_unpublished_quarantine_append_is_truncated() {
    let (temp, mut spool) = open_temp();
    let terminal = spool.append("a", b"retry-after-partial").unwrap();
    let active_frame = encode_frame(terminal.seq, b"a", b"retry-after-partial").unwrap();
    let frame = quarantine::encode(
        terminal.seq,
        TerminalReason::MalformedPayload,
        &active_frame,
    )
    .unwrap();
    drop(spool);
    let partial_len = FRAME_HEADER_BYTES + 12;
    fs::write(temp.path().join(QUARANTINE_FILE), &frame[..partial_len]).unwrap();

    let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();

    assert_eq!(
        report.quarantine_truncated_partial_tail_bytes,
        partial_len as u64
    );
    assert_eq!(spool.pending_count(), 1);
    assert_eq!(spool.pending_records()[0].seq, terminal.seq);
    assert_eq!(spool.quarantine_count(), 0);
}

#[test]
fn quarantine_retry_is_idempotent_but_reason_mismatch_fences() {
    let (_temp, mut spool) = open_temp();
    let terminal = spool.append("a", b"idempotent").unwrap();
    spool
        .quarantine(terminal.seq, TerminalReason::MalformedPayload)
        .unwrap();

    spool
        .quarantine(terminal.seq, TerminalReason::MalformedPayload)
        .unwrap();
    assert_eq!(spool.quarantine_count(), 1);
    assert_eq!(
        spool.quarantine(terminal.seq, TerminalReason::StaleBranchAuthorization),
        Err(SpoolError::QuarantineCorrupted { at_offset: 0 })
    );
    assert_eq!(
        spool.append("b", b"blocked"),
        Err(SpoolError::QuarantineRecoveryRequired)
    );
}

#[test]
fn ambiguous_terminal_move_fences_mutations_and_reopens_idempotently() {
    for failure in [
        TerminalMoveFailure::AfterQuarantinePublish,
        TerminalMoveFailure::AfterActivePublish,
    ] {
        let (temp, mut spool) = open_temp();
        let terminal = spool.append("a", b"move-boundary-secret").unwrap();
        *FAIL_TERMINAL_MOVE_AT.lock().unwrap() = Some((spool.records_path.clone(), failure));

        assert_eq!(
            spool.quarantine(terminal.seq, TerminalReason::MalformedPayload),
            Err(SpoolError::QuarantineRecoveryRequired)
        );
        assert!(spool.recovery_required());
        assert_eq!(
            spool.append("b", b"blocked"),
            Err(SpoolError::QuarantineRecoveryRequired)
        );
        assert_eq!(
            spool.ack_through(terminal.seq),
            Err(SpoolError::QuarantineRecoveryRequired)
        );
        drop(spool);

        let (mut reopened, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
        assert_eq!(report.quarantined_records, 1);
        assert_eq!(reopened.quarantine_count(), 1);
        assert_eq!(reopened.pending_count(), 0);
        assert!(reopened.append("b", b"after-reopen").is_ok());
    }
}

#[cfg(unix)]
#[test]
fn spool_tightens_directory_and_payload_file_permissions() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("spool");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let records = dir.join(RECORDS_FILE);
    fs::write(&records, []).unwrap();
    fs::set_permissions(&records, fs::Permissions::from_mode(0o644)).unwrap();

    let (mut spool, _) = HostAdmissionSpool::open(&dir, bounds()).unwrap();
    let terminal = spool.append("a", b"private-payload").unwrap();
    spool
        .quarantine(terminal.seq, TerminalReason::MalformedPayload)
        .unwrap();
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&records).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(dir.join(META_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(dir.join(QUARANTINE_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
