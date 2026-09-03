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

use tracedecay_domain::{
    UtcMicros, canonical_json_bytes,
    framed_log::{self, checksum as frame_checksum},
};
use tracedecay_private_fs::framed_log::{
    DirectorySyncPolicy, atomic_write as shared_atomic_write, read_bounded as shared_read_bounded,
    sync_directory as shared_sync_directory,
    validate_regular_or_missing as shared_validate_regular,
};

use crate::{
    HookContractError, HookEventEnvelopeV2, HookScopeBindingV1, MAX_HOOK_PAYLOAD_BYTES,
    MAX_REPLAY_BATCH_BYTES, MAX_REPLAY_BATCH_RECORDS, MAX_SPOOL_AGE_MICROS,
};

mod checkpoint;
mod frame;
mod lease;
mod meta;
mod replay;
mod types;

#[cfg(test)]
use checkpoint::{CHECKPOINT_ENTRY_BYTES, CHECKPOINT_HEADER_BYTES, CHECKPOINT_MAGIC};
use checkpoint::{
    CHECKPOINT_REWRITE_BYTE_THRESHOLD, CHECKPOINT_REWRITE_FRAME_THRESHOLD, CheckpointAnchorV1,
    RecordsFileRevisionV1, read_checkpoint, read_frame_at, read_transition, records_file_revision,
    write_checkpoint, write_transition,
};
use types::{AcknowledgedSequenceV1, HookSpoolMetaV1, PendingRecordV1, SpoolIntegrityV1};
pub use types::{
    HookReplayBatchV1, HookSpoolAckDispositionV1, HookSpoolAckV1, HookSpoolConfigV1,
    HookSpoolError, HookSpoolLimitsV1, HookSpoolOpenReportV1, HookSpoolRecordV1,
    HookSpoolResetReasonV1, HookSpoolWriterLeaseV1,
};

use frame::{
    append_frame, decode_complete_frame, encode_frame, scan_records, scan_records_from,
    truncate_records,
};
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
// Member filenames retain the spool layout generation; this header version owns the body shape.
const CHECKPOINT_FORMAT_VERSION: u16 = 2;
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
const CHECKPOINT_FILE: &str = "checkpoint.v1.bin";
const TRANSITION_FILE: &str = "checkpoint-transition.v1.json";
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
    checkpoint: Option<CheckpointAnchorV1>,
    observed_records_revision: Option<RecordsFileRevisionV1>,
    pending: Vec<PendingRecordV1>,
    pending_by_session: BTreeMap<[u8; 32], (u32, u64)>,
    physical_len: u64,
    round_robin_after: Option<[u8; 32]>,
    replay_claims: BTreeMap<[u8; 32], [u8; 16]>,
    recovery_required: bool,
    /// Held for its `Drop` only: closes the writer-lease hold observation.
    #[cfg(feature = "hotpath")]
    _lease_hold: SpoolLeaseHoldObservationV1,
}

#[cfg(feature = "hotpath")]
static SPOOL_LEASES_HELD: AtomicU64 = AtomicU64::new(0);

/// Writer-lease hold observation. Acquisition wait is the
/// `hooks.spool.acquire_lease` span; this records how long the sole writer
/// lease is then *held* (open handle lifetime), which is what other writers
/// contend against. Drop-based so panic or early return cannot leak the gauge.
#[cfg(feature = "hotpath")]
#[derive(Debug)]
struct SpoolLeaseHoldObservationV1 {
    acquired: std::time::Instant,
}

#[cfg(feature = "hotpath")]
impl SpoolLeaseHoldObservationV1 {
    fn enter() -> Self {
        let held = SPOOL_LEASES_HELD
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        hotpath::gauge!("hooks.spool.lease.held").set(held);
        Self {
            acquired: std::time::Instant::now(),
        }
    }
}

