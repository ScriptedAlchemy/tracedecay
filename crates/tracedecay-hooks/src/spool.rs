//! Transport-only append-only Hook V2 replay spool.
//!
//! This is intentionally not a database or product queue. It persists only
//! already-validated, content-free [`crate::HookEventEnvelopeV2`] bytes plus
//! framing/checksum metadata. The daemon owns replay authorization and every
//! acknowledgement; this module only makes those transitions crash-safe.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::framed_log::{
    DirectorySyncPolicy, append_durable, atomic_write as shared_atomic_write,
    read_bounded as shared_read_bounded, sync_directory as shared_sync_directory,
    truncate_file as shared_truncate_file, validate_regular_or_missing as shared_validate_regular,
};
use tracedecay_domain::{
    UtcMicros, canonical_json_bytes,
    framed_log::{self, checksum as frame_checksum, partial_tail_matches_prefix},
};

use crate::{
    HookContractError, HookEventEnvelopeV2, HookHostV1, HookScopeBindingV1, MAX_HOOK_PAYLOAD_BYTES,
    MAX_REPLAY_BATCH_BYTES, MAX_REPLAY_BATCH_RECORDS, MAX_SPOOL_AGE_MICROS,
    MAX_SPOOL_BYTES_PER_HOST, MAX_SPOOL_BYTES_PER_SESSION, MAX_SPOOL_RECORDS_PER_HOST,
    MAX_SPOOL_RECORDS_PER_SESSION, decode_hook_event_envelope_compat,
};

mod replay;

use replay::{
    batch_for_session, is_expired, replayable_sessions, round_robin_after, usage_by_session,
};

const SPOOL_MAGIC: &[u8; 4] = b"TDH2";
const SPOOL_FORMAT_VERSION: u16 = 1;
const SPOOL_META_VERSION: u16 = 1;
const FRAME_LENGTH_BYTES: usize = 4;
const FRAME_HEADER_BYTES: usize = 4 + 2 + 8 + 8 + 32 + 4;
const FRAME_CHECKSUM_BYTES: usize = framed_log::CHECKSUM_BYTES;
const CONTROL_RECORD_RESERVE: u32 = 1;
const CONTROL_FRAME_RESERVE_BYTES: u64 = 4 * 1024;
// Acknowledgements can arrive out of global sequence order because replay is
// fair across sessions. Reserve room for one bounded marker per live record.
const MAX_META_BYTES: usize = 1024 * 1024;
const MAX_LEASE_BYTES: usize = 512;
const MAX_REPLAY_SESSIONS: usize = 4;
const RECORDS_FILE: &str = "records.v1.bin";
const META_FILE: &str = "meta.v1.json";
const LEASE_FILE: &str = "writer.v1.lease";
const REPLAY_CURSOR_FILE: &str = "replay-cursor.v1.bin";
const DIRECTORY_POLICY: DirectorySyncPolicy = DirectorySyncPolicy::Strict;

/// Per-host and per-session bounds. Callers may narrow these for a host test
/// or constrained installation but can never widen the checked-in limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolLimitsV1 {
    pub max_host_records: u32,
    pub max_host_bytes: u64,
    pub max_session_records: u32,
    pub max_session_bytes: u64,
}

impl HookSpoolLimitsV1 {
    pub const fn stock() -> Self {
        Self {
            max_host_records: MAX_SPOOL_RECORDS_PER_HOST,
            max_host_bytes: MAX_SPOOL_BYTES_PER_HOST,
            max_session_records: MAX_SPOOL_RECORDS_PER_SESSION,
            max_session_bytes: MAX_SPOOL_BYTES_PER_SESSION,
        }
    }

    fn validate(self) -> Result<(), HookSpoolError> {
        if self.max_host_records == 0
            || self.max_host_records > MAX_SPOOL_RECORDS_PER_HOST
            || self.max_host_bytes == 0
            || self.max_host_bytes > MAX_SPOOL_BYTES_PER_HOST
            || self.max_session_records == 0
            || self.max_session_records > self.max_host_records
            || self.max_session_records > MAX_SPOOL_RECORDS_PER_SESSION
            || self.max_session_bytes == 0
            || self.max_session_bytes > self.max_host_bytes
            || self.max_session_bytes > MAX_SPOOL_BYTES_PER_SESSION
        {
            return Err(HookSpoolError::InvalidLimits);
        }
        Ok(())
    }
}

/// Configuration owned by the thin host adapter. Time is caller-provided so
/// the spool does not read a clock or invent a product timing policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolConfigV1 {
    pub host: HookHostV1,
    pub limits: HookSpoolLimitsV1,
    pub writer_lease_micros: i64,
}

impl HookSpoolConfigV1 {
    pub const fn stock(host: HookHostV1) -> Self {
        Self {
            host,
            limits: HookSpoolLimitsV1::stock(),
            writer_lease_micros: 5_000_000,
        }
    }

    fn validate(self) -> Result<(), HookSpoolError> {
        self.limits.validate()?;
        if self.writer_lease_micros <= 0 {
            return Err(HookSpoolError::InvalidLease);
        }
        Ok(())
    }
}

/// A durable replay record. `envelope` is the exact canonical payload framed
/// on disk; `framed_len` includes the length prefix and checksum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolRecordV1 {
    pub sequence: u64,
    pub protected_session_id: [u8; 32],
    pub queued_at: UtcMicros,
    pub envelope: HookEventEnvelopeV2,
    pub encoded_len: u32,
    pub checksum: [u8; 32],
    pub framed_len: u32,
}

/// The opened spool's bounded recovery report. Corrupt bytes are never
/// discarded unless a matching append intent proves they are an unpublished
/// partial tail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolOpenReportV1 {
    pub pending_records: u32,
    pub pending_bytes: u64,
    pub committed_through: u64,
    pub next_sequence: u64,
    pub truncated_partial_tail_bytes: u64,
    pub corrupted_at_offset: Option<u64>,
}

