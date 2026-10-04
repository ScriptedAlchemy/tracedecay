use std::io::{Seek, SeekFrom, Write};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::*;
use crate::{HookCapabilityV1, HookEventFamily, HookEventSupportV1, HookEventV2, HookOrderingV1};
use tracedecay_domain::NativeHostIdentityV1;

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "tracedecay-hooks-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // The spool accepts an existing root only when it is private, so the
        // fixture must create it through the same authority.
        tracedecay_private_fs::create_private_directory(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn config() -> HookSpoolConfigV1 {
    HookSpoolConfigV1 {
        host: NativeHostIdentityV1::CursorDesktop,
        limits: HookSpoolLimitsV1 {
            max_host_records: 8,
            max_host_bytes: 32 * 1024,
            max_session_records: 4,
            max_session_bytes: 16 * 1024,
        },
        writer_lease_micros: 100,
    }
}

fn binding() -> HookScopeBindingV1 {
    HookScopeBindingV1 {
        host: NativeHostIdentityV1::CursorDesktop,
        project_id: [1; 16],
        repository_id: [2; 16],
        worktree_id: [3; 16],
        worktree_epoch: 4,
        binding_token: [7; 32],
        capabilities: vec![
            HookCapabilityV1 {
                family: HookEventFamily::SessionBoundary,
                support: HookEventSupportV1::Native,
            },
            HookCapabilityV1 {
                family: HookEventFamily::SavedEdit,
                support: HookEventSupportV1::Native,
            },
        ],
    }
}

fn envelope(event: u8, session: u8) -> HookEventEnvelopeV2 {
    HookEventEnvelopeV2 {
        schema_version: crate::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [event; 16],
        producer: NativeHostIdentityV1::CursorDesktop,
        protected_session_id: [session; 32],
        project_id: [1; 16],
        repository_id: [2; 16],
        worktree_id: [3; 16],
        worktree_epoch: 4,
        binding_token: [7; 32],
        ordering: HookOrderingV1::ProviderSequence(event as u64),
        observed_at: UtcMicros(10),
        event: HookEventV2::SessionBoundary {
            boundary: crate::HookBoundaryV1::Start,
        },
    }
}

fn numbered_envelope(event: u32, session: u8) -> HookEventEnvelopeV2 {
    let mut numbered = envelope((event % 251) as u8 + 1, session);
    numbered.event_id = [0; 16];
    numbered.event_id[..4].copy_from_slice(&event.to_le_bytes());
    numbered.ordering = HookOrderingV1::ProviderSequence(u64::from(event));
    numbered
}

fn regular_envelope(event: u8, session: u8) -> HookEventEnvelopeV2 {
    HookEventEnvelopeV2 {
        event: HookEventV2::SavedEdit {
            file_id: [event; 16],
            changed_range_count: 1,
        },
        ..envelope(event, session)
    }
}

fn lifecycle_envelope(event: u32, session: u8) -> HookEventEnvelopeV2 {
    let mut envelope = numbered_envelope(event, session);
    envelope.event = if event.is_multiple_of(2) {
        HookEventV2::ToolLifecycle {
            tool_id: [event as u8; 16],
            phase: crate::HookLifecyclePhaseV1::Completed,
            effect_receipt_id: Some([event.wrapping_add(1) as u8; 16]),
        }
    } else {
        HookEventV2::TestLifecycle {
            test_run_id: [event as u8; 16],
            test_count: 128,
            phase: crate::HookLifecyclePhaseV1::Completed,
            receipt_id: Some([event.wrapping_add(1) as u8; 16]),
        }
    };
    envelope
}

fn binding_for_envelopes(
    host: NativeHostIdentityV1,
    envelopes: &[HookEventEnvelopeV2],
) -> HookScopeBindingV1 {
    let mut binding = binding();
    binding.host = host;
    binding.capabilities.clear();
    for family in envelopes.iter().map(|envelope| envelope.event.family()) {
        if !binding
            .capabilities
            .iter()
            .any(|capability| capability.family == family)
        {
            binding.capabilities.push(HookCapabilityV1 {
                family,
                support: crate::stock_event_support(host, family),
            });
        }
    }
    binding
}

fn publish_checkpoint(
    root: &Path,
    config: HookSpoolConfigV1,
    envelopes: &[HookEventEnvelopeV2],
) -> Vec<HookSpoolRecordV1> {
    let (mut spool, _) = HookSpoolV1::open(root, config, UtcMicros(10)).unwrap();
    let binding = binding_for_envelopes(config.host, envelopes);
    let records = envelopes
        .iter()
        .cloned()
        .map(|envelope| spool.append(envelope, &binding, UtcMicros(10)).unwrap())
        .collect::<Vec<_>>();
    drop(spool);
    fs::remove_file(checkpoint_path(root)).unwrap();
    let (spool, report) = HookSpoolV1::open(root, config, UtcMicros(11)).unwrap();
    assert!(report.checkpoint_rewritten);
    drop(spool);
    records
}

fn checkpoint_header_json_len(bytes: &[u8]) -> usize {
    u32::from_le_bytes(
        bytes[CHECKPOINT_HEADER_BYTES..CHECKPOINT_HEADER_BYTES + 4]
            .try_into()
            .unwrap(),
    ) as usize
}

#[test]
fn checksum_is_real_sha256() {
    assert_eq!(
        hook_spool_checksum(b"abc"),
        [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ]
    );
}

/// A group-writable existing spool root fails closed on open and keeps its
/// mode: per-file modes cannot protect members while the directory itself
/// stays group-writable, and the spool never re-permissions a root it did not
/// create.
#[cfg(unix)]
#[test]
fn open_refuses_a_group_writable_existing_root_without_rewriting_it() {
    use std::os::unix::fs::PermissionsExt;
    let root = TestDir::new("permissive-root");
    fs::set_permissions(&root.0, fs::Permissions::from_mode(0o770)).unwrap();

    assert!(matches!(
        HookSpoolV1::open(&root.0, config(), UtcMicros(10)),
        Err(HookSpoolError::UnsafePath)
    ));
    let mode = fs::metadata(&root.0).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o770,
        "a refused root keeps the mode it was found with"
    );
}