#[cfg(feature = "hotpath")]
impl Drop for SpoolLeaseHoldObservationV1 {
    fn drop(&mut self) {
        let _ = SPOOL_LEASES_HELD.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
            held.checked_sub(1)
        });
        hotpath::gauge!("hooks.spool.lease.held").set(SPOOL_LEASES_HELD.load(Ordering::Relaxed));
        hotpath::gauge!("hooks.spool.lease.hold_micros")
            .set(u64::try_from(self.acquired.elapsed().as_micros()).unwrap_or(u64::MAX));
    }
}

impl HookSpoolV1 {
    /// Explicitly recreate one exact host spool without decoding incompatible
    /// metadata, records, or cursors. The normal writer lease still fences a
    /// live adapter, and only the incompatible transport-owned files are
    /// removed.
    #[hotpath::measure(label = "hooks.spool.reset")]
    pub fn reset(
        root: impl Into<PathBuf>,
        config: HookSpoolConfigV1,
        now: UtcMicros,
    ) -> Result<(), HookSpoolError> {
        config.validate()?;
        let root = root.into();
        ensure_root(&root)?;
        let (_lease, lease_file) = acquire_lease(&root, config.writer_lease_micros, now)?;
        for path in [
            records_path(&root),
            meta_path(&root),
            checkpoint_path(&root),
            transition_path(&root),
            replay_cursor_path(&root),
        ] {
            remove_spool_member(&path)?;
        }
        hotpath::measure_block!("hooks.spool.fsync.directory", {
            shared_sync_directory(&root, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)
        })?;
        drop(lease_file);
        Ok(())
    }

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
    #[hotpath::measure(label = "hooks.spool.open")]
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