/// Opaque lease evidence held by exactly one local writer at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolWriterLeaseV1 {
    pub token: [u8; 16],
    pub expires_at: UtcMicros,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookSpoolAckDispositionV1 {
    Committed,
    TerminalTombstone,
}

/// A daemon receipt acknowledgement. The receipt is opaque transport evidence
/// and does not assert a business/application effect by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolAckV1 {
    pub sequence: u64,
    pub receipt_id: [u8; 16],
    pub disposition: HookSpoolAckDispositionV1,
}

/// A fair per-session replay lease. The caller reauthorizes every record with
/// the daemon before transmission; one batch never contains two sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookReplayBatchV1 {
    pub claim_id: [u8; 16],
    pub protected_session_id: [u8; 32],
    pub records: Vec<HookSpoolRecordV1>,
    pub byte_count: u32,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HookSpoolError {
    #[error("hook spool filesystem operation failed")]
    Io,
    #[error("hook spool root or member path is unsafe")]
    UnsafePath,
    #[error("hook spool limits are invalid")]
    InvalidLimits,
    #[error("hook spool writer lease is invalid")]
    InvalidLease,
    #[error("another live hook spool writer owns this host")]
    WriterLeaseHeld,
    #[error("hook spool writer lease was lost or expired")]
    WriterLeaseLost,
    #[error("hook spool format version is unsupported")]
    UnsupportedVersion,
    #[error("hook spool metadata is malformed or internally inconsistent")]
    MetadataCorrupted,
    #[error("hook spool publication was interrupted; reopen is required before another mutation")]
    RecoveryRequired,
    #[error("hook spool frame is corrupt at offset {at_offset}")]
    Corrupted { at_offset: u64 },
    #[error("hook spool record exceeds the bounded payload limit")]
    RecordTooLarge,
    #[error("hook spool quota is full")]
    SpoolFull,
    #[error("hook envelope is invalid for the supplied daemon binding")]
    EnvelopeRejected(HookContractError),
    #[error("hook event ID conflicts with a different pending envelope")]
    EventIdConflict,
    #[error("hook spool acknowledgement is unknown or conflicts with prior receipt evidence")]
    AckConflict,
    #[error("hook spool replay claim is unknown")]
    ReplayClaimUnknown,
    #[error("hook spool replay batch exceeds a checked-in bound")]
    ReplayBatchExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookSpoolMetaV1 {
    version: u16,
    committed_through: u64,
    next_sequence: u64,
    acknowledged: Vec<AcknowledgedSequenceV1>,
    integrity: SpoolIntegrityV1,
    append_intent: Option<AppendIntentV1>,
}

impl HookSpoolMetaV1 {
    const fn fresh() -> Self {
        Self {
            version: SPOOL_META_VERSION,
            committed_through: 0,
            next_sequence: 1,
            acknowledged: Vec::new(),
            integrity: SpoolIntegrityV1::Healthy,
            append_intent: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SpoolIntegrityV1 {
    Healthy,
    Corrupted { at_offset: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppendIntentV1 {
    sequence: u64,
    file_offset: u64,
    framed_len: u32,
    frame: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgedSequenceV1 {
    sequence: u64,
    receipt_id: [u8; 16],
    disposition: HookSpoolAckDispositionV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseFileV1 {
    version: u16,
    token: [u8; 16],
    expires_at: UtcMicros,
}

#[derive(Debug)]
struct ScanResult {
    records: Vec<HookSpoolRecordV1>,
    valid_end: u64,
    physical_len: u64,
    partial_tail: Option<Vec<u8>>,
    corruption: Option<u64>,
}

/// A host-local transport spool. It owns a short writer lease, performs no
/// query/model/database work, and has no authority to rebind an event.
#[derive(Debug)]
pub struct HookSpoolV1 {
    root: PathBuf,
    config: HookSpoolConfigV1,
    lease: HookSpoolWriterLeaseV1,
    lease_file: File,
    meta: HookSpoolMetaV1,
    pending: Vec<HookSpoolRecordV1>,
    pending_by_session: BTreeMap<[u8; 32], (u32, u64)>,
    physical_len: u64,
    round_robin_after: Option<[u8; 32]>,
    replay_claims: BTreeMap<[u8; 32], [u8; 16]>,
    recovery_required: bool,
}

impl HookSpoolV1 {
    /// Open/recover a bounded spool and acquire the sole writer lease. The OS
    /// releases the prior process lock when its file descriptor closes. Expiry
    /// independently prevents a live-but-stale owner from mutating the spool.
    pub fn open(
        root: impl Into<PathBuf>,
        config: HookSpoolConfigV1,
        now: UtcMicros,
    ) -> Result<(Self, HookSpoolOpenReportV1), HookSpoolError> {
        config.validate()?;
        let root = root.into();
        ensure_root(&root)?;
        let (lease, lease_file) = acquire_lease(&root, config.writer_lease_micros, now)?;
        Self::open_after_lease(root, config, lease, lease_file, now)
    }

    fn open_after_lease(
        root: PathBuf,
        config: HookSpoolConfigV1,
        lease: HookSpoolWriterLeaseV1,
        lease_file: File,
        _now: UtcMicros,
    ) -> Result<(Self, HookSpoolOpenReportV1), HookSpoolError> {
        let mut meta = read_meta(&root)?.unwrap_or_else(HookSpoolMetaV1::fresh);
        validate_meta(&meta, config.limits, config.host)?;
        let mut scan = scan_records(&root, config)?;
        let mut truncated_partial_tail_bytes = 0;

        if let Some(offset) = scan.corruption {
            meta.integrity = SpoolIntegrityV1::Corrupted { at_offset: offset };
            write_meta(&root, &meta)?;
        } else if let Some(partial) = scan.partial_tail.as_ref() {
            if partial_tail_matches_intent(&meta, scan.valid_end, partial) {
                truncate_records(&root, scan.valid_end)?;
                truncated_partial_tail_bytes = scan.physical_len.saturating_sub(scan.valid_end);
                scan.physical_len = scan.valid_end;
                scan.partial_tail = None;
                meta.append_intent = None;
                write_meta(&root, &meta)?;
            } else {
                meta.integrity = SpoolIntegrityV1::Corrupted {
                    at_offset: scan.valid_end,
                };
                write_meta(&root, &meta)?;
            }
        }

        if matches!(meta.integrity, SpoolIntegrityV1::Healthy) {
            reconcile_append_intent(&mut meta, &scan.records, config.host)?;
            validate_meta_against_records(&meta, &scan.records, config.limits)?;
            write_meta(&root, &meta)?;
        }

        let acknowledged = acknowledged_map(&meta)?;
        let pending = scan
            .records
            .into_iter()
            .filter(|record| {
                record.sequence > meta.committed_through
                    && !acknowledged.contains_key(&record.sequence)
            })
            .collect::<Vec<_>>();
        let pending_by_session = usage_by_session(&pending, config.limits)?;
        let report = HookSpoolOpenReportV1 {
            pending_records: u32::try_from(pending.len()).map_err(|_| HookSpoolError::SpoolFull)?,
            pending_bytes: pending
                .iter()
                .map(|record| u64::from(record.framed_len))
                .sum(),
            committed_through: meta.committed_through,
            next_sequence: meta.next_sequence,
            truncated_partial_tail_bytes,
            corrupted_at_offset: match meta.integrity {
                SpoolIntegrityV1::Healthy => None,
                SpoolIntegrityV1::Corrupted { at_offset } => Some(at_offset),
            },
        };
        let round_robin_after = read_replay_cursor(&root)?;
        let mut spool = Self {
            root,
            config,
            lease,
            lease_file,
            meta,
            pending,
            pending_by_session,
            physical_len: scan.physical_len,
            round_robin_after,
            replay_claims: BTreeMap::new(),
            recovery_required: false,
        };
        // A crash may leave logically acknowledged frames in the active file.
        // Metadata is already durable, so this recovery compaction is safe.
        if matches!(spool.meta.integrity, SpoolIntegrityV1::Healthy)
            && spool.physical_len > spool.pending_bytes()
        {
            spool.compact_pending()?;
        }
        Ok((spool, report))
    }

    pub fn lease(&self) -> HookSpoolWriterLeaseV1 {
        self.lease
    }

    /// Refresh a still-owned writer lease. Expired or replaced leases fail
    /// closed; no caller may continue appending after that point.
    ///
    /// DECISION NEEDED: this method has zero callers today, but it is the
    /// only lease-renewal path in this crate. A long-lived spool writer
    /// (e.g. a daemon replay loop that keeps a `HookSpoolV1` open across
    /// many `append` calls) never renews its lease, so `writer_lease_micros`
    /// after acquisition, `ensure_live_lease` (see below) starts rejecting
    /// every subsequent append with `HookSpoolError::WriterLeaseExpired`
    /// (or similar), even though nothing else holds the lease. Either wire
    /// this into the daemon replay loop so long-lived writers renew before
    /// expiry, or explicitly document spool writer leases as single-shot
    /// (acquire, do bounded work, drop) and size `writer_lease_micros`
    /// accordingly. Left in place pending that decision; do not delete.
    pub fn renew_writer_lease(&mut self, now: UtcMicros) -> Result<(), HookSpoolError> {
        self.ensure_live_lease(now)?;
        let renewed = HookSpoolWriterLeaseV1 {
            token: self.lease.token,
            expires_at: UtcMicros(
                now.0
                    .checked_add(self.config.writer_lease_micros)
                    .ok_or(HookSpoolError::InvalidLease)?,
            ),
        };
        write_lease_file(&mut self.lease_file, renewed)?;
        self.lease = renewed;
        Ok(())
    }

    /// Append one validated envelope. An exact pending `event_id` duplicate
    /// returns its existing record; reusing that ID for a different envelope
    /// is rejected. The append intent is persisted before frame publication,
    /// and the frame + containing directory are fsynced before the sequence is
    /// advanced.
    pub fn append(
        &mut self,
        envelope: HookEventEnvelopeV2,
        binding: &HookScopeBindingV1,
        now: UtcMicros,
    ) -> Result<HookSpoolRecordV1, HookSpoolError> {
        self.ensure_writable(now)?;
        envelope
            .validate(binding)
            .map_err(HookSpoolError::EnvelopeRejected)?;
        if envelope.producer != self.config.host {
            return Err(HookSpoolError::EnvelopeRejected(
                HookContractError::BindingMismatch,
            ));
        }
        let encoded =
            canonical_json_bytes(&envelope).map_err(|_| HookSpoolError::RecordTooLarge)?;
        if encoded.is_empty() || encoded.len() > MAX_HOOK_PAYLOAD_BYTES {
            return Err(HookSpoolError::RecordTooLarge);
        }
        if let Some(existing) = self
            .pending
            .iter()
            .find(|record| record.envelope.event_id == envelope.event_id)
        {
            return if existing.envelope == envelope {
                Ok(existing.clone())
            } else {
                Err(HookSpoolError::EventIdConflict)
            };
        }
        let sequence = self.meta.next_sequence;
        let frame = encode_frame(sequence, now, envelope.protected_session_id, &encoded)?;
        let frame_len = u64::try_from(frame.len()).map_err(|_| HookSpoolError::SpoolFull)?;
        self.ensure_append_capacity(&envelope, frame_len)?;
        if self.physical_len.saturating_add(frame_len) > self.config.limits.max_host_bytes {
            self.compact_pending()?;
        }
        if self.physical_len.saturating_add(frame_len) > self.config.limits.max_host_bytes {
            return Err(HookSpoolError::SpoolFull);
        }

        let intent = append_intent(sequence, self.physical_len, &frame)?;
        let mut intent_meta = self.meta.clone();
        intent_meta.append_intent = Some(intent);
        write_meta(&self.root, &intent_meta)?;
        self.meta = intent_meta;

        if let Err(error) = append_frame(&records_path(&self.root), &frame) {
            self.recovery_required = true;
            return Err(error);
        }
        let record = decode_complete_frame(&frame, 0, self.config.host)?;
        let mut committed_meta = self.meta.clone();
        committed_meta.next_sequence = sequence
            .checked_add(1)
            .ok_or(HookSpoolError::MetadataCorrupted)?;
        committed_meta.append_intent = None;
        if let Err(error) = write_meta(&self.root, &committed_meta) {
            self.recovery_required = true;
            return Err(error);
        }
        self.meta = committed_meta;
        self.physical_len = self.physical_len.saturating_add(frame_len);
        self.note_pending(&record)?;
        Ok(record)
    }

    /// Return up to four fair session batches. FIFO is preserved inside each
    /// session; a session with an in-flight claim is skipped until released.
    pub fn claim_replay_batches(
        &mut self,
        now: UtcMicros,
        requested_sessions: usize,
    ) -> Result<Vec<HookReplayBatchV1>, HookSpoolError> {
        self.ensure_healthy()?;
        let session_cap = requested_sessions.min(MAX_REPLAY_SESSIONS);
        if session_cap == 0 {
            return Ok(Vec::new());
        }
        let candidates = replayable_sessions(&self.pending, now);
        let ordered = round_robin_after(&candidates, self.round_robin_after);
        let mut selected = Vec::new();
        for session in ordered {
            if selected.len() == session_cap || self.replay_claims.contains_key(&session) {
                continue;
            }
            let records = batch_for_session(&self.pending, session, now)?;
            if records.is_empty() {
                continue;
            }
            let byte_count = records.iter().map(|record| record.framed_len).sum::<u32>();
            let claim_id = next_token();
            selected.push((
                session,
                claim_id,
                HookReplayBatchV1 {
                    claim_id,
                    protected_session_id: session,
                    records,
                    byte_count,
                },
            ));
        }
        if let Some((last_session, _, _)) = selected.last()
            && self.round_robin_after != Some(*last_session)
        {
            write_replay_cursor(&self.root, *last_session)?;
            self.round_robin_after = Some(*last_session);
        }
        let mut batches = Vec::with_capacity(selected.len());
        for (session, claim_id, batch) in selected {
            self.replay_claims.insert(session, claim_id);
            batches.push(batch);
        }
        Ok(batches)
    }

    /// Release an in-memory replay claim after a daemon transport attempt.
    /// Durable acknowledgements remain separate and are safe across restart.
    pub fn release_replay_claim(&mut self, claim_id: [u8; 16]) -> Result<(), HookSpoolError> {
        let session = self
            .replay_claims
            .iter()
            .find_map(|(session, active)| (*active == claim_id).then_some(*session))
            .ok_or(HookSpoolError::ReplayClaimUnknown)?;
        self.replay_claims.remove(&session);
        Ok(())
    }

    /// List records whose maximum transport age has elapsed. They remain
    /// durable until the daemon supplies a terminal tombstone acknowledgement.
    pub fn expired_records(&self, now: UtcMicros) -> Vec<HookSpoolRecordV1> {
        self.pending
            .iter()
            .filter(|record| is_expired(record, now))
            .cloned()
            .collect()
    }

    /// Persist one daemon acknowledgement and compact logically deleted
    /// frames. Out-of-order session acknowledgements are supported so fair
    /// replay never waits behind another session's transient saturation.
    pub fn acknowledge(
        &mut self,
        acknowledgement: HookSpoolAckV1,
        now: UtcMicros,
    ) -> Result<bool, HookSpoolError> {
        self.ensure_writable(now)?;
        if acknowledgement.sequence == 0 || acknowledgement.receipt_id == [0; 16] {
            return Err(HookSpoolError::AckConflict);
        }
        let existing = acknowledged_map(&self.meta)?;
        if acknowledgement.sequence <= self.meta.committed_through {
            return Ok(false);
        }
        if let Some(existing) = existing.get(&acknowledgement.sequence) {
            return if existing.receipt_id == acknowledgement.receipt_id
                && existing.disposition == acknowledgement.disposition
            {
                Ok(false)
            } else {
                Err(HookSpoolError::AckConflict)
            };
        }
        let index = self
            .pending
            .iter()
            .position(|record| record.sequence == acknowledgement.sequence)
            .ok_or(HookSpoolError::AckConflict)?;
        let removed = self.pending[index].clone();
        let mut next_meta = self.meta.clone();
        next_meta.acknowledged.push(AcknowledgedSequenceV1 {
            sequence: acknowledgement.sequence,
            receipt_id: acknowledgement.receipt_id,
            disposition: acknowledgement.disposition,
        });
        normalize_acknowledgements(&mut next_meta)?;
        write_meta(&self.root, &next_meta)?;
        self.meta = next_meta;
        self.pending.remove(index);
        self.release_usage(&removed);
        self.compact_pending()?;
        Ok(true)
    }

    fn ensure_append_capacity(
        &self,
        envelope: &HookEventEnvelopeV2,
        frame_len: u64,
    ) -> Result<(), HookSpoolError> {
        let control = matches!(
            envelope.event.family(),
            crate::HookEventFamily::SessionBoundary | crate::HookEventFamily::PromptBoundary
        );
        let host_record_limit = if control {
            self.config.limits.max_host_records
        } else {
            self.config
                .limits
                .max_host_records
                .saturating_sub(CONTROL_RECORD_RESERVE)
        };
        let host_byte_limit = if control {
            self.config.limits.max_host_bytes
        } else {
            self.config
                .limits
                .max_host_bytes
                .saturating_sub(CONTROL_FRAME_RESERVE_BYTES)
        };
        let host_records = u32::try_from(self.pending.len())
            .map_err(|_| HookSpoolError::SpoolFull)?
            .checked_add(1)
            .ok_or(HookSpoolError::SpoolFull)?;
        if host_records > host_record_limit
            || self.pending_bytes().saturating_add(frame_len) > host_byte_limit
        {
            return Err(HookSpoolError::SpoolFull);
        }
        let (records, bytes) = self
            .pending_by_session
            .get(&envelope.protected_session_id)
            .copied()
            .unwrap_or_default();
        let session_record_limit = if control {
            self.config.limits.max_session_records
        } else {
            self.config
                .limits
                .max_session_records
                .saturating_sub(CONTROL_RECORD_RESERVE)
        };
        let session_byte_limit = if control {
            self.config.limits.max_session_bytes
        } else {
            self.config
                .limits
                .max_session_bytes
                .saturating_sub(CONTROL_FRAME_RESERVE_BYTES)
        };
        if records.saturating_add(1) > session_record_limit
            || bytes.saturating_add(frame_len) > session_byte_limit
        {
            return Err(HookSpoolError::SpoolFull);
        }
        Ok(())
    }

    fn note_pending(&mut self, record: &HookSpoolRecordV1) -> Result<(), HookSpoolError> {
        let entry = self
            .pending_by_session
            .entry(record.protected_session_id)
            .or_default();
        entry.0 = entry.0.checked_add(1).ok_or(HookSpoolError::SpoolFull)?;
        entry.1 = entry.1.saturating_add(u64::from(record.framed_len));
        self.pending.push(record.clone());
        Ok(())
    }

    fn release_usage(&mut self, record: &HookSpoolRecordV1) {
        if let Some(entry) = self
            .pending_by_session
            .get_mut(&record.protected_session_id)
        {
            entry.0 = entry.0.saturating_sub(1);
            entry.1 = entry.1.saturating_sub(u64::from(record.framed_len));
            if entry.0 == 0 {
                self.pending_by_session.remove(&record.protected_session_id);
            }
        }
    }

    fn pending_bytes(&self) -> u64 {
        self.pending
            .iter()
            .map(|record| u64::from(record.framed_len))
            .sum()
    }

    fn compact_pending(&mut self) -> Result<(), HookSpoolError> {
        self.ensure_healthy()?;
        let mut bytes = Vec::with_capacity(self.pending_bytes() as usize);
        let mut offset = 0u64;
        let mut rebuilt = Vec::with_capacity(self.pending.len());
        for record in &self.pending {
            let payload = canonical_json_bytes(&record.envelope)
                .map_err(|_| HookSpoolError::MetadataCorrupted)?;
            let frame = encode_frame(
                record.sequence,
                record.queued_at,
                record.protected_session_id,
                &payload,
            )?;
            let rebuilt_record = decode_complete_frame(&frame, offset, self.config.host)?;
            offset = offset.saturating_add(frame.len() as u64);
            bytes.extend_from_slice(&frame);
            rebuilt.push(rebuilt_record);
        }
        shared_atomic_write(
            &records_path(&self.root),
            "records",
            &bytes,
            DIRECTORY_POLICY,
        )
        .map_err(|_| HookSpoolError::Io)?;
        self.pending = rebuilt;
        self.physical_len = offset;
        Ok(())
    }

    fn ensure_live_lease(&self, now: UtcMicros) -> Result<(), HookSpoolError> {
        if self.lease.expires_at.0 <= now.0 {
            return Err(HookSpoolError::WriterLeaseLost);
        }
        Ok(())
    }

    fn ensure_healthy(&self) -> Result<(), HookSpoolError> {
        match self.meta.integrity {
            SpoolIntegrityV1::Healthy => Ok(()),
            SpoolIntegrityV1::Corrupted { at_offset } => {
                Err(HookSpoolError::Corrupted { at_offset })
            }
        }
    }

    fn ensure_writable(&self, now: UtcMicros) -> Result<(), HookSpoolError> {
        self.ensure_healthy()?;
        if self.recovery_required {
            return Err(HookSpoolError::RecoveryRequired);
        }
        self.ensure_live_lease(now)
    }
}

impl Drop for HookSpoolV1 {
    fn drop(&mut self) {
        let _ = self.lease_file.unlock();
    }
}

fn records_path(root: &Path) -> PathBuf {
    root.join(RECORDS_FILE)
}

fn meta_path(root: &Path) -> PathBuf {
    root.join(META_FILE)
}

fn lease_path(root: &Path) -> PathBuf {
    root.join(LEASE_FILE)
}

fn replay_cursor_path(root: &Path) -> PathBuf {
    root.join(REPLAY_CURSOR_FILE)
}

fn ensure_root(root: &Path) -> Result<(), HookSpoolError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(HookSpoolError::UnsafePath);
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(HookSpoolError::Io),
    }
    fs::create_dir_all(root).map_err(|_| HookSpoolError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))
            .map_err(|_| HookSpoolError::Io)?;
    }
    shared_sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)
}

fn validate_regular_or_missing(path: &Path) -> Result<bool, HookSpoolError> {
    shared_validate_regular(path).map_err(|_| HookSpoolError::UnsafePath)
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, HookSpoolError> {
    match shared_read_bounded(path, maximum) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            Err(HookSpoolError::UnsafePath)
        }
        Err(_) => Err(HookSpoolError::MetadataCorrupted),
    }
}

fn read_meta(root: &Path) -> Result<Option<HookSpoolMetaV1>, HookSpoolError> {
    read_bounded(&meta_path(root), MAX_META_BYTES)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| HookSpoolError::MetadataCorrupted))
        .transpose()
}