#[test]
fn nonfinal_meta_version_requires_explicit_reset_with_exact_provenance() {
    let root = TestDir::new("reset-meta-version");
    let (spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    let mut meta = spool.meta.clone();
    meta.version = SPOOL_META_VERSION.saturating_add(1);
    drop(spool);
    write_meta(&root.0, &meta).unwrap();

    assert_eq!(
        HookSpoolV1::open(&root.0, config(), UtcMicros(20)).unwrap_err(),
        HookSpoolError::ResetRequired {
            reason: HookSpoolResetReasonV1::MetadataVersion {
                found: SPOOL_META_VERSION.saturating_add(1),
                expected: SPOOL_META_VERSION,
            },
        }
    );
}

#[test]
fn nonfinal_meta_shape_requires_explicit_reset() {
    let root = TestDir::new("reset-meta-shape");
    let (spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    drop(spool);
    let mut meta: serde_json::Value =
        serde_json::from_slice(&fs::read(meta_path(&root.0)).unwrap()).unwrap();
    meta.as_object_mut()
        .unwrap()
        .insert("retired_cursor".to_owned(), serde_json::json!(7));
    fs::write(meta_path(&root.0), serde_json::to_vec(&meta).unwrap()).unwrap();

    assert_eq!(
        HookSpoolV1::open(&root.0, config(), UtcMicros(20)).unwrap_err(),
        HookSpoolError::ResetRequired {
            reason: HookSpoolResetReasonV1::MetadataShape,
        }
    );
}

#[test]
fn nonfinal_frame_header_requires_explicit_reset_with_exact_provenance() {
    let root = TestDir::new("reset-frame");
    let payload = canonical_json_bytes(&envelope(1, 9)).unwrap();
    let mut frame = encode_frame(1, UtcMicros(10), [9; 32], &payload).unwrap();
    frame[4..8].copy_from_slice(b"TDH1");
    frame[8..10].copy_from_slice(&2u16.to_le_bytes());
    fs::write(records_path(&root.0), frame).unwrap();

    assert_eq!(
        HookSpoolV1::open(&root.0, config(), UtcMicros(20)).unwrap_err(),
        HookSpoolError::ResetRequired {
            reason: HookSpoolResetReasonV1::FrameFormat {
                found_magic: *b"TDH1",
                found_version: 2,
                expected_magic: *SPOOL_MAGIC,
                expected_version: SPOOL_FORMAT_VERSION,
            },
        }
    );
}

#[test]
fn nonfinal_envelope_shape_in_final_frame_requires_explicit_reset() {
    let root = TestDir::new("reset-envelope-shape");
    let mut payload = serde_json::to_value(envelope(1, 9)).unwrap();
    payload
        .as_object_mut()
        .unwrap()
        .insert("authorization_epoch".to_owned(), serde_json::json!(41));
    let payload = serde_json::to_vec(&payload).unwrap();
    let frame = encode_frame(1, UtcMicros(10), [9; 32], &payload).unwrap();
    fs::write(records_path(&root.0), frame).unwrap();

    assert_eq!(
        HookSpoolV1::open(&root.0, config(), UtcMicros(20)).unwrap_err(),
        HookSpoolError::ResetRequired {
            reason: HookSpoolResetReasonV1::EnvelopeShape,
        }
    );
}

#[test]
fn nonfinal_envelope_version_requires_explicit_reset_with_exact_provenance() {
    let root = TestDir::new("reset-envelope-version");
    let mut payload = serde_json::to_value(envelope(1, 9)).unwrap();
    payload["schema_version"] =
        serde_json::json!(crate::HOOK_EVENT_SCHEMA_VERSION.saturating_add(1));
    let payload = serde_json::to_vec(&payload).unwrap();
    let frame = encode_frame(1, UtcMicros(10), [9; 32], &payload).unwrap();
    fs::write(records_path(&root.0), frame).unwrap();

    assert_eq!(
        HookSpoolV1::open(&root.0, config(), UtcMicros(20)).unwrap_err(),
        HookSpoolError::ResetRequired {
            reason: HookSpoolResetReasonV1::EnvelopeVersion {
                found: crate::HOOK_EVENT_SCHEMA_VERSION.saturating_add(1),
                expected: crate::HOOK_EVENT_SCHEMA_VERSION,
            },
        }
    );
}

#[test]
fn explicit_reset_recreates_nonfinal_spool_without_decoding_it() {
    let root = TestDir::new("explicit-reset");
    let (spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    drop(spool);
    fs::write(
        meta_path(&root.0),
        b"{\"version\":999,\"opaque\":\"not decoded\"}",
    )
    .unwrap();
    fs::write(records_path(&root.0), b"nonfinal records").unwrap();
    fs::write(replay_cursor_path(&root.0), b"nonfinal cursor").unwrap();

    HookSpoolV1::reset(&root.0, config(), UtcMicros(20)).unwrap();

    let (_, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(30)).unwrap();
    assert_eq!(report.pending_records, 0);
    assert_eq!(report.next_sequence, 1);
}

#[test]
fn append_ack_compact_and_reopen_are_exact() {
    let root = TestDir::new("ack");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    let first = spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    let second = spool
        .append(envelope(2, 10), &binding(), UtcMicros(10))
        .unwrap();
    spool
        .acknowledge(
            HookSpoolAckV1 {
                sequence: second.sequence,
                receipt_id: [22; 16],
                disposition: HookSpoolAckDispositionV1::Committed,
            },
            UtcMicros(10),
        )
        .unwrap();
    spool
        .acknowledge(
            HookSpoolAckV1 {
                sequence: first.sequence,
                receipt_id: [21; 16],
                disposition: HookSpoolAckDispositionV1::Committed,
            },
            UtcMicros(10),
        )
        .unwrap();
    drop(spool);
    let (spool, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(20)).unwrap();
    assert_eq!(report.committed_through, 2);
    assert_eq!(report.scanned_records, 0);
    assert!(spool.pending.is_empty());
    assert_eq!(fs::metadata(records_path(&root.0)).unwrap().len(), 0);
}

#[test]
fn one_batch_acknowledgement_settles_every_valid_slot_and_reports_each_conflict() {
    let root = TestDir::new("ack-many");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    let first = spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    let second = spool
        .append(envelope(2, 10), &binding(), UtcMicros(10))
        .unwrap();
    let third = spool
        .append(envelope(3, 11), &binding(), UtcMicros(10))
        .unwrap();
    let ack = |sequence, receipt| HookSpoolAckV1 {
        sequence,
        receipt_id: [receipt; 16],
        disposition: HookSpoolAckDispositionV1::Committed,
    };

    let outcomes = spool
        .acknowledge_many(
            &[
                ack(third.sequence, 33),
                ack(first.sequence, 31),
                // A second, different receipt for the same record conflicts.
                ack(first.sequence, 99),
                // The identical receipt repeated is a no-op.
                ack(third.sequence, 33),
                ack(42, 44),
            ],
            UtcMicros(10),
        )
        .unwrap();

    assert_eq!(
        outcomes,
        vec![
            Ok(true),
            Ok(true),
            Err(HookSpoolError::AckConflict),
            Ok(false),
            Err(HookSpoolError::AckConflict),
        ]
    );
    drop(spool);
    let (spool, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(20)).unwrap();
    assert_eq!(report.committed_through, first.sequence);
    assert_eq!(
        spool
            .pending
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![second.sequence]
    );
}

#[test]
fn replay_probe_is_false_until_a_record_is_appended() {
    let root = TestDir::new("replay-probe");
    assert!(!HookSpoolV1::has_records(&root.0).unwrap());

    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();

    assert!(HookSpoolV1::has_records(&root.0).unwrap());
}

#[test]
fn identical_event_id_and_envelope_reuses_pending_record_after_reopen() {
    let root = TestDir::new("dedupe");
    let mut config = config();
    config.limits.max_session_records = 1;
    let (mut spool, _) = HookSpoolV1::open(&root.0, config, UtcMicros(10)).unwrap();
    let first = spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    let physical_len = spool.physical_len;
    drop(spool);
    let (mut spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(11)).unwrap();
    assert_eq!(report.scanned_records, 1);

    let duplicate = spool
        .append(envelope(1, 9), &binding(), UtcMicros(11))
        .unwrap();

    assert_eq!(duplicate, first);
    assert_eq!(spool.pending[0].to_record(), Some(first));
    assert_eq!(spool.meta.next_sequence, 2);
    assert_eq!(spool.physical_len, physical_len);
}

#[test]
fn reused_event_id_with_different_envelope_is_rejected_after_reopen() {
    let root = TestDir::new("event-id-conflict");
    let mut config = config();
    config.limits.max_session_records = 1;
    let (mut spool, _) = HookSpoolV1::open(&root.0, config, UtcMicros(10)).unwrap();
    spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    drop(spool);
    let (mut spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(11)).unwrap();
    assert_eq!(report.scanned_records, 1);
    let mut conflicting = envelope(1, 9);
    conflicting.observed_at = UtcMicros(11);

    assert_eq!(
        spool
            .append(conflicting, &binding(), UtcMicros(11))
            .unwrap_err(),
        HookSpoolError::EventIdConflict
    );
    assert_eq!(spool.pending.len(), 1);
    assert_eq!(spool.meta.next_sequence, 2);
}

/// Under the writer lease an append writes only its frame (and the
/// rebuildable checkpoint transition): the metadata file is not rewritten, so
/// a contended hook holds the lease for no metadata barrier, and reopening
/// still continues the sequence from the committed frame.
#[test]
fn an_append_writes_only_its_frame_and_reopen_continues_the_sequence() {
    let root = TestDir::new("append-writes-frame");
    let (spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    drop(spool);
    let meta_before = fs::read(meta_path(&root.0)).unwrap();

    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    let record = spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    spool.commit().unwrap();

    assert_eq!(fs::read(meta_path(&root.0)).unwrap(), meta_before);
    assert_eq!(
        fs::metadata(records_path(&root.0)).unwrap().len(),
        u64::from(record.framed_len)
    );
    let (spool, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(11)).unwrap();
    assert_eq!(report.next_sequence, 2);
    assert_eq!(spool.pending.len(), 1);
    assert_eq!(spool.pending[0].to_record(), Some(record));
}

#[test]
fn checkpoint_anchor_is_reused_until_the_suffix_threshold() {
    let root = TestDir::new("checkpoint-suffix");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    for event in 1..=4 {
        spool
            .append(envelope(event, event + 8), &binding(), UtcMicros(10))
            .unwrap();
    }
    drop(spool);

    let (mut spool, first_reopen) = HookSpoolV1::open(&root.0, config(), UtcMicros(11)).unwrap();
    assert_eq!(first_reopen.scanned_records, 4);
    assert!(!first_reopen.checkpoint_rewritten);
    spool
        .append(envelope(5, 13), &binding(), UtcMicros(11))
        .unwrap();
    drop(spool);

    let (spool, second_reopen) = HookSpoolV1::open(&root.0, config(), UtcMicros(12)).unwrap();
    assert_eq!(second_reopen.pending_records, 5);
    assert_eq!(
        second_reopen.scanned_records, 5,
        "the empty anchor remains reusable until its suffix reaches the rewrite threshold"
    );
    assert!(!second_reopen.checkpoint_rewritten);
    assert_eq!(spool.pending.len(), 5);
    drop(spool);

    let (restarted, restart_report) = HookSpoolV1::open(&root.0, config(), UtcMicros(13)).unwrap();
    assert_eq!(restart_report.scanned_records, 5);
    assert!(!restart_report.checkpoint_rewritten);
    assert_eq!(restarted.pending.len(), 5);
}

#[test]
fn checkpoint_rewrites_are_amortized_across_dispatches() {
    const INITIAL_RECORDS: u32 = 400;
    const DISPATCHES: u32 = 24;

    let root = TestDir::new("checkpoint-amortized");
    let config = HookSpoolConfigV1::stock(NativeHostIdentityV1::CursorDesktop);
    let (mut spool, _) = HookSpoolV1::open(&root.0, config, UtcMicros(10)).unwrap();
    for event in 1..=INITIAL_RECORDS {
        spool
            .append(numbered_envelope(event, 9), &binding(), UtcMicros(10))
            .unwrap();
    }
    drop(spool);

    let (spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(11)).unwrap();
    assert!(report.checkpoint_rewritten);
    drop(spool);
    let mut checkpoint_rewrites = 1u32;

    for event in (INITIAL_RECORDS + 1)..=(INITIAL_RECORDS + DISPATCHES) {
        let (mut spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(11)).unwrap();
        checkpoint_rewrites += u32::from(report.checkpoint_rewritten);
        spool
            .append(numbered_envelope(event, 9), &binding(), UtcMicros(11))
            .unwrap();
    }

    let (spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(12)).unwrap();
    checkpoint_rewrites += u32::from(report.checkpoint_rewritten);
    assert!(
        checkpoint_rewrites <= DISPATCHES.div_ceil(CHECKPOINT_REWRITE_FRAME_THRESHOLD) + 1,
        "{checkpoint_rewrites} checkpoint rewrites for {DISPATCHES} dispatches"
    );
    assert!(report.scanned_records <= CHECKPOINT_REWRITE_FRAME_THRESHOLD);
    assert_eq!(
        spool
            .pending
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        (1..=u64::from(INITIAL_RECORDS + DISPATCHES)).collect::<Vec<_>>()
    );
    assert_eq!(
        spool
            .pending
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        (1..=INITIAL_RECORDS + DISPATCHES)
            .map(|event| numbered_envelope(event, 9).event_id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn checkpoint_stores_a_fixed_width_index_without_envelopes() {
    const RECORDS: u32 = 40;

    let compact_root = TestDir::new("checkpoint-fixed-compact");
    let large_root = TestDir::new("checkpoint-fixed-large");
    let compact_config = HookSpoolConfigV1::stock(NativeHostIdentityV1::Hermes);
    let large_config = HookSpoolConfigV1::stock(NativeHostIdentityV1::Hermes);
    let compact = (1..=RECORDS)
        .map(|event| {
            let mut envelope = numbered_envelope(event, 9);
            envelope.producer = NativeHostIdentityV1::Hermes;
            envelope
        })
        .collect::<Vec<_>>();
    let large = (1..=RECORDS)
        .map(|event| {
            let mut envelope = lifecycle_envelope(event, 9);
            envelope.producer = NativeHostIdentityV1::Hermes;
            envelope
        })
        .collect::<Vec<_>>();
    publish_checkpoint(&compact_root.0, compact_config, &compact);
    publish_checkpoint(&large_root.0, large_config, &large);

    let compact_bytes = fs::read(checkpoint_path(&compact_root.0)).unwrap();
    let large_bytes = fs::read(checkpoint_path(&large_root.0)).unwrap();
    let compact_header_len = checkpoint_header_json_len(&compact_bytes);
    let large_header_len = checkpoint_header_json_len(&large_bytes);
    let expected_compact = CHECKPOINT_HEADER_BYTES
        + 4
        + compact_header_len
        + RECORDS as usize * CHECKPOINT_ENTRY_BYTES
        + FRAME_CHECKSUM_BYTES;
    let expected_large = CHECKPOINT_HEADER_BYTES
        + 4
        + large_header_len
        + RECORDS as usize * CHECKPOINT_ENTRY_BYTES
        + FRAME_CHECKSUM_BYTES;
    assert_eq!(compact_bytes.len(), expected_compact);
    assert_eq!(large_bytes.len(), expected_large);
    assert_eq!(compact_bytes.len(), large_bytes.len());
    assert_eq!(
        &compact_bytes[..CHECKPOINT_HEADER_BYTES],
        &large_bytes[..CHECKPOINT_HEADER_BYTES]
    );
    let compact_entries_at = CHECKPOINT_HEADER_BYTES + 4 + compact_header_len;
    let large_entries_at = CHECKPOINT_HEADER_BYTES + 4 + large_header_len;
    assert_ne!(
        &compact_bytes[compact_entries_at..compact_bytes.len() - FRAME_CHECKSUM_BYTES],
        &large_bytes[large_entries_at..large_bytes.len() - FRAME_CHECKSUM_BYTES]
    );

    let (compact_spool, compact_report) =
        HookSpoolV1::open(&compact_root.0, compact_config, UtcMicros(12)).unwrap();
    assert_eq!(compact_report.checkpoint_records, RECORDS);
    assert_eq!(compact_report.checkpoint_bytes, compact_bytes.len() as u64);
    assert_eq!(compact_report.scanned_records, 0);
    drop(compact_spool);
    let (large_spool, large_report) =
        HookSpoolV1::open(&large_root.0, large_config, UtcMicros(12)).unwrap();
    assert_eq!(large_report.checkpoint_records, RECORDS);
    assert_eq!(large_report.checkpoint_bytes, large_bytes.len() as u64);
    assert_eq!(large_report.scanned_records, 0);
    drop(large_spool);
}

#[test]
fn open_after_one_dispatch_decodes_only_the_appended_suffix() {
    const INITIAL_RECORDS: u32 = 3_000;
    const DISPATCHES: u32 = 5;

    let root = TestDir::new("checkpoint-suffix-materialization");
    let config = HookSpoolConfigV1::stock(NativeHostIdentityV1::CursorDesktop);
    let initial = (1..=INITIAL_RECORDS)
        .map(|event| numbered_envelope(event, (event % 251) as u8 + 1))
        .collect::<Vec<_>>();
    publish_checkpoint(&root.0, config, &initial);

    for suffix_records in 0..DISPATCHES {
        let (mut spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(12)).unwrap();
        assert_eq!(report.checkpoint_records, INITIAL_RECORDS);
        assert_eq!(report.scanned_records, suffix_records);
        spool
            .append(
                numbered_envelope(
                    INITIAL_RECORDS + suffix_records + 1,
                    ((INITIAL_RECORDS + suffix_records + 1) % 251) as u8 + 1,
                ),
                &binding(),
                UtcMicros(12),
            )
            .unwrap();
    }

    let (spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(13)).unwrap();
    assert_eq!(report.checkpoint_records, INITIAL_RECORDS);
    assert_eq!(report.scanned_records, DISPATCHES);
    assert_eq!(
        spool
            .pending
            .iter()
            .filter(|entry| entry.envelope.is_some())
            .count(),
        report.scanned_records as usize
    );
    assert_eq!(report.pending_records, INITIAL_RECORDS + DISPATCHES);
}

#[test]
fn hydration_detects_a_corrupted_checkpointed_frame() {
    let root = TestDir::new("checkpoint-hydration-corruption");
    let config = HookSpoolConfigV1::stock(NativeHostIdentityV1::CursorDesktop);
    let first = numbered_envelope(1, 9);
    publish_checkpoint(&root.0, config, &[first.clone(), numbered_envelope(2, 9)]);
    let (mut spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(12)).unwrap();
    assert_eq!(report.checkpoint_records, 2);
    assert_eq!(report.scanned_records, 0);

    let mut records = std::fs::OpenOptions::new()
        .write(true)
        .open(records_path(&root.0))
        .unwrap();
    records.seek(SeekFrom::Start(70)).unwrap();
    records.write_all(&[0xff]).unwrap();
    records.sync_all().unwrap();
    drop(records);

    assert_eq!(
        spool.pending_envelope(first.event_id),
        Err(HookSpoolError::MetadataCorrupted)
    );
    assert_eq!(
        spool
            .append(numbered_envelope(3, 9), &binding(), UtcMicros(12))
            .unwrap_err(),
        HookSpoolError::RecoveryRequired
    );
    drop(spool);

    let (spool, reopened) = HookSpoolV1::open(&root.0, config, UtcMicros(13)).unwrap();
    assert_eq!(reopened.corrupted_at_offset, Some(0));
    assert!(matches!(
        spool.ensure_healthy(),
        Err(HookSpoolError::Corrupted { at_offset: 0 })
    ));
}

#[test]
fn hydration_rejects_a_wrong_checkpoint_index_without_poisoning_records() {
    let root = TestDir::new("checkpoint-hydration-index-mismatch");
    let config = HookSpoolConfigV1::stock(NativeHostIdentityV1::CursorDesktop);
    let first = numbered_envelope(1, 9);
    let envelopes = [first.clone(), numbered_envelope(2, 10)];
    publish_checkpoint(&root.0, config, &envelopes);

    let mut checkpoint = fs::read(checkpoint_path(&root.0)).unwrap();
    let header_len = checkpoint_header_json_len(&checkpoint);
    let queued_at = CHECKPOINT_HEADER_BYTES + 4 + header_len + 8;
    checkpoint[queued_at..queued_at + 8].copy_from_slice(&UtcMicros(999).0.to_le_bytes());
    let checksum_at = checkpoint.len() - FRAME_CHECKSUM_BYTES;
    let checksum = frame_checksum(&checkpoint[..checksum_at]);
    checkpoint[checksum_at..].copy_from_slice(&checksum);
    fs::write(checkpoint_path(&root.0), checkpoint).unwrap();

    let (mut spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(12)).unwrap();
    assert_eq!(report.checkpoint_records, envelopes.len() as u32);
    assert_eq!(report.scanned_records, 0);
    assert_eq!(
        spool.pending_envelope(first.event_id),
        Err(HookSpoolError::MetadataCorrupted)
    );
    assert_eq!(
        spool
            .append(numbered_envelope(3, 11), &binding(), UtcMicros(12))
            .unwrap_err(),
        HookSpoolError::RecoveryRequired
    );
    drop(spool);

    let (mut spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(13)).unwrap();
    assert_eq!(report.checkpoint_records, 0);
    assert_eq!(report.scanned_records, envelopes.len() as u32);
    assert!(report.checkpoint_rewritten);
    assert_eq!(report.corrupted_at_offset, None);
    assert_eq!(spool.pending_envelope(first.event_id), Ok(Some(first)));
}

#[test]
fn checkpoint_body_version_one_is_rejected_and_rewritten() {
    let root = TestDir::new("checkpoint-old-body");
    let config = HookSpoolConfigV1::stock(NativeHostIdentityV1::CursorDesktop);
    let envelopes = [numbered_envelope(1, 9), numbered_envelope(2, 9)];
    publish_checkpoint(&root.0, config, &envelopes);

    let body = b"{\"version\":1}";
    let mut old_checkpoint = Vec::new();
    old_checkpoint.extend_from_slice(CHECKPOINT_MAGIC);
    old_checkpoint.extend_from_slice(&1u16.to_le_bytes());
    old_checkpoint.extend_from_slice(&(body.len() as u64).to_le_bytes());
    old_checkpoint.extend_from_slice(body);
    let checksum = frame_checksum(&old_checkpoint);
    old_checkpoint.extend_from_slice(&checksum);
    fs::write(checkpoint_path(&root.0), old_checkpoint).unwrap();

    let (spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(12)).unwrap();
    assert_eq!(report.scanned_records, envelopes.len() as u32);
    assert_eq!(report.checkpoint_bytes, 0);
    assert!(report.checkpoint_rewritten);
    drop(spool);
    let rewritten = fs::read(checkpoint_path(&root.0)).unwrap();
    assert_eq!(
        u16::from_le_bytes([rewritten[4], rewritten[5]]),
        CHECKPOINT_FORMAT_VERSION
    );
}

#[test]
fn compaction_copies_surviving_checkpointed_frames_byte_exactly() {
    let root = TestDir::new("checkpoint-compaction-bytes");
    let config = HookSpoolConfigV1::stock(NativeHostIdentityV1::CursorDesktop);
    let envelopes = [
        numbered_envelope(1, 9),
        numbered_envelope(2, 10),
        numbered_envelope(3, 11),
    ];
    publish_checkpoint(&root.0, config, &envelopes);
    let (mut spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(12)).unwrap();
    assert_eq!(report.checkpoint_records, 3);
    assert!(spool.pending.iter().all(|entry| entry.envelope.is_none()));
    let original = fs::read(records_path(&root.0)).unwrap();
    let survivors = spool
        .pending
        .iter()
        .filter(|entry| entry.sequence != 2)
        .flat_map(|entry| {
            let start = entry.file_offset as usize;
            let end = start + entry.framed_len as usize;
            original[start..end].iter().copied()
        })
        .collect::<Vec<_>>();

    spool
        .acknowledge(
            HookSpoolAckV1 {
                sequence: 2,
                receipt_id: [22; 16],
                disposition: HookSpoolAckDispositionV1::Committed,
            },
            UtcMicros(12),
        )
        .unwrap();
    spool.reclaim(UtcMicros(12)).unwrap();
    assert_eq!(fs::read(records_path(&root.0)).unwrap(), survivors);
    drop(spool);

    let (spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(13)).unwrap();
    assert_eq!(report.scanned_records, 0);
    assert_eq!(
        spool
            .pending
            .iter()
            .map(|entry| (entry.sequence, entry.event_id))
            .collect::<Vec<_>>(),
        [(1, envelopes[0].event_id), (3, envelopes[2].event_id)]
    );
}

#[test]
fn replay_hydrates_only_checkpointed_records_in_the_batch() {
    let root = TestDir::new("checkpoint-replay-hydration");
    let config = HookSpoolConfigV1::stock(NativeHostIdentityV1::CursorDesktop);
    let envelopes = (1..=6)
        .map(|event| numbered_envelope(event, if event <= 3 { 9 } else { 10 }))
        .collect::<Vec<_>>();
    let appended = publish_checkpoint(&root.0, config, &envelopes);
    let (mut spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(12)).unwrap();
    assert_eq!(report.checkpoint_records, envelopes.len() as u32);
    assert!(spool.pending.iter().all(|entry| entry.envelope.is_none()));

    let batches = spool.claim_replay_batches(UtcMicros(12), 1).unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].records, appended[..3]);
    assert_eq!(
        spool
            .pending
            .iter()
            .filter(|entry| entry.envelope.is_some())
            .count(),
        batches[0].records.len()
    );
}

#[test]
fn checkpointed_replay_hydrates_native_lifecycle() {
    let root = TestDir::new("checkpoint-native-lifecycle");
    let config = HookSpoolConfigV1::stock(NativeHostIdentityV1::OpenCode);
    let mut envelope = numbered_envelope(1, 9);
    envelope.producer = NativeHostIdentityV1::OpenCode;
    envelope.protected_session_id = Sha256::digest(b"session.native.checkpoint").into();
    envelope.event = HookEventV2::ToolLifecycle {
        tool_id: [8; 16],
        phase: crate::HookLifecyclePhaseV1::Completed,
        effect_receipt_id: None,
    };
    let lifecycle = NativeContextScoutLifecycleV1::new(
        "session.native.checkpoint",
        "call.native.checkpoint",
        envelope.event_id,
    )
    .unwrap();
    let binding = binding_for_envelopes(config.host, std::slice::from_ref(&envelope));
    let (mut spool, _) = HookSpoolV1::open(&root.0, config, UtcMicros(10)).unwrap();
    spool
        .append_with_native_lifecycle(
            envelope.clone(),
            Some(lifecycle.clone()),
            &binding,
            UtcMicros(10),
        )
        .unwrap();
    drop(spool);
    fs::remove_file(checkpoint_path(&root.0)).unwrap();
    drop(HookSpoolV1::open(&root.0, config, UtcMicros(11)).unwrap().0);

    let (mut spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(12)).unwrap();
    assert_eq!(report.checkpoint_records, 1);
    assert!(spool.pending[0].envelope.is_none());
    let batches = spool.claim_replay_batches(UtcMicros(12), 1).unwrap();
    assert_eq!(batches[0].records[0].envelope, envelope);
    assert_eq!(batches[0].records[0].native_lifecycle, Some(lifecycle));
}

#[test]
fn transition_extended_anchor_detects_corrupted_prefix() {
    let root = TestDir::new("checkpoint-extended-corrupt-prefix");
    let config = HookSpoolConfigV1::stock(NativeHostIdentityV1::CursorDesktop);
    let (mut spool, _) = HookSpoolV1::open(&root.0, config, UtcMicros(10)).unwrap();
    for event in 1..=CHECKPOINT_REWRITE_FRAME_THRESHOLD {
        spool
            .append(numbered_envelope(event, 9), &binding(), UtcMicros(10))
            .unwrap();
    }
    drop(spool);
    let (spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(11)).unwrap();
    assert!(report.checkpoint_rewritten);
    drop(spool);

    for event in (CHECKPOINT_REWRITE_FRAME_THRESHOLD + 1)..=(CHECKPOINT_REWRITE_FRAME_THRESHOLD + 3)
    {
        let (mut spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(11)).unwrap();
        assert!(!report.checkpoint_rewritten);
        spool
            .append(numbered_envelope(event, 9), &binding(), UtcMicros(11))
            .unwrap();
    }

    let mut records = std::fs::OpenOptions::new()
        .write(true)
        .open(records_path(&root.0))
        .unwrap();
    records.seek(SeekFrom::Start(70)).unwrap();
    records.write_all(&[0xff]).unwrap();
    records.sync_all().unwrap();
    drop(records);

    let (spool, report) = HookSpoolV1::open(&root.0, config, UtcMicros(12)).unwrap();
    assert_eq!(report.corrupted_at_offset, Some(0));
    assert!(matches!(
        spool.ensure_healthy(),
        Err(HookSpoolError::Corrupted { at_offset: 0 })
    ));
}

#[test]
fn checkpoint_corruption_falls_back_and_detects_the_corrupt_prefix() {
    let root = TestDir::new("checkpoint-corrupt-prefix");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    spool
        .append(envelope(2, 10), &binding(), UtcMicros(10))
        .unwrap();
    drop(spool);
    let (spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(11)).unwrap();
    drop(spool);

    let mut records = std::fs::OpenOptions::new()
        .write(true)
        .open(records_path(&root.0))
        .unwrap();
    records.seek(SeekFrom::Start(70)).unwrap();
    records.write_all(&[0xff]).unwrap();
    records.sync_all().unwrap();
    drop(records);

    let (spool, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(12)).unwrap();
    assert_eq!(report.corrupted_at_offset, Some(0));
    assert!(matches!(
        spool.ensure_healthy(),
        Err(HookSpoolError::Corrupted { at_offset: 0 })
    ));
}

#[test]
fn live_writer_never_attests_an_external_prefix_mutation() {
    let root = TestDir::new("checkpoint-live-mutation");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    drop(spool);
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(11)).unwrap();

    let mut records = std::fs::OpenOptions::new()
        .write(true)
        .open(records_path(&root.0))
        .unwrap();
    records.seek(SeekFrom::Start(70)).unwrap();
    records.write_all(&[0xff]).unwrap();
    records.sync_all().unwrap();
    drop(records);

    assert_eq!(
        spool
            .append(envelope(2, 10), &binding(), UtcMicros(11))
            .unwrap_err(),
        HookSpoolError::MetadataCorrupted
    );
    drop(spool);
    let (_, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(12)).unwrap();
    assert_eq!(report.corrupted_at_offset, Some(0));
}

#[test]
fn torn_stale_and_replaced_checkpoints_never_skip_authoritative_scans() {
    let root = TestDir::new("checkpoint-fail-closed");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    spool
        .append(envelope(2, 10), &binding(), UtcMicros(10))
        .unwrap();
    drop(spool);
    let (spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(11)).unwrap();
    drop(spool);

    fs::write(checkpoint_path(&root.0), b"{\"body\":").unwrap();
    let (spool, torn_report) = HookSpoolV1::open(&root.0, config(), UtcMicros(12)).unwrap();
    assert_eq!(torn_report.scanned_records, 2);
    drop(spool);

    let mut mismatched = fs::read(checkpoint_path(&root.0)).unwrap();
    let checksum_byte = mismatched.last_mut().unwrap();
    *checksum_byte ^= 0xff;
    fs::write(checkpoint_path(&root.0), mismatched).unwrap();
    let (spool, mismatched_report) = HookSpoolV1::open(&root.0, config(), UtcMicros(13)).unwrap();
    assert_eq!(mismatched_report.scanned_records, 2);
    drop(spool);

    let stale_checkpoint = fs::read(checkpoint_path(&root.0)).unwrap();
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(14)).unwrap();
    spool
        .append(envelope(3, 11), &binding(), UtcMicros(14))
        .unwrap();
    drop(spool);
    fs::remove_file(transition_path(&root.0)).unwrap();
    let (spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(15)).unwrap();
    drop(spool);
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(16)).unwrap();
    spool
        .append(envelope(4, 12), &binding(), UtcMicros(16))
        .unwrap();
    drop(spool);
    fs::write(checkpoint_path(&root.0), stale_checkpoint).unwrap();
    let (spool, stale_report) = HookSpoolV1::open(&root.0, config(), UtcMicros(17)).unwrap();
    assert_eq!(stale_report.scanned_records, 4);
    drop(spool);

    let replacement = root.0.join("records-replacement.bin");
    fs::write(&replacement, fs::read(records_path(&root.0)).unwrap()).unwrap();
    fs::rename(&replacement, records_path(&root.0)).unwrap();
    let (spool, replaced_report) = HookSpoolV1::open(&root.0, config(), UtcMicros(18)).unwrap();
    assert_eq!(replaced_report.scanned_records, 4);
    assert_eq!(spool.pending.len(), 4);
}

#[test]
fn control_event_capacity_survives_regular_event_saturation() {
    let root = TestDir::new("control-capacity");
    let mut config = config();
    config.limits.max_host_records = 3;
    config.limits.max_session_records = 3;
    let control = envelope(4, 9);
    let control_payload = canonical_json_bytes(&control).unwrap();
    let control_frame = encode_frame(3, UtcMicros(10), [9; 32], &control_payload).unwrap();
    assert!(
        control_frame.len() as u64 <= CONTROL_FRAME_RESERVE_BYTES,
        "reserved bytes must cover the checked-in control envelope"
    );
    let (mut spool, _) = HookSpoolV1::open(&root.0, config, UtcMicros(10)).unwrap();

    spool
        .append(regular_envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    spool
        .append(regular_envelope(2, 9), &binding(), UtcMicros(10))
        .unwrap();
    assert_eq!(
        spool
            .append(regular_envelope(3, 9), &binding(), UtcMicros(10))
            .unwrap_err(),
        HookSpoolError::SpoolFull
    );
    spool
        .append(control, &binding(), UtcMicros(10))
        .expect("reserved capacity admits a session control event");
    assert_eq!(spool.pending.len(), 3);
}

/// Appends `frame[..torn_len]` the way a writer killed mid-write leaves it.
fn write_torn_frame(root: &Path, frame: &[u8], torn_len: usize) {
    let mut output = std::fs::OpenOptions::new()
        .append(true)
        .open(records_path(root))
        .unwrap();
    output.write_all(&frame[..torn_len]).unwrap();
    output.sync_all().unwrap();
}

#[test]
fn a_torn_append_past_the_committed_extent_is_truncated_and_its_sequence_reused() {
    let root = TestDir::new("recovery");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    spool.commit().unwrap();
    let payload = canonical_json_bytes(&envelope(2, 9)).unwrap();
    let frame = encode_frame(2, UtcMicros(10), [9; 32], &payload).unwrap();
    let torn_len = 100.min(frame.len() - 1);
    write_torn_frame(&root.0, &frame, torn_len);

    let (mut spool, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(20)).unwrap();
    assert_eq!(report.corrupted_at_offset, None);
    assert_eq!(report.scanned_records, 1);
    assert_eq!(report.truncated_partial_tail_bytes, torn_len as u64);
    assert_eq!(spool.meta.next_sequence, 2);
    assert_eq!(
        spool
            .append(envelope(2, 9), &binding(), UtcMicros(20))
            .unwrap()
            .sequence,
        2
    );
    spool.commit().unwrap();
}

/// A commit acknowledged every byte through its published extent, so a
/// records file cut short inside that extent lost acknowledged data: it is
/// corruption, never a torn append to truncate.
#[test]
fn a_tail_cut_inside_the_committed_extent_is_corruption() {
    let root = TestDir::new("recovery-committed-cut");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    let first = spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    spool
        .append(envelope(2, 9), &binding(), UtcMicros(10))
        .unwrap();
    spool.commit().unwrap();
    let records = std::fs::OpenOptions::new()
        .write(true)
        .open(records_path(&root.0))
        .unwrap();
    records.set_len(u64::from(first.framed_len) + 100).unwrap();
    drop(records);

    let (spool, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(20)).unwrap();
    assert_eq!(report.truncated_partial_tail_bytes, 0);
    assert_eq!(
        report.corrupted_at_offset,
        Some(u64::from(first.framed_len))
    );
    assert_eq!(
        spool.ensure_healthy(),
        Err(HookSpoolError::Corrupted {
            at_offset: u64::from(first.framed_len)
        })
    );
}

/// Released builds persisted a per-append intent in metadata (its frame as a
/// JSON byte array) under the same metadata version. That metadata still
/// opens: the torn tail it described is recovered from the records file, and
/// the next metadata write drops the retired field.
#[test]
fn released_metadata_with_an_append_intent_opens_and_drops_it() {
    let root = TestDir::new("recovery-released-intent");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    let first = spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    spool.commit().unwrap();
    let payload = canonical_json_bytes(&envelope(2, 9)).unwrap();
    let frame = encode_frame(2, UtcMicros(10), [9; 32], &payload).unwrap();
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(meta_path(&root.0)).unwrap()).unwrap();
    persisted["append_intent"] = serde_json::json!({
        "sequence": 2,
        "file_offset": first.framed_len,
        "framed_len": frame.len(),
        "frame": frame,
    });
    fs::write(meta_path(&root.0), serde_json::to_vec(&persisted).unwrap()).unwrap();
    let torn_len = 100.min(frame.len() - 1);
    write_torn_frame(&root.0, &frame, torn_len);

    let (mut spool, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(20)).unwrap();
    assert_eq!(report.corrupted_at_offset, None);
    assert_eq!(report.truncated_partial_tail_bytes, torn_len as u64);
    assert_eq!(report.next_sequence, 2);
    assert!(
        spool
            .acknowledge(
                HookSpoolAckV1 {
                    sequence: first.sequence,
                    receipt_id: [21; 16],
                    disposition: HookSpoolAckDispositionV1::Committed,
                },
                UtcMicros(20),
            )
            .unwrap()
    );
    let rewritten: serde_json::Value =
        serde_json::from_slice(&fs::read(meta_path(&root.0)).unwrap()).unwrap();
    assert_eq!(rewritten.get("append_intent"), None);
    assert_eq!(rewritten["committed_through"], 1);
    assert_eq!(rewritten["next_sequence"], 2);
}

/// Writer processes loop the hook sequence (bounded open, append, commit)
/// and print each event only after its commit returns, the point a hook
/// acknowledges its host. Killing them all mid-stream lands inside appends,
/// lease waits, and shared syncs; the reopened spool must be healthy, hold
/// every acknowledged event exactly once, and continue its sequence.
#[test]
fn killing_writers_mid_batch_keeps_every_acknowledged_record_and_no_torn_one() {
    const WRITER_ENV: &str = "TRACEDECAY_HOOK_SPOOL_KILLED_WRITER";
    const ROOT_ENV: &str = "TRACEDECAY_HOOK_SPOOL_KILLED_ROOT";
    const WRITERS: u32 = 4;
    const ACKNOWLEDGED_BEFORE_KILL: usize = 48;
    let config = HookSpoolConfigV1::stock(NativeHostIdentityV1::CursorDesktop);
    if let Ok(writer) = std::env::var(WRITER_ENV) {
        let writer: u32 = writer.parse().expect("writer ordinal");
        let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("writer spool root"));
        let mut stdout = std::io::stdout();
        for index in 0..crate::MAX_SPOOL_RECORDS_PER_SESSION - 1 {
            let event = writer * 100_000 + index + 1;
            let (mut spool, _) = match HookSpoolV1::open_within(
                &root,
                config,
                UtcMicros(10),
                Duration::from_secs(5),
            ) {
                Ok(opened) => opened,
                Err(HookSpoolError::AdmissionTimedOut) => continue,
                Err(error) => panic!("writer open failed: {error:?}"),
            };
            spool
                .append(
                    numbered_envelope(event, writer as u8 + 1),
                    &binding(),
                    UtcMicros(10),
                )
                .expect("writer append");
            spool.commit().expect("writer commit");
            writeln!(stdout, "{event}").expect("acknowledge");
            stdout.flush().expect("acknowledge");
        }
        return;
    }

    let root = TestDir::new("killed-writers");
    let spool_root = root.0.join("spool");
    drop(HookSpoolV1::open(&spool_root, config, UtcMicros(10)).unwrap());
    let test_name =
        "spool::tests::killing_writers_mid_batch_keeps_every_acknowledged_record_and_no_torn_one";
    let mut children = (0..WRITERS)
        .map(|writer| {
            Command::new(std::env::current_exe().expect("current test binary"))
                .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
                .env(WRITER_ENV, writer.to_string())
                .env(ROOT_ENV, &spool_root)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn writer")
        })
        .collect::<Vec<_>>();
    let (acknowledged_tx, acknowledged_rx) = std::sync::mpsc::channel::<u32>();
    let readers = children
        .iter_mut()
        .map(|child| {
            let stdout = child.stdout.take().expect("writer stdout");
            let acknowledged_tx = acknowledged_tx.clone();
            std::thread::spawn(move || {
                for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)) {
                    let Ok(line) = line else { break };
                    if let Ok(event) = line.trim().parse::<u32>() {
                        let _ = acknowledged_tx.send(event);
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    drop(acknowledged_tx);
    let mut acknowledged = std::collections::BTreeSet::new();
    while acknowledged.len() < ACKNOWLEDGED_BEFORE_KILL {
        let event = acknowledged_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("writers acknowledge before the kill");
        acknowledged.insert(event);
    }
    for child in &mut children {
        child.kill().expect("SIGKILL writer");
    }
    for child in &mut children {
        child.wait().expect("reap writer");
    }
    for reader in readers {
        reader.join().expect("writer stdout reader");
    }
    acknowledged.extend(acknowledged_rx.try_iter());

    let (mut spool, report) = HookSpoolV1::open(&spool_root, config, UtcMicros(20)).unwrap();
    assert_eq!(report.corrupted_at_offset, None);
    let records = spool
        .expired_records(UtcMicros(10 + MAX_SPOOL_AGE_MICROS + 1))
        .unwrap();
    let sequences = records
        .iter()
        .map(|record| record.sequence)
        .collect::<Vec<_>>();
    assert_eq!(
        sequences,
        (1..report.next_sequence).collect::<Vec<_>>(),
        "reopen continues one contiguous sequence"
    );
    let spooled = records
        .iter()
        .map(|record| u32::from_le_bytes(record.envelope.event_id[..4].try_into().unwrap()))
        .collect::<Vec<_>>();
    let unique = spooled
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), spooled.len(), "no event is spooled twice");
    assert_eq!(
        acknowledged.difference(&unique).collect::<Vec<_>>(),
        Vec::<&u32>::new(),
        "every acknowledged event survives the kill"
    );
    let next_event = 900_001;
    spool
        .append(numbered_envelope(next_event, 9), &binding(), UtcMicros(20))
        .unwrap();
    spool.commit().unwrap();
}

#[test]
fn fair_replay_is_fifo_per_session_and_round_robin_across_sessions() {
    let root = TestDir::new("fair");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    spool
        .append(envelope(2, 10), &binding(), UtcMicros(10))
        .unwrap();
    spool
        .append(envelope(3, 9), &binding(), UtcMicros(10))
        .unwrap();
    let batches = spool.claim_replay_batches(UtcMicros(11), 4).unwrap();
    assert_eq!(batches.len(), 2);
    assert_eq!(
        batches[0]
            .records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    assert_eq!(
        batches[1]
            .records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        [2]
    );
    assert!(
        spool
            .claim_replay_batches(UtcMicros(11), 4)
            .unwrap()
            .is_empty()
    );
    for batch in batches {
        spool.release_replay_claim(batch.claim_id).unwrap();
    }
    let next = spool.claim_replay_batches(UtcMicros(11), 1).unwrap();
    assert_eq!(next[0].protected_session_id, [9; 32]);
}

#[test]
fn fair_replay_cursor_survives_spool_reopen() {
    let root = TestDir::new("fair-reopen");
    {
        let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
        for event in 1..=5 {
            spool
                .append(envelope(event, event + 8), &binding(), UtcMicros(10))
                .unwrap();
        }
        let first = spool.claim_replay_batches(UtcMicros(11), 4).unwrap();
        assert_eq!(
            first
                .iter()
                .map(|batch| batch.protected_session_id)
                .collect::<Vec<_>>(),
            [[9; 32], [10; 32], [11; 32], [12; 32]]
        );
    }

    let (mut reopened, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(12)).unwrap();
    let next = reopened.claim_replay_batches(UtcMicros(12), 1).unwrap();
    assert_eq!(
        next[0].protected_session_id, [13; 32],
        "reopening the spool must not starve sessions after the first four"
    );
}

#[test]
fn live_writer_lease_blocks_a_second_host_process() {
    let root = TestDir::new("lease");
    let (spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    assert_eq!(
        HookSpoolV1::open(&root.0, config(), UtcMicros(11)).unwrap_err(),
        HookSpoolError::WriterLeaseHeld
    );
    drop(spool);
    assert!(HookSpoolV1::open(&root.0, config(), UtcMicros(11)).is_ok());
}

#[test]
fn writer_lease_contends_and_releases_across_processes() {
    const MODE_ENV: &str = "TRACEDECAY_HOOK_SPOOL_LOCK_PROBE";
    const ROOT_ENV: &str = "TRACEDECAY_HOOK_SPOOL_LOCK_ROOT";
    if let Ok(mode) = std::env::var(MODE_ENV) {
        let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("child lock root"));
        match mode.as_str() {
            "contended" => assert_eq!(
                HookSpoolV1::open(&root, config(), UtcMicros(11)).unwrap_err(),
                HookSpoolError::WriterLeaseHeld
            ),
            "released" => {
                HookSpoolV1::open(&root, config(), UtcMicros(12))
                    .expect("OS releases lock when owner descriptor closes");
            }
            other => panic!("unknown child lock probe mode: {other}"),
        }
        return;
    }

    let root = TestDir::new("process-lease");
    let (spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    let test_name = "spool::tests::writer_lease_contends_and_releases_across_processes";
    let run_child = |mode: &str| {
        Command::new(std::env::current_exe().expect("current test binary"))
            .args(["--exact", test_name, "--nocapture"])
            .env(MODE_ENV, mode)
            .env(ROOT_ENV, &root.0)
            .status()
            .expect("run lock probe child")
    };
    assert!(run_child("contended").success());
    drop(spool);
    assert!(run_child("released").success());
}

/// The writer lease is single-shot: once the caller's clock passes the
/// acquisition deadline the handle fails closed, and the documented recovery
/// (drop + reopen) restores a working writer without losing durable records.
/// This is the guard against a silent, permanent append-rejection loop: a
/// caller that reads a fresh clock per mutation must reopen rather than retry.
#[test]
fn an_elapsed_writer_lease_fails_closed_and_reopening_restores_the_writer() {
    let root = TestDir::new("lease-expiry");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    let queued = spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();

    // config().writer_lease_micros is 100, so the lease acquired at 10 is dead.
    let expired = UtcMicros(10 + 100);
    assert_eq!(
        spool
            .append(envelope(2, 9), &binding(), expired)
            .unwrap_err(),
        HookSpoolError::WriterLeaseLost
    );
    assert_eq!(
        spool
            .acknowledge(
                HookSpoolAckV1 {
                    sequence: queued.sequence,
                    receipt_id: [31; 16],
                    disposition: HookSpoolAckDispositionV1::Committed,
                },
                expired,
            )
            .unwrap_err(),
        HookSpoolError::WriterLeaseLost
    );
    // Retrying on the same handle can never recover: there is no renewal path.
    assert_eq!(
        spool
            .append(envelope(2, 9), &binding(), UtcMicros(expired.0 + 1_000))
            .unwrap_err(),
        HookSpoolError::WriterLeaseLost
    );
    drop(spool);

    let (mut reopened, report) = HookSpoolV1::open(&root.0, config(), expired).unwrap();
    assert_eq!(
        report.pending_records, 1,
        "an elapsed lease must not discard durable records"
    );
    reopened
        .append(envelope(2, 9), &binding(), expired)
        .expect("a fresh lease admits appends again");
    assert!(
        reopened
            .acknowledge(
                HookSpoolAckV1 {
                    sequence: queued.sequence,
                    receipt_id: [31; 16],
                    disposition: HookSpoolAckDispositionV1::Committed,
                },
                expired,
            )
            .unwrap(),
        "the record spooled under the previous lease is still acknowledgeable"
    );
}

/// The production writer lifecycle: one clock reading is taken at open and
/// reused for every mutation of that session, so a bounded-but-slow pass (a
/// daemon drain awaiting admission per record) can never expire underneath
/// itself no matter how much wall-clock time elapses.
#[test]
fn a_writer_reusing_its_acquisition_timestamp_never_expires_mid_session() {
    let root = TestDir::new("lease-single-shot");
    let now = UtcMicros(10);
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), now).unwrap();
    for event in 1..=4 {
        let record = spool
            .append(envelope(event, 9), &binding(), now)
            .expect("append under the acquisition timestamp");
        assert!(
            spool
                .acknowledge(
                    HookSpoolAckV1 {
                        sequence: record.sequence,
                        receipt_id: [event.wrapping_add(40); 16],
                        disposition: HookSpoolAckDispositionV1::Committed,
                    },
                    now,
                )
                .unwrap()
        );
    }
    assert!(spool.pending.is_empty());
}

#[test]
fn quotas_are_never_evicted_and_expired_records_need_tombstones() {
    let root = TestDir::new("quota");
    let mut config = config();
    config.limits.max_session_records = 1;
    let (mut spool, _) = HookSpoolV1::open(&root.0, config, UtcMicros(10)).unwrap();
    spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    assert_eq!(
        spool
            .append(envelope(2, 9), &binding(), UtcMicros(10))
            .unwrap_err(),
        HookSpoolError::SpoolFull
    );
    assert_eq!(
        spool
            .expired_records(UtcMicros(10 + MAX_SPOOL_AGE_MICROS + 1))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(spool.pending.len(), 1);
}

/// A hook callback waits for the writer lease within its synchronous budget,
/// so the drain's settlement and reclaim must not hold the lease across a
/// durability barrier, however slow the disk.
#[test]
fn settling_and_reclaiming_hold_the_writer_lease_across_no_durability_barrier() {
    let root = TestDir::new("lease-barrier-free");
    let (mut writer, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    let records = (1..=3)
        .map(|event| {
            writer
                .append(envelope(event, 9), &binding(), UtcMicros(10))
                .unwrap()
        })
        .collect::<Vec<_>>();
    writer.commit().unwrap();
    // The drain reacquires its lease while an admitted callback may hold it,
    // and the dead-lease fixture's 100µs wait gives that reacquire a single
    // try_lock attempt; the drain therefore uses the production lease wait.
    let drain_config = HookSpoolConfigV1 {
        writer_lease_micros: HookSpoolConfigV1::stock(NativeHostIdentityV1::CursorDesktop)
            .writer_lease_micros,
        ..config()
    };
    let (mut drain, _) = HookSpoolV1::open(&root.0, drain_config, UtcMicros(10)).unwrap();
    let acknowledgements = records
        .iter()
        .map(|record| HookSpoolAckV1 {
            sequence: record.sequence,
            receipt_id: [record.sequence as u8; 16],
            disposition: HookSpoolAckDispositionV1::Committed,
        })
        .collect::<Vec<_>>();

    // Each barrier outlasts a callback's whole lease-wait budget.
    let slow_disk = tracedecay_private_fs::framed_log::sync_latency::inject(
        &root.0,
        2 * crate::HOOK_SYNCHRONOUS_BUDGET,
    );
    let settled = std::sync::atomic::AtomicBool::new(false);
    let admissions = std::thread::scope(|scope| {
        let callbacks = scope.spawn(|| {
            let mut admissions = Vec::new();
            while !settled.load(std::sync::atomic::Ordering::SeqCst) {
                admissions.push(
                    HookSpoolV1::open_within(
                        &root.0,
                        config(),
                        UtcMicros(11),
                        crate::HOOK_SYNCHRONOUS_BUDGET,
                    )
                    .map(drop),
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            admissions
        });
        let outcomes = drain.acknowledge_many(&acknowledgements, UtcMicros(11));
        settled.store(true, std::sync::atomic::Ordering::SeqCst);
        // A callback that entered its lease wait just before settlement is
        // admitted once the drain lets go, within the budget it already has.
        drop(drain);
        assert_eq!(outcomes.unwrap(), vec![Ok(true); 3]);
        callbacks.join().unwrap()
    });
    assert!(
        slow_disk.syncs() >= 2,
        "settlement and reclaim ran barriers"
    );
    assert!(!admissions.is_empty());
    assert_eq!(
        admissions
            .iter()
            .filter(|admission| admission.is_err())
            .count(),
        0,
        "{admissions:?}"
    );
    drop(slow_disk);
    let (spool, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(12)).unwrap();
    assert_eq!(report.pending_records, 0);
    assert!(spool.pending.is_empty());
    assert_eq!(fs::metadata(records_path(&root.0)).unwrap().len(), 0);
}

/// Bounded admission waits only on contention. A writer whose budget is
/// already spent, like a callback or a drain descheduled past its deadline,
/// still takes a free lease, including the lease a settlement reacquires
/// after its barriers; a held lease refuses it without waiting.
#[test]
fn a_writer_whose_budget_is_spent_takes_a_free_lease_but_never_a_held_one() {
    let root = TestDir::new("spent-budget");
    let (owner, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    assert_eq!(
        HookSpoolV1::open_within(&root.0, config(), UtcMicros(11), Duration::ZERO).unwrap_err(),
        HookSpoolError::AdmissionTimedOut
    );
    drop(owner);

    let (mut admitted, _) =
        HookSpoolV1::open_within(&root.0, config(), UtcMicros(11), Duration::ZERO)
            .expect("a free lease admits a writer with no budget left");
    let record = admitted
        .append(envelope(1, 9), &binding(), UtcMicros(11))
        .unwrap();
    assert!(
        admitted
            .acknowledge(
                HookSpoolAckV1 {
                    sequence: record.sequence,
                    receipt_id: [41; 16],
                    disposition: HookSpoolAckDispositionV1::Committed,
                },
                UtcMicros(11),
            )
            .unwrap()
    );
    drop(admitted);
    let (_, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(12)).unwrap();
    assert_eq!(report.pending_records, 0);
    assert_eq!(report.next_sequence, 2);
}

#[test]
fn bounded_writer_admission_preserves_failfast_and_times_out_without_mutation() {
    let root = TestDir::new("bounded-admission");
    let (owner, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    let before = fs::read(meta_path(&root.0)).unwrap();
    assert_eq!(
        HookSpoolV1::open(&root.0, config(), UtcMicros(11)).unwrap_err(),
        HookSpoolError::WriterLeaseHeld
    );
    assert_eq!(
        HookSpoolV1::open_within(
            &root.0,
            config(),
            UtcMicros(11),
            std::time::Duration::from_millis(20),
        )
        .unwrap_err(),
        HookSpoolError::AdmissionTimedOut
    );
    assert_eq!(fs::read(meta_path(&root.0)).unwrap(), before);
    drop(owner);
    let (mut admitted, _) = HookSpoolV1::open_within(
        &root.0,
        config(),
        UtcMicros(11),
        crate::HOOK_SYNCHRONOUS_BUDGET,
    )
    .unwrap();
    admitted
        .append(envelope(1, 1), &binding(), UtcMicros(12))
        .unwrap();
    drop(admitted);
    let (_, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(13)).unwrap();
    assert_eq!(report.next_sequence, 2);
}
