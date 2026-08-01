use std::io::Write;
use std::process::Command;

use super::*;
use crate::{
    HookCapabilityV1, HookEventFamily, HookEventSupportV1, HookEventV2, HookHostV1, HookOrderingV1,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "tracedecay-hooks-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
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
        host: HookHostV1::CursorDesktop,
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
        host: HookHostV1::CursorDesktop,
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
        producer: HookHostV1::CursorDesktop,
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

fn regular_envelope(event: u8, session: u8) -> HookEventEnvelopeV2 {
    HookEventEnvelopeV2 {
        event: HookEventV2::SavedEdit {
            file_id: [event; 16],
            changed_range_count: 1,
        },
        ..envelope(event, session)
    }
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
    assert!(spool.pending.is_empty());
    assert_eq!(fs::metadata(records_path(&root.0)).unwrap().len(), 0);
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
    let (mut spool, _) = HookSpoolV1::open(&root.0, config, UtcMicros(11)).unwrap();

    let duplicate = spool
        .append(envelope(1, 9), &binding(), UtcMicros(11))
        .unwrap();

    assert_eq!(duplicate, first);
    assert_eq!(spool.pending, [first]);
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
    let (mut spool, _) = HookSpoolV1::open(&root.0, config, UtcMicros(11)).unwrap();
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

#[test]
fn matching_torn_append_tail_is_truncated_and_sequence_is_reused() {
    let root = TestDir::new("recovery");
    let (mut spool, _) = HookSpoolV1::open(&root.0, config(), UtcMicros(10)).unwrap();
    spool
        .append(envelope(1, 9), &binding(), UtcMicros(10))
        .unwrap();
    let payload = canonical_json_bytes(&envelope(2, 9)).unwrap();
    let frame = encode_frame(2, UtcMicros(10), [9; 32], &payload).unwrap();
    let mut meta = spool.meta.clone();
    meta.append_intent = Some(append_intent(2, spool.physical_len, &frame).unwrap());
    write_meta(&root.0, &meta).unwrap();
    let mut output = std::fs::OpenOptions::new()
        .append(true)
        .open(records_path(&root.0))
        .unwrap();
    let torn_len = 100.min(frame.len() - 1);
    output.write_all(&frame[..torn_len]).unwrap();
    output.sync_all().unwrap();
    drop(output);
    drop(spool);
    let (mut spool, report) = HookSpoolV1::open(&root.0, config(), UtcMicros(20)).unwrap();
    assert_eq!(report.truncated_partial_tail_bytes, torn_len as u64);
    assert_eq!(spool.meta.next_sequence, 2);
    assert_eq!(
        spool
            .append(envelope(2, 9), &binding(), UtcMicros(20))
            .unwrap()
            .sequence,
        2
    );
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
            .len(),
        1
    );
    assert_eq!(spool.pending.len(), 1);
}