fn write_meta(root: &Path, meta: &HookSpoolMetaV1) -> Result<(), HookSpoolError> {
    let bytes = serde_json::to_vec(meta).map_err(|_| HookSpoolError::MetadataCorrupted)?;
    if bytes.len() > MAX_META_BYTES {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    shared_atomic_write(&meta_path(root), "meta", &bytes, DIRECTORY_POLICY)
        .map_err(|_| HookSpoolError::Io)
}

fn read_replay_cursor(root: &Path) -> Result<Option<[u8; 32]>, HookSpoolError> {
    read_bounded(&replay_cursor_path(root), 32)?
        .map(|bytes| {
            bytes
                .try_into()
                .map_err(|_| HookSpoolError::MetadataCorrupted)
        })
        .transpose()
}

fn write_replay_cursor(root: &Path, cursor: [u8; 32]) -> Result<(), HookSpoolError> {
    shared_atomic_write(
        &replay_cursor_path(root),
        "replay-cursor",
        &cursor,
        DIRECTORY_POLICY,
    )
    .map_err(|_| HookSpoolError::Io)
}

fn write_lease_file(file: &mut File, lease: HookSpoolWriterLeaseV1) -> Result<(), HookSpoolError> {
    let bytes = serde_json::to_vec(&LeaseFileV1 {
        version: SPOOL_FORMAT_VERSION,
        token: lease.token,
        expires_at: lease.expires_at,
    })
    .map_err(|_| HookSpoolError::InvalidLease)?;
    if bytes.is_empty() || bytes.len() > MAX_LEASE_BYTES {
        return Err(HookSpoolError::InvalidLease);
    }
    file.set_len(0).map_err(|_| HookSpoolError::Io)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|_| HookSpoolError::Io)?;
    file.write_all(&bytes).map_err(|_| HookSpoolError::Io)?;
    file.sync_all().map_err(|_| HookSpoolError::Io)
}