    #[hotpath::measure(label = "hooks.spool.open_after_lease")]
    fn open_after_lease(
        root: PathBuf,
        config: HookSpoolConfigV1,
        lease: HookSpoolWriterLeaseV1,
        lease_file: File,
        _now: UtcMicros,
    ) -> Result<(Self, HookSpoolOpenReportV1), HookSpoolError> {
        let stored_meta = read_meta(&root)?;
        let meta_was_missing = stored_meta.is_none();
        let mut meta = stored_meta.unwrap_or_else(HookSpoolMetaV1::fresh);
        validate_meta(&meta, config.limits, config.host)?;
        let current_revision = records_file_revision(&root)?;
        let cached_checkpoint = read_checkpoint(&root, config)?;
        let checkpoint_bytes = cached_checkpoint
            .as_ref()
            .map_or(0, |checkpoint| checkpoint.bytes);
        let mut checkpoint_records = 0u32;
        let mut checkpoint_highest_sequence = None;
        let (mut scan, reusable_checkpoint) = match cached_checkpoint {
            Some(checkpoint) if checkpoint.records_revision == current_revision => {
                checkpoint_records = u32::try_from(checkpoint.records.len())
                    .map_err(|_| HookSpoolError::MetadataCorrupted)?;
                checkpoint_highest_sequence =
                    checkpoint.records.last().map(|record| record.sequence);
                let validated_end = checkpoint
                    .records_revision
                    .as_ref()
                    .map_or(0, |revision| revision.length);
                let anchor = CheckpointAnchorV1 {
                    records_revision: checkpoint.records_revision.clone(),
                    checksum: checkpoint.checksum,
                };
                (
                    scan_records_from(&root, config, checkpoint.records, validated_end)?,
                    Some(anchor),
                )
            }
            Some(checkpoint) => {
                let transition = read_transition(&root)?;
                let validated_end = checkpoint
                    .records_revision
                    .as_ref()
                    .map_or(0, |revision| revision.length);
                let transition_matches = transition.as_ref().is_some_and(|transition| {
                    transition.checkpoint_checksum == checkpoint.checksum
                        && transition.checkpoint_revision == checkpoint.records_revision
                        && Some(&transition.current_revision) == current_revision.as_ref()
                        && transition.current_revision.length >= validated_end
                });
                if transition_matches {
                    checkpoint_records = u32::try_from(checkpoint.records.len())
                        .map_err(|_| HookSpoolError::MetadataCorrupted)?;
                    checkpoint_highest_sequence =
                        checkpoint.records.last().map(|record| record.sequence);
                    let anchor = CheckpointAnchorV1 {
                        records_revision: checkpoint.records_revision.clone(),
                        checksum: checkpoint.checksum,
                    };
                    (
                        scan_records_from(&root, config, checkpoint.records, validated_end)?,
                        Some(anchor),
                    )
                } else {
                    (scan_records(&root, config)?, None)
                }
            }
            None => (scan_records(&root, config)?, None),
        };
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

        let checkpoint_suffix_bytes =
            reusable_checkpoint
                .as_ref()
                .map_or(scan.valid_end, |anchor| {
                    scan.valid_end.saturating_sub(
                        anchor
                            .records_revision
                            .as_ref()
                            .map_or(0, |revision| revision.length),
                    )
                });
        let rewrite_checkpoint = reusable_checkpoint.is_none()
            || scan.scanned_records >= CHECKPOINT_REWRITE_FRAME_THRESHOLD
            || checkpoint_suffix_bytes >= CHECKPOINT_REWRITE_BYTE_THRESHOLD;
        let mut checkpoint_rewritten = false;
        let checkpoint = if matches!(meta.integrity, SpoolIntegrityV1::Healthy) {
            let unreconciled_meta = meta.clone();
            if meta.append_intent.as_ref().is_some_and(|intent| {
                checkpoint_highest_sequence.is_some_and(|highest| intent.sequence <= highest)
            }) {
                return Err(HookSpoolError::MetadataCorrupted);
            }
            let suffix_at = usize::try_from(checkpoint_records)
                .map_err(|_| HookSpoolError::MetadataCorrupted)?;
            let suffix_records = scan
                .records
                .get(suffix_at..)
                .ok_or(HookSpoolError::MetadataCorrupted)?;
            reconcile_append_intent(&mut meta, suffix_records, config.host)?;
            validate_meta_against_records(
                &meta,
                scan.records.iter().map(|record| record.sequence),
                config.limits,
            )?;
            if meta_was_missing || meta != unreconciled_meta {
                write_meta(&root, &meta)?;
            }
            Some(match (reusable_checkpoint, rewrite_checkpoint) {
                (Some(checkpoint), false) => checkpoint,
                _ => {
                    checkpoint_rewritten = true;
                    write_checkpoint(&root, config, &scan.records)?
                }
            })
        } else {
            None
        };

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
            pending_bytes: pending_by_session.values().map(|(_, bytes)| *bytes).sum(),
            committed_through: meta.committed_through,
            next_sequence: meta.next_sequence,
            scanned_records: scan.scanned_records,
            checkpoint_records,
            checkpoint_bytes,
            checkpoint_rewritten,
            truncated_partial_tail_bytes,
            corrupted_at_offset: match meta.integrity {
                SpoolIntegrityV1::Healthy => None,
                SpoolIntegrityV1::Corrupted { at_offset } => Some(at_offset),
            },
        };
        let round_robin_after = read_replay_cursor(&root)?;
        let observed_records_revision = if checkpoint_rewritten {
            checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.records_revision.clone())
        } else {
            current_revision
        };
        let mut spool = Self {
            root,
            config,
            lease,
            lease_file,
            meta,
            checkpoint,
            observed_records_revision,
            pending,
            pending_by_session,
            physical_len: scan.physical_len,
            round_robin_after,
            replay_claims: BTreeMap::new(),
            recovery_required: false,
            #[cfg(feature = "hotpath")]
            _lease_hold: SpoolLeaseHoldObservationV1::enter(),
        };
        // A crash may leave logically acknowledged frames in the active file.
        // Metadata is already durable, so this recovery compaction is safe.
        if matches!(spool.meta.integrity, SpoolIntegrityV1::Healthy)
            && spool.physical_len > spool.pending_bytes()
        {
            spool.compact_pending()?;
        }
        hotpath::gauge!("hooks.spool.pending.frame_count").set(report.pending_records);
        hotpath::gauge!("hooks.spool.pending.bytes").set(report.pending_bytes);
        Ok((spool, report))
    }

    pub fn lease(&self) -> HookSpoolWriterLeaseV1 {
        self.lease
    }

    /// Return the durable pending envelope for an exact provider event ID.
    /// Callers use this only to preserve a prior transport attempt's envelope
    /// on retry; it does not grant replay or acknowledgement authority.
    pub fn pending_envelope(
        &mut self,
        event_id: [u8; 16],
    ) -> Result<Option<HookEventEnvelopeV2>, HookSpoolError> {
        let Some(index) = self
            .pending
            .iter()
            .position(|record| record.event_id == event_id)
        else {
            return Ok(None);
        };
        self.hydrate(index).map(|record| Some(record.envelope))
    }

    /// Append one validated envelope. An exact pending `event_id` duplicate
    /// returns its existing record; reusing that ID for a different envelope
    /// is rejected. The append intent is persisted before frame publication,
    /// and the frame + containing directory are fsynced before the sequence is
    /// advanced.
    #[hotpath::measure(label = "hooks.spool.append")]
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
        if let Some(index) = self
            .pending
            .iter()
            .position(|record| record.event_id == envelope.event_id)
        {
            let existing = self.hydrate(index)?;
            return if existing.envelope == envelope {
                Ok(existing)
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
        if records_file_revision(&self.root)? != self.observed_records_revision {
            self.recovery_required = true;
            return Err(HookSpoolError::MetadataCorrupted);
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
        let record = decode_complete_frame(&frame, self.physical_len, self.config.host)?;
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
        self.note_pending(&record, self.physical_len.saturating_sub(frame_len))?;
        let Some(checkpoint) = self.checkpoint.as_ref() else {
            self.recovery_required = true;
            return Err(HookSpoolError::RecoveryRequired);
        };
        match write_transition(&self.root, checkpoint) {
            Ok(revision) if revision.length == self.physical_len => {
                self.observed_records_revision = Some(revision);
            }
            Ok(_) => {
                self.recovery_required = true;
                return Err(HookSpoolError::MetadataCorrupted);
            }
            Err(error) => {
                self.recovery_required = true;
                return Err(error);
            }
        }
        #[cfg(feature = "hotpath")]
        {
            hotpath::gauge!("hooks.spool.append.frame_bytes").set(frame_len);
            hotpath::gauge!("hooks.spool.pending.frame_count").set(self.pending.len());
            hotpath::gauge!("hooks.spool.pending.bytes").set(self.pending_bytes());
        }
        Ok(record)
    }

    /// Return up to four fair session batches. FIFO is preserved inside each
    /// session; a session with an in-flight claim is skipped until released.
    #[hotpath::measure(label = "hooks.spool.claim_replay")]
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
            let indices = batch_for_session(&self.pending, session, now)?;
            if indices.is_empty() {
                continue;
            }
            let byte_count = indices.iter().try_fold(0u32, |bytes, index| {
                bytes
                    .checked_add(self.pending[*index].framed_len)
                    .ok_or(HookSpoolError::ReplayBatchExceeded)
            })?;
            let claim_id = next_token();
            selected.push((session, claim_id, indices, byte_count));
        }
        let indices = selected
            .iter()
            .flat_map(|(_, _, indices, _)| indices.iter().copied())
            .collect::<Vec<_>>();
        let mut hydrated = self.hydrate_many(&indices)?.into_iter();
        if let Some((last_session, _, _, _)) = selected.last()
            && self.round_robin_after != Some(*last_session)
        {
            write_replay_cursor(&self.root, *last_session)?;
            self.round_robin_after = Some(*last_session);
        }
        let mut batches = Vec::with_capacity(selected.len());
        for (session, claim_id, indices, byte_count) in selected {
            let records = (0..indices.len())
                .map(|_| hydrated.next().ok_or(HookSpoolError::MetadataCorrupted))
                .collect::<Result<Vec<_>, _>>()?;
            self.replay_claims.insert(session, claim_id);
            batches.push(HookReplayBatchV1 {
                claim_id,
                protected_session_id: session,
                records,
                byte_count,
            });
        }
        #[cfg(feature = "hotpath")]
        {
            let frame_count = batches
                .iter()
                .map(|batch| batch.records.len())
                .sum::<usize>();
            let frame_bytes = batches
                .iter()
                .map(|batch| u64::from(batch.byte_count))
                .sum::<u64>();
            let queue_wait_micros = batches
                .iter()
                .flat_map(|batch| batch.records.iter())
                .map(|record| now.0.saturating_sub(record.queued_at.0))
                .max()
                .unwrap_or(0);
            hotpath::gauge!("hooks.spool.replay.batch_count").set(batches.len());
            hotpath::gauge!("hooks.spool.replay.frame_count").set(frame_count);
            hotpath::gauge!("hooks.spool.replay.frame_bytes").set(frame_bytes);
            hotpath::gauge!("hooks.spool.queue_wait_micros").set(queue_wait_micros);
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
    pub fn expired_records(
        &mut self,
        now: UtcMicros,
    ) -> Result<Vec<HookSpoolRecordV1>, HookSpoolError> {
        let indices = self
            .pending
            .iter()
            .enumerate()
            .filter(|(_, record)| is_expired(record, now))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let expired = self.hydrate_many(&indices)?;
        hotpath::gauge!("hooks.spool.expired.frame_count").set(expired.len());
        Ok(expired)
    }

    /// Persist one daemon acknowledgement and compact logically deleted
    /// frames. Out-of-order session acknowledgements are supported so fair
    /// replay never waits behind another session's transient saturation.
    #[hotpath::measure(label = "hooks.spool.acknowledge")]
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
        #[cfg(feature = "hotpath")]
        {
            // A tombstone is a delivery that expired or was refused, not a
            // success; the disposition mix keeps those failures visible.
            hotpath::gauge!(match acknowledgement.disposition {
                HookSpoolAckDispositionV1::Committed => "hooks.spool.ack.committed",
                HookSpoolAckDispositionV1::TerminalTombstone => "hooks.spool.ack.tombstoned",
            })
            .inc(1);
            hotpath::gauge!("hooks.spool.ack.frame_bytes").set(u64::from(removed.framed_len));
            hotpath::gauge!("hooks.spool.queue_wait_micros")
                .set(now.0.saturating_sub(removed.queued_at.0));
            hotpath::gauge!("hooks.spool.pending.frame_count").set(self.pending.len());
            hotpath::gauge!("hooks.spool.pending.bytes").set(self.pending_bytes());
        }
        // Compaction rewrites every remaining frame, so draining N records
        // must not rewrite the file once per acknowledgement (O(N^2) bytes).
        // Reclaim only when acknowledged frames occupy at least as much of
        // the file as the live ones; a fully drained spool always compacts
        // to zero, and append reclaims on demand when the byte cap nears.
        if self.physical_len > self.pending_bytes().saturating_mul(2) {
            self.compact_pending()?;
        }
        Ok(true)
    }

    fn hydrate(&mut self, index: usize) -> Result<HookSpoolRecordV1, HookSpoolError> {
        self.hydrate_many(&[index])?
            .into_iter()
            .next()
            .ok_or(HookSpoolError::MetadataCorrupted)
    }

    fn hydrate_many(
        &mut self,
        indices: &[usize],
    ) -> Result<Vec<HookSpoolRecordV1>, HookSpoolError> {
        let entries = indices
            .iter()
            .map(|index| {
                self.pending
                    .get(*index)
                    .cloned()
                    .map(|entry| (*index, entry))
                    .ok_or(HookSpoolError::MetadataCorrupted)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(records) = entries
            .iter()
            .map(|(_, entry)| entry.to_record())
            .collect::<Option<Vec<_>>>()
        {
            return Ok(records);
        }
        let revision = match records_file_revision(&self.root) {
            Ok(revision) => revision,
            Err(error) => {
                self.recovery_required = true;
                return Err(error);
            }
        };
        if revision != self.observed_records_revision {
            self.recovery_required = true;
            return Err(HookSpoolError::MetadataCorrupted);
        }
        let file = match File::open(records_path(&self.root)) {
            Ok(file) => file,
            Err(_) => {
                self.recovery_required = true;
                return Err(HookSpoolError::Io);
            }
        };
        let mut records = Vec::with_capacity(entries.len());
        for (index, entry) in entries {
            if let Some(record) = entry.to_record() {
                records.push(record);
                continue;
            }
            let frame = match read_frame_at(&file, entry.file_offset, entry.framed_len) {
                Ok(frame) => frame,
                Err(error) => {
                    self.recovery_required = true;
                    return Err(error);
                }
            };
            let record = match decode_complete_frame(&frame, entry.file_offset, self.config.host) {
                Ok(record) if entry.matches_record(&record) => record,
                Ok(_) => return self.fail_checkpoint_mismatch(),
                Err(_) => return self.fail_corrupted(entry.file_offset),
            };
            let pending = self
                .pending
                .get_mut(index)
                .ok_or(HookSpoolError::MetadataCorrupted)?;
            pending.envelope = Some(record.envelope.clone());
            records.push(record);
        }
        Ok(records)
    }

    fn fail_corrupted<T>(&mut self, at_offset: u64) -> Result<T, HookSpoolError> {
        self.meta.integrity = SpoolIntegrityV1::Corrupted { at_offset };
        if let Err(error) = write_meta(&self.root, &self.meta) {
            self.recovery_required = true;
            return Err(error);
        }
        Err(HookSpoolError::Corrupted { at_offset })
    }

    fn fail_checkpoint_mismatch<T>(&mut self) -> Result<T, HookSpoolError> {
        self.recovery_required = true;
        if remove_spool_member(&checkpoint_path(&self.root)).is_err() {
            return Err(HookSpoolError::Io);
        }
        self.checkpoint = None;
        Err(HookSpoolError::MetadataCorrupted)
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

    fn note_pending(
        &mut self,
        record: &HookSpoolRecordV1,
        file_offset: u64,
    ) -> Result<(), HookSpoolError> {
        let entry = self
            .pending_by_session
            .entry(record.protected_session_id)
            .or_default();
        entry.0 = entry.0.checked_add(1).ok_or(HookSpoolError::SpoolFull)?;
        entry.1 = entry.1.saturating_add(u64::from(record.framed_len));
        self.pending
            .push(PendingRecordV1::from_record(record, file_offset));
        Ok(())
    }

    fn release_usage(&mut self, record: &PendingRecordV1) {
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
        self.pending_by_session
            .values()
            .map(|(_, bytes)| *bytes)
            .sum()
    }

    #[hotpath::measure(label = "hooks.spool.compact")]
    fn compact_pending(&mut self) -> Result<(), HookSpoolError> {
        self.ensure_healthy()?;
        if records_file_revision(&self.root)? != self.observed_records_revision {
            self.recovery_required = true;
            return Err(HookSpoolError::MetadataCorrupted);
        }
        let maximum =
            usize::try_from(self.config.limits.max_host_bytes).map_err(|_| HookSpoolError::Io)?;
        let source = match read_bounded(&records_path(&self.root), maximum)? {
            Some(source) => source,
            None if self.pending.is_empty() => Vec::new(),
            None => return self.fail_corrupted(0),
        };
        let mut bytes = Vec::with_capacity(self.pending_bytes() as usize);
        let mut offset = 0u64;
        let mut rebuilt = Vec::with_capacity(self.pending.len());
        for entry in self.pending.clone() {
            let start = usize::try_from(entry.file_offset)
                .map_err(|_| HookSpoolError::MetadataCorrupted)?;
            let end = start
                .checked_add(
                    usize::try_from(entry.framed_len)
                        .map_err(|_| HookSpoolError::MetadataCorrupted)?,
                )
                .ok_or(HookSpoolError::MetadataCorrupted)?;
            let Some(frame) = source.get(start..end) else {
                return self.fail_corrupted(entry.file_offset);
            };
            let record = match decode_complete_frame(frame, entry.file_offset, self.config.host) {
                Ok(record) if entry.matches_record(&record) => record,
                Ok(_) => return self.fail_checkpoint_mismatch(),
                Err(_) => return self.fail_corrupted(entry.file_offset),
            };
            let rebuilt_entry = PendingRecordV1::from_record(&record, offset);
            offset = offset.saturating_add(u64::from(entry.framed_len));
            bytes.extend_from_slice(frame);
            rebuilt.push(rebuilt_entry);
        }
        hotpath::measure_block!("hooks.spool.fsync.compact", {
            shared_atomic_write(
                &records_path(&self.root),
                "records",
                &bytes,
                DIRECTORY_POLICY,
            )
            .map_err(|_| HookSpoolError::Io)
        })?;
        let checkpoint = match write_checkpoint(&self.root, self.config, &rebuilt) {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                self.recovery_required = true;
                return Err(error);
            }
        };
        self.pending = rebuilt;
        self.observed_records_revision = checkpoint.records_revision.clone();
        self.checkpoint = Some(checkpoint);
        self.physical_len = offset;
        hotpath::gauge!("hooks.spool.compact.frame_count").set(self.pending.len());
        hotpath::gauge!("hooks.spool.compact.bytes").set(self.physical_len);
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

fn checkpoint_path(root: &Path) -> PathBuf {
    root.join(CHECKPOINT_FILE)
}

fn transition_path(root: &Path) -> PathBuf {
    root.join(TRANSITION_FILE)
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
        Ok(_) => {
            // An existing root must already be private to the current owner:
            // a group/world-writable or foreign-owned directory lets another
            // local account replace spool members despite their per-file
            // modes. Transient metadata failures stay Io rather than
            // condemning the path.
            return tracedecay_private_fs::validate_private_directory(root).map_err(|error| {
                match error.kind() {
                    io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput => {
                        HookSpoolError::UnsafePath
                    }
                    _ => HookSpoolError::Io,
                }
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(HookSpoolError::Io),
    }
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent).map_err(|_| HookSpoolError::Io)?;
    }
    match tracedecay_private_fs::create_private_directory(root) {
        Ok(()) => {}
        // A concurrent opener may win the creation race; the directory is
        // acceptable only if it is private.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            tracedecay_private_fs::validate_private_directory(root).map_err(|error| match error
                .kind()
            {
                io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput => {
                    HookSpoolError::UnsafePath
                }
                _ => HookSpoolError::Io,
            })?;
        }
        Err(_) => return Err(HookSpoolError::Io),
    }
    hotpath::measure_block!("hooks.spool.fsync.directory", {
        shared_sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)
    })
}

fn validate_regular_or_missing(path: &Path) -> Result<bool, HookSpoolError> {
    shared_validate_regular(path).map_err(|_| HookSpoolError::UnsafePath)
}

fn remove_spool_member(path: &Path) -> Result<(), HookSpoolError> {
    if !validate_regular_or_missing(path)? {
        return Ok(());
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(HookSpoolError::Io),
    }
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
    hotpath::measure_block!("hooks.spool.fsync.cursor", {
        shared_atomic_write(
            &replay_cursor_path(root),
            "replay-cursor",
            &cursor,
            DIRECTORY_POLICY,
        )
        .map_err(|_| HookSpoolError::Io)
    })
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
