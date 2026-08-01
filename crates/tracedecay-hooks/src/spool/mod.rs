//! Transport-only append-only Hook V2 replay spool.
//!
//! This is intentionally not a database or product queue. It persists only
//! already-validated, content-free [`crate::HookEventEnvelopeV2`] bytes plus
//! framing/checksum metadata. The daemon owns replay authorization and every
//! acknowledgement; this module only makes those transitions crash-safe.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tracedecay_application::framed_log::{
    DirectorySyncPolicy, atomic_write as shared_atomic_write, read_bounded as shared_read_bounded,
    sync_directory as shared_sync_directory,
    validate_regular_or_missing as shared_validate_regular,
};
use tracedecay_domain::{
    UtcMicros, canonical_json_bytes,
    framed_log::{self, checksum as frame_checksum},
};

use crate::{
    HookContractError, HookEventEnvelopeV2, HookScopeBindingV1, MAX_HOOK_PAYLOAD_BYTES,
    MAX_REPLAY_BATCH_BYTES, MAX_REPLAY_BATCH_RECORDS, MAX_SPOOL_AGE_MICROS,
};

mod frame;
mod lease;
mod meta;
mod replay;
mod types;

use types::{AcknowledgedSequenceV1, HookSpoolMetaV1, SpoolIntegrityV1};
pub use types::{
    HookReplayBatchV1, HookSpoolAckDispositionV1, HookSpoolAckV1, HookSpoolConfigV1,
    HookSpoolError, HookSpoolLimitsV1, HookSpoolOpenReportV1, HookSpoolRecordV1,
    HookSpoolWriterLeaseV1,
};

use frame::{append_frame, decode_complete_frame, encode_frame, scan_records, truncate_records};
use lease::acquire_lease;
use meta::{
    acknowledged_map, append_intent, normalize_acknowledgements, partial_tail_matches_intent,
    read_meta, reconcile_append_intent, validate_meta, validate_meta_against_records, write_meta,
};
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
    ///
    /// The lease is single-shot and non-renewable: open, do bounded work with
    /// the `now` the lease was acquired at, drop. A handle held past
    /// `config.writer_lease_micros` of caller-observed time stops accepting
    /// mutations with [`HookSpoolError::WriterLeaseLost`]; the only recovery is
    /// to drop it and reopen, which is lossless because every record and
    /// acknowledgement is durable before its call returns.
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
mod tests;