fn acquire_lease(
    root: &Path,
    lease_duration_micros: i64,
    now: UtcMicros,
) -> Result<(HookSpoolWriterLeaseV1, File), HookSpoolError> {
    let expires_at = UtcMicros(
        now.0
            .checked_add(lease_duration_micros)
            .ok_or(HookSpoolError::InvalidLease)?,
    );
    let candidate = HookSpoolWriterLeaseV1 {
        token: next_token(),
        expires_at,
    };
    let path = lease_path(root);
    validate_regular_or_missing(&path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|_| HookSpoolError::Io)?;
    if !validate_regular_or_missing(&path)? {
        return Err(HookSpoolError::UnsafePath);
    }
    file.try_lock().map_err(map_try_lock_error)?;
    write_lease_file(&mut file, candidate)?;
    shared_sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)?;
    Ok((candidate, file))
}

fn map_try_lock_error(error: std::fs::TryLockError) -> HookSpoolError {
    match error {
        std::fs::TryLockError::WouldBlock => HookSpoolError::WriterLeaseHeld,
        std::fs::TryLockError::Error(_) => HookSpoolError::Io,
    }
}

fn append_frame(path: &Path, frame: &[u8]) -> Result<(), HookSpoolError> {
    append_durable(path, frame, DIRECTORY_POLICY)
        .map(|_| ())
        .map_err(|_| HookSpoolError::Io)
}

fn truncate_records(root: &Path, length: u64) -> Result<(), HookSpoolError> {
    shared_truncate_file(&records_path(root), length, DIRECTORY_POLICY)
        .map_err(|_| HookSpoolError::Io)
}

fn scan_records(root: &Path, config: HookSpoolConfigV1) -> Result<ScanResult, HookSpoolError> {
    let path = records_path(root);
    if !validate_regular_or_missing(&path)? {
        return Ok(ScanResult {
            records: Vec::new(),
            valid_end: 0,
            physical_len: 0,
            partial_tail: None,
            corruption: None,
        });
    }
    let physical_len = fs::metadata(&path).map_err(|_| HookSpoolError::Io)?.len();
    if physical_len > config.limits.max_host_bytes {
        return Err(HookSpoolError::SpoolFull);
    }
    let mut file = File::open(&path).map_err(|_| HookSpoolError::Io)?;
    let mut records = Vec::new();
    let mut offset = 0u64;
    let mut previous_sequence = None;
    while offset < physical_len {
        let remaining = physical_len - offset;
        if remaining < FRAME_LENGTH_BYTES as u64 {
            return partial_scan(records, offset, physical_len, &mut file);
        }
        let mut prefix = [0u8; FRAME_LENGTH_BYTES];
        file.read_exact(&mut prefix)
            .map_err(|_| HookSpoolError::Io)?;
        let declared = u32::from_le_bytes(prefix) as usize;
        let minimum = FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES;
        let maximum = minimum
            .checked_add(MAX_HOOK_PAYLOAD_BYTES)
            .ok_or(HookSpoolError::MetadataCorrupted)?;
        if declared < minimum || declared > maximum {
            return Ok(corrupt_scan(records, offset, physical_len));
        }
        let frame_len = FRAME_LENGTH_BYTES
            .checked_add(declared)
            .ok_or(HookSpoolError::MetadataCorrupted)?;
        if frame_len as u64 > remaining {
            file.seek(SeekFrom::Start(offset))
                .map_err(|_| HookSpoolError::Io)?;
            return partial_scan(records, offset, physical_len, &mut file);
        }
        let mut frame = Vec::with_capacity(frame_len);
        frame.extend_from_slice(&prefix);
        let mut body = vec![0u8; declared];
        file.read_exact(&mut body).map_err(|_| HookSpoolError::Io)?;
        frame.extend_from_slice(&body);
        let record = match decode_complete_frame(&frame, offset, config.host) {
            Ok(record) => record,
            Err(HookSpoolError::Corrupted { .. }) | Err(HookSpoolError::MetadataCorrupted) => {
                return Ok(corrupt_scan(records, offset, physical_len));
            }
            Err(error) => return Err(error),
        };
        if previous_sequence.is_some_and(|previous| record.sequence <= previous)
            || records.len() >= config.limits.max_host_records as usize
        {
            return Ok(corrupt_scan(records, offset, physical_len));
        }
        previous_sequence = Some(record.sequence);
        offset = offset.saturating_add(u64::from(record.framed_len));
        records.push(record);
    }
    Ok(ScanResult {
        records,
        valid_end: offset,
        physical_len,
        partial_tail: None,
        corruption: None,
    })
}

fn partial_scan(
    records: Vec<HookSpoolRecordV1>,
    offset: u64,
    physical_len: u64,
    file: &mut File,
) -> Result<ScanResult, HookSpoolError> {
    let mut partial_tail = Vec::with_capacity((physical_len - offset) as usize);
    file.read_to_end(&mut partial_tail)
        .map_err(|_| HookSpoolError::Io)?;
    Ok(ScanResult {
        records,
        valid_end: offset,
        physical_len,
        partial_tail: Some(partial_tail),
        corruption: None,
    })
}

fn corrupt_scan(records: Vec<HookSpoolRecordV1>, offset: u64, physical_len: u64) -> ScanResult {
    ScanResult {
        records,
        valid_end: offset,
        physical_len,
        partial_tail: None,
        corruption: Some(offset),
    }
}

fn encode_frame(
    sequence: u64,
    queued_at: UtcMicros,
    protected_session_id: [u8; 32],
    payload: &[u8],
) -> Result<Vec<u8>, HookSpoolError> {
    if sequence == 0 || payload.is_empty() || payload.len() > MAX_HOOK_PAYLOAD_BYTES {
        return Err(HookSpoolError::RecordTooLarge);
    }
    let body_len = FRAME_HEADER_BYTES
        .checked_add(payload.len())
        .and_then(|length| length.checked_add(FRAME_CHECKSUM_BYTES))
        .ok_or(HookSpoolError::RecordTooLarge)?;
    let body_len = u32::try_from(body_len).map_err(|_| HookSpoolError::RecordTooLarge)?;
    let mut frame = Vec::with_capacity(FRAME_LENGTH_BYTES + body_len as usize);
    frame.extend_from_slice(&body_len.to_le_bytes());
    frame.extend_from_slice(SPOOL_MAGIC);
    frame.extend_from_slice(&SPOOL_FORMAT_VERSION.to_le_bytes());
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&queued_at.0.to_le_bytes());
    frame.extend_from_slice(&protected_session_id);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    let checksum = frame_checksum(&frame);
    frame.extend_from_slice(&checksum);
    Ok(frame)
}

fn decode_complete_frame(
    frame: &[u8],
    file_offset: u64,
    host: HookHostV1,
) -> Result<HookSpoolRecordV1, HookSpoolError> {
    let minimum = FRAME_LENGTH_BYTES + FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES;
    if frame.len() < minimum {
        return Err(HookSpoolError::Corrupted {
            at_offset: file_offset,
        });
    }
    let declared = u32::from_le_bytes(
        frame[..4]
            .try_into()
            .map_err(|_| HookSpoolError::MetadataCorrupted)?,
    ) as usize;
    if declared + FRAME_LENGTH_BYTES != frame.len()
        || &frame[4..8] != SPOOL_MAGIC
        || u16::from_le_bytes([frame[8], frame[9]]) != SPOOL_FORMAT_VERSION
    {
        return Err(HookSpoolError::Corrupted {
            at_offset: file_offset,
        });
    }
    let checksum_at = frame.len() - FRAME_CHECKSUM_BYTES;
    let checksum: [u8; 32] = frame[checksum_at..]
        .try_into()
        .map_err(|_| HookSpoolError::MetadataCorrupted)?;
    if frame_checksum(&frame[..checksum_at]) != checksum {
        return Err(HookSpoolError::Corrupted {
            at_offset: file_offset,
        });
    }
    let sequence = u64::from_le_bytes(
        frame[10..18]
            .try_into()
            .map_err(|_| HookSpoolError::MetadataCorrupted)?,
    );
    let queued_at = UtcMicros(i64::from_le_bytes(
        frame[18..26]
            .try_into()
            .map_err(|_| HookSpoolError::MetadataCorrupted)?,
    ));
    let protected_session_id = frame[26..58]
        .try_into()
        .map_err(|_| HookSpoolError::MetadataCorrupted)?;
    let payload_len = u32::from_le_bytes(
        frame[58..62]
            .try_into()
            .map_err(|_| HookSpoolError::MetadataCorrupted)?,
    ) as usize;
    if sequence == 0
        || payload_len == 0
        || payload_len > MAX_HOOK_PAYLOAD_BYTES
        || 62usize.saturating_add(payload_len) != checksum_at
    {
        return Err(HookSpoolError::Corrupted {
            at_offset: file_offset,
        });
    }
    let payload = &frame[62..checksum_at];
    let envelope =
        decode_hook_event_envelope_compat(payload).map_err(|_| HookSpoolError::Corrupted {
            at_offset: file_offset,
        })?;
    if envelope.producer != host || envelope.protected_session_id != protected_session_id {
        return Err(HookSpoolError::Corrupted {
            at_offset: file_offset,
        });
    }
    Ok(HookSpoolRecordV1 {
        sequence,
        protected_session_id,
        queued_at,
        envelope,
        encoded_len: u32::try_from(payload_len).map_err(|_| HookSpoolError::MetadataCorrupted)?,
        checksum,
        framed_len: u32::try_from(frame.len()).map_err(|_| HookSpoolError::MetadataCorrupted)?,
    })
}

fn append_intent(
    sequence: u64,
    file_offset: u64,
    frame: &[u8],
) -> Result<AppendIntentV1, HookSpoolError> {
    Ok(AppendIntentV1 {
        sequence,
        file_offset,
        framed_len: u32::try_from(frame.len()).map_err(|_| HookSpoolError::MetadataCorrupted)?,
        frame: frame.to_vec(),
    })
}

fn partial_tail_matches_intent(meta: &HookSpoolMetaV1, offset: u64, partial: &[u8]) -> bool {
    let Some(intent) = &meta.append_intent else {
        return false;
    };
    intent.sequence == meta.next_sequence
        && intent.file_offset == offset
        && partial_tail_matches_prefix(partial, &intent.frame, intent.framed_len as usize)
}

fn reconcile_append_intent(
    meta: &mut HookSpoolMetaV1,
    records: &[HookSpoolRecordV1],
    host: HookHostV1,
) -> Result<(), HookSpoolError> {
    let Some(intent) = meta.append_intent.clone() else {
        return Ok(());
    };
    if intent.sequence != meta.next_sequence || !valid_append_intent(&intent, host) {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    if let Some(record) = records
        .iter()
        .find(|record| record.sequence == intent.sequence)
    {
        let payload = canonical_json_bytes(&record.envelope)
            .map_err(|_| HookSpoolError::MetadataCorrupted)?;
        let frame = encode_frame(
            record.sequence,
            record.queued_at,
            record.protected_session_id,
            &payload,
        )?;
        if record.framed_len != intent.framed_len || frame != intent.frame {
            return Err(HookSpoolError::MetadataCorrupted);
        }
        meta.next_sequence = meta
            .next_sequence
            .checked_add(1)
            .ok_or(HookSpoolError::MetadataCorrupted)?;
    }
    meta.append_intent = None;
    Ok(())
}

fn validate_meta(
    meta: &HookSpoolMetaV1,
    limits: HookSpoolLimitsV1,
    host: HookHostV1,
) -> Result<(), HookSpoolError> {
    if meta.version != SPOOL_META_VERSION
        || meta.next_sequence == 0
        || meta.next_sequence <= meta.committed_through
        || meta.acknowledged.len() > limits.max_host_records as usize
    {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    let _ = acknowledged_map(meta)?;
    if let Some(intent) = &meta.append_intent
        && (intent.sequence != meta.next_sequence
            || intent.framed_len
                < (FRAME_LENGTH_BYTES + FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES) as u32
            || !valid_append_intent(intent, host)
            || intent
                .file_offset
                .checked_add(u64::from(intent.framed_len))
                .is_none())
    {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    Ok(())
}

fn valid_append_intent(intent: &AppendIntentV1, host: HookHostV1) -> bool {
    let minimum = FRAME_LENGTH_BYTES + FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES;
    if intent.sequence == 0
        || intent.frame.len() < minimum
        || intent.frame.len() != intent.framed_len as usize
        || intent.frame.get(4..8) != Some(SPOOL_MAGIC.as_slice())
    {
        return false;
    }
    let Some(sequence) = intent.frame.get(10..18) else {
        return false;
    };
    let Ok(sequence) = <[u8; 8]>::try_from(sequence) else {
        return false;
    };
    let checksum_at = intent.frame.len() - FRAME_CHECKSUM_BYTES;
    let Some(checksum) = intent.frame.get(checksum_at..) else {
        return false;
    };
    intent.sequence == u64::from_le_bytes(sequence)
        && frame_checksum(&intent.frame[..checksum_at]) == checksum
        && decode_complete_frame(&intent.frame, intent.file_offset, host).is_ok()
}

fn validate_meta_against_records(
    meta: &HookSpoolMetaV1,
    records: &[HookSpoolRecordV1],
    limits: HookSpoolLimitsV1,
) -> Result<(), HookSpoolError> {
    let outstanding = meta
        .next_sequence
        .checked_sub(meta.committed_through)
        .and_then(|distance| distance.checked_sub(1))
        .ok_or(HookSpoolError::MetadataCorrupted)?;
    if outstanding > limits.max_host_records as u64 {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    let acknowledged = acknowledged_map(meta)?;
    let present = records
        .iter()
        .map(|record| record.sequence)
        .collect::<BTreeSet<_>>();
    if records
        .iter()
        .any(|record| record.sequence >= meta.next_sequence)
    {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    for sequence in meta.committed_through.saturating_add(1)..meta.next_sequence {
        if !acknowledged.contains_key(&sequence) && !present.contains(&sequence) {
            return Err(HookSpoolError::MetadataCorrupted);
        }
    }
    Ok(())
}

fn acknowledged_map(
    meta: &HookSpoolMetaV1,
) -> Result<BTreeMap<u64, AcknowledgedSequenceV1>, HookSpoolError> {
    let mut entries = BTreeMap::new();
    for entry in &meta.acknowledged {
        if entry.sequence <= meta.committed_through
            || entry.sequence >= meta.next_sequence
            || entry.receipt_id == [0; 16]
            || entries.insert(entry.sequence, *entry).is_some()
        {
            return Err(HookSpoolError::MetadataCorrupted);
        }
    }
    Ok(entries)
}

fn normalize_acknowledgements(meta: &mut HookSpoolMetaV1) -> Result<(), HookSpoolError> {
    let mut map = acknowledged_map(meta)?;
    while let Some(next) = meta.committed_through.checked_add(1) {
        if map.remove(&next).is_some() {
            meta.committed_through = next;
        } else {
            break;
        }
    }
    meta.acknowledged = map.into_values().collect();
    Ok(())
}

fn next_token() -> [u8; 16] {
    static TOKEN_NONCE: AtomicU64 = AtomicU64::new(1);
    let nonce = TOKEN_NONCE.fetch_add(1, Ordering::Relaxed);
    let mut token = [0u8; 16];
    token[..8].copy_from_slice(&nonce.to_le_bytes());
    token[8..12].copy_from_slice(&std::process::id().to_le_bytes());
    token[12..].copy_from_slice(&(nonce as u32).rotate_left(13).to_le_bytes());
    token
}

/// SHA-256 over exact spool framing bytes.
pub fn hook_spool_checksum(input: &[u8]) -> [u8; 32] {
    frame_checksum(input)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::{
        HookCapabilityV1, HookEventFamily, HookEventSupportV1, HookEventV2, HookOrderingV1,
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
    fn replay_decoder_migrates_retained_authorization_era_frame() {
        let payload = include_bytes!("../fixtures/envelopes/authorization-era-saved-edit.json");
        let frame = encode_frame(1, UtcMicros(10), [8; 32], payload).unwrap();
        let record = decode_complete_frame(&frame, 0, HookHostV1::CursorDesktop).unwrap();

        assert_eq!(record.sequence, 1);
        assert_eq!(
            record.envelope.event,
            HookEventV2::SavedEdit {
                file_id: [11; 16],
                changed_range_count: 1,
            }
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
        let mut output = OpenOptions::new()
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
    fn standard_try_lock_errors_keep_contention_distinct_from_io() {
        assert_eq!(
            map_try_lock_error(std::fs::TryLockError::WouldBlock),
            HookSpoolError::WriterLeaseHeld
        );
        assert_eq!(
            map_try_lock_error(std::fs::TryLockError::Error(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "denied",
            ))),
            HookSpoolError::Io
        );
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
}
