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
use std::time::Duration;

#[cfg(test)]
use tracedecay_domain::canonical_json_bytes;
use tracedecay_domain::{
    UtcMicros,
    framed_log::{self, checksum as frame_checksum},
};
use tracedecay_private_fs::FileLease;
use tracedecay_private_fs::framed_log::{
    DirectorySyncPolicy, StagedReplacement, atomic_write_accelerator,
    read_bounded as shared_read_bounded, sync_directory as shared_sync_directory,
    validate_regular_or_missing as shared_validate_regular,
};

use crate::{
    HOOK_SYNCHRONOUS_BUDGET, HookContractError, HookEventEnvelopeV2, HookScopeBindingV1,
    MAX_HOOK_PAYLOAD_BYTES, MAX_REPLAY_BATCH_BYTES, MAX_REPLAY_BATCH_RECORDS, MAX_SPOOL_AGE_MICROS,
    NativeContextScoutLifecycleV1,
};

mod checkpoint;
mod commit;
mod frame;
mod lease;
mod meta;
mod replay;
mod types;

#[cfg(test)]
use checkpoint::{CHECKPOINT_ENTRY_BYTES, CHECKPOINT_HEADER_BYTES, CHECKPOINT_MAGIC};
use checkpoint::{
    CHECKPOINT_REWRITE_BYTE_THRESHOLD, CHECKPOINT_REWRITE_FRAME_THRESHOLD, CheckpointAnchorV1,
    RecordsFileRevisionV1, RecordsPrefixDigestV1, read_checkpoint, read_frame_at, read_transition,
    records_file_revision, write_checkpoint, write_transition,
};
use types::{AcknowledgedSequenceV1, HookSpoolMetaV1, PendingRecordV1, SpoolIntegrityV1};
pub use types::{
    HookReplayBatchV1, HookSpoolAckDispositionV1, HookSpoolAckV1, HookSpoolConfigV1,
    HookSpoolError, HookSpoolLimitsV1, HookSpoolOpenReportV1, HookSpoolRecordV1,
    HookSpoolResetReasonV1, HookSpoolWriterLeaseV1,
};

use commit::{COMMIT_FILE, CommitLockV1, commit_records, forget_synced_extent};
use frame::{
    append_frame, decode_complete_frame, encode_frame, encode_spool_payload, scan_records,
    scan_records_from, truncate_records,
};
use lease::{acquire_lease, acquire_lease_bounded};
use meta::{
    acknowledged_map, advance_next_sequence, encode_meta, normalize_acknowledgements, read_meta,
    validate_meta, validate_meta_against_records, write_meta,
};
use replay::{
    batch_for_session, is_expired, replayable_sessions, round_robin_after, usage_by_session,
};

const SPOOL_MAGIC: &[u8; 4] = b"TDH2";
const SPOOL_FORMAT_VERSION: u16 = 1;
const SPOOL_META_VERSION: u16 = 1;
// Member filenames retain the spool layout generation; this header version owns the body shape.
const CHECKPOINT_FORMAT_VERSION: u16 = 3;
const FRAME_LENGTH_BYTES: usize = 4;
const FRAME_HEADER_BYTES: usize = 4 + 2 + 8 + 8 + 32 + 4;
const FRAME_CHECKSUM_BYTES: usize = framed_log::CHECKSUM_BYTES;
const CONTROL_RECORD_RESERVE: u32 = 1;
const CONTROL_FRAME_RESERVE_BYTES: u64 = 4 * 1024;
// Acknowledgements can arrive out of global sequence order because replay is
// fair across sessions. Reserve room for one bounded marker per live record.
const MAX_META_BYTES: usize = 1024 * 1024;
const MAX_REPLAY_SESSIONS: usize = 4;
const RECORDS_FILE: &str = "records.v1.bin";
/// The spool file an append writes in place. Compaction publishes its
/// replacement by rename, so an in-place data write to this file is a record
/// landing (or the rare torn-tail repair).
pub const HOOK_SPOOL_RECORDS_FILE: &str = RECORDS_FILE;
const META_FILE: &str = "meta.v1.json";
const CHECKPOINT_FILE: &str = "checkpoint.v1.bin";
const TRANSITION_FILE: &str = "checkpoint-transition.v1.json";
const LEASE_FILE: &str = "writer.v1.lease";
const REPLAY_CURSOR_FILE: &str = "replay-cursor.v1.bin";
const DIRECTORY_POLICY: DirectorySyncPolicy = DirectorySyncPolicy::Strict;
// Committers queue behind at most one batch's sync, the same bounded wait a
// hook spends on writer admission.
const COMMIT_WAIT: Duration = HOOK_SYNCHRONOUS_BUDGET;

/// A host-local transport spool. It owns a short writer lease, performs no
/// query/model/database work, and has no authority to rebind an event.
///
/// No durability barrier runs under the writer lease in steady state: appends
/// write unsynced frames, the group commit syncs them after the lease is
/// released, and settlement or reclaim stages its synced replacement with the
/// lease released and holds it only to publish by rename. Only recovery of a
/// torn or corrupt spool, its first metadata write, and an explicit reset
/// sync under the lease.
#[derive(Debug)]
pub struct HookSpoolV1 {
    root: PathBuf,
    config: HookSpoolConfigV1,
    lease: HookSpoolWriterLeaseV1,
    /// `None` only while [`Self::without_lease`] runs a barrier.
    lease_file: Option<FileLease>,
    meta: HookSpoolMetaV1,
    checkpoint: Option<CheckpointAnchorV1>,
    observed_records_revision: Option<RecordsFileRevisionV1>,
    /// Digest of `[0, physical_len)` as this handle read or wrote it. The
    /// revision cannot see a same-length in-place write within one timestamp
    /// tick, so an append re-hashes the file against this before attesting.
    records_prefix: RecordsPrefixDigestV1,
    pending: Vec<PendingRecordV1>,
    pending_by_session: BTreeMap<[u8; 32], (u32, u64)>,
    physical_len: u64,
    round_robin_after: Option<[u8; 32]>,
    replay_claims: BTreeMap<[u8; 32], [u8; 16]>,
    recovery_required: bool,
    /// Frames this handle wrote or deduplicated against, not yet committed.
    uncommitted: Option<UncommittedExtentV1>,
    /// Held for its `Drop` only: closes the writer-lease hold observation.
    _lease_hold: Option<SpoolLeaseHoldObservationV1>,
}

static SPOOL_LEASES_HELD: AtomicU64 = AtomicU64::new(0);

/// No metrics recorder is installed outside profiling sessions, so spool
/// gauges that cost an atomic, a clock read, or a walk run only under TRACE.
#[inline(always)]
fn observing() -> bool {
    tracing::level_enabled!(tracing::Level::TRACE)
}

/// Writer-lease hold observation. Acquisition wait is the
/// `hooks.spool.acquire_lease` span; this records how long the sole writer
/// lease is then *held* (open handle lifetime), which is what other writers
/// contend against. Drop-based so panic or early return cannot leak the gauge.
#[derive(Debug)]
struct SpoolLeaseHoldObservationV1 {
    acquired: std::time::Instant,
}

impl SpoolLeaseHoldObservationV1 {
    fn enter() -> Option<Self> {
        if !observing() {
            return None;
        }
        let held = SPOOL_LEASES_HELD
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        metrics::gauge!("hooks.spool.lease.held").set((held) as f64);
        Some(Self {
            acquired: std::time::Instant::now(),
        })
    }
}

impl Drop for SpoolLeaseHoldObservationV1 {
    fn drop(&mut self) {
        let _ = SPOOL_LEASES_HELD.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
            held.checked_sub(1)
        });
        metrics::gauge!("hooks.spool.lease.held")
            .set((SPOOL_LEASES_HELD.load(Ordering::Relaxed)) as f64);
        metrics::gauge!("hooks.spool.lease.hold_micros")
            .set((u64::try_from(self.acquired.elapsed().as_micros()).unwrap_or(u64::MAX)) as f64);
    }
}

/// The prefix of one records file a handle must make durable before its
/// caller acknowledges what it appended.
#[derive(Clone, Copy, Debug)]
struct UncommittedExtentV1 {
    identity: [u8; 32],
    end: u64,
}

impl HookSpoolV1 {
    /// Cheap conservative replay probe that never acquires the writer lease.
    ///
    /// `false` means the records file is absent or empty. `true` does not claim
    /// that every physical record is still pending; opening under the writer
    /// lease remains the authority for acknowledgement and recovery state.
    pub fn has_records(root: &Path) -> Result<bool, HookSpoolError> {
        let path = records_path(root);
        if !validate_regular_or_missing(&path)? {
            return Ok(false);
        }
        fs::metadata(path)
            .map(|metadata| metadata.len() > 0)
            .map_err(|_| HookSpoolError::Io)
    }

    /// Explicitly recreate one exact host spool without decoding incompatible
    /// metadata, records, or cursors. The normal writer lease still fences a
    /// live adapter, and only the incompatible transport-owned files are
    /// removed.
    #[tracing::instrument(name = "hooks.spool.reset", level = "trace", skip_all)]
    pub fn reset(
        root: impl Into<PathBuf>,
        config: HookSpoolConfigV1,
        now: UtcMicros,
    ) -> Result<(), HookSpoolError> {
        config.validate()?;
        let root = root.into();
        ensure_root(&root)?;
        let (_lease, lease_file) = acquire_lease(&root, config.writer_lease_micros, now)?;
        let commit_lock = CommitLockV1::acquire(&root, COMMIT_WAIT)?;
        for path in [
            records_path(&root),
            meta_path(&root),
            checkpoint_path(&root),
            transition_path(&root),
            replay_cursor_path(&root),
        ] {
            remove_spool_member(&path)?;
        }
        commit_lock.invalidate()?;
        {
            let _span = tracing::trace_span!("hooks.spool.fsync.directory").entered();
            shared_sync_directory(&root, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)
        }?;
        lease_file.release().map_err(|_| HookSpoolError::Io)?;
        Ok(())
    }

    /// Retire one exact host spool whose records another durable owner has
    /// adopted: reset it under the writer lease, then remove its lock members
    /// and the emptied root.
    pub fn remove(
        root: impl Into<PathBuf>,
        config: HookSpoolConfigV1,
        now: UtcMicros,
    ) -> Result<(), HookSpoolError> {
        let root = root.into();
        Self::reset(&root, config, now)?;
        for member in [LEASE_FILE, COMMIT_FILE] {
            remove_spool_member(&root.join(member))?;
        }
        fs::remove_dir(&root).map_err(|_| HookSpoolError::Io)?;
        let Some(parent) = root.parent() else {
            return Err(HookSpoolError::UnsafePath);
        };
        shared_sync_directory(parent, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)
    }

    /// Open/recover a bounded spool and acquire the sole writer lease. The OS
    /// releases the prior process lock when its file descriptor closes. Expiry
    /// independently prevents a live-but-stale owner from mutating the spool.
    ///
    /// The lease is single-shot and non-renewable: open, do bounded work with
    /// the `now` the lease was acquired at, drop. A handle held past
    /// `config.writer_lease_micros` of caller-observed time stops accepting
    /// mutations with [`HookSpoolError::WriterLeaseLost`]; the only recovery is
    /// to drop it and reopen, which is lossless because every acknowledgement
    /// is durable before its call returns and appended records stay in the
    /// records file for the next committer.
    #[tracing::instrument(name = "hooks.spool.open", level = "trace", skip_all)]
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

    /// Wait only for writer admission, for at most `wait_budget` measured from
    /// the lock attempt (after the spool root and lease file exist). Once
    /// admitted, recovery and append retain their existing durable semantics.
    #[tracing::instrument(name = "hooks.spool.open_within", level = "trace", skip_all)]
    pub fn open_within(
        root: impl Into<PathBuf>,
        config: HookSpoolConfigV1,
        now: UtcMicros,
        wait_budget: std::time::Duration,
    ) -> Result<(Self, HookSpoolOpenReportV1), HookSpoolError> {
        config.validate()?;
        let root = root.into();
        ensure_root(&root)?;
        let (lease, lease_file) =
            acquire_lease_bounded(&root, config.writer_lease_micros, now, Some(wait_budget))?;
        Self::open_after_lease(root, config, lease, lease_file, now)
    }

    #[tracing::instrument(name = "hooks.spool.open_after_lease", level = "trace", skip_all)]
    fn open_after_lease(
        root: PathBuf,
        config: HookSpoolConfigV1,
        lease: HookSpoolWriterLeaseV1,
        lease_file: FileLease,
        _now: UtcMicros,
    ) -> Result<(Self, HookSpoolOpenReportV1), HookSpoolError> {
        let stored_meta = read_meta(&root)?;
        let meta_was_missing = stored_meta.is_none();
        let mut meta = stored_meta.unwrap_or_else(HookSpoolMetaV1::fresh);
        validate_meta(&meta, config.limits)?;
        let current_revision = records_file_revision(&root)?;
        let cached_checkpoint = read_checkpoint(&root, config)?;
        let checkpoint_bytes = cached_checkpoint
            .as_ref()
            .map_or(0, |checkpoint| checkpoint.bytes);
        let mut checkpoint_records = 0u32;
        let revision_trusted = match cached_checkpoint {
            Some(checkpoint) if checkpoint.records_revision == current_revision => Some(checkpoint),
            Some(checkpoint) => {
                let transition = read_transition(&root)?;
                let extends_checkpoint = transition.as_ref().is_some_and(|transition| {
                    transition.checkpoint_checksum == checkpoint.checksum
                        && transition.checkpoint_revision == checkpoint.records_revision
                        && Some(&transition.current_revision) == current_revision.as_ref()
                        && transition.current_revision.length >= checkpoint.covered_end()
                });
                extends_checkpoint.then_some(checkpoint)
            }
            None => None,
        };
        // The revision names the file, not its bytes: only the covered
        // digest proves the indexed prefix is still what the checkpoint saw.
        let mut records_prefix = RecordsPrefixDigestV1::empty();
        let content_trusted = match revision_trusted {
            Some(checkpoint)
                if records_prefix.read_through(&root, checkpoint.covered_end())?
                    && records_prefix.checksum() == checkpoint.covered_checksum =>
            {
                Some(checkpoint)
            }
            Some(_) => {
                records_prefix = RecordsPrefixDigestV1::empty();
                None
            }
            None => None,
        };
        let (mut scan, reusable_checkpoint) = match content_trusted {
            Some(checkpoint) => {
                checkpoint_records = u32::try_from(checkpoint.records.len())
                    .map_err(|_| HookSpoolError::MetadataCorrupted)?;
                let anchor = CheckpointAnchorV1 {
                    records_revision: checkpoint.records_revision.clone(),
                    checksum: checkpoint.checksum,
                };
                let covered_end = checkpoint.covered_end();
                (
                    scan_records_from(&root, config, checkpoint.records, covered_end)?,
                    Some(anchor),
                )
            }
            None => (scan_records(&root, config)?, None),
        };
        let mut truncated_partial_tail_bytes = 0;

        if let Some(offset) = scan.corruption {
            meta.integrity = SpoolIntegrityV1::Corrupted { at_offset: offset };
            write_meta(&root, &meta)?;
        } else if scan.partial_tail.is_some() {
            // A writer died mid-append. Its caller never committed, so no
            // acknowledgement covers the torn frame, unless a published sync
            // already reached past its start: then durable bytes were lost.
            let commit_lock = CommitLockV1::acquire(&root, COMMIT_WAIT)?;
            let synced_through = match current_revision.as_ref() {
                Some(revision) => commit_lock.synced_through(revision.identity)?,
                None => None,
            };
            if synced_through.is_some_and(|synced| synced > scan.valid_end) {
                meta.integrity = SpoolIntegrityV1::Corrupted {
                    at_offset: scan.valid_end,
                };
                write_meta(&root, &meta)?;
            } else {
                truncate_records(&root, scan.valid_end)?;
                commit_lock.invalidate()?;
                truncated_partial_tail_bytes = scan.physical_len.saturating_sub(scan.valid_end);
                scan.physical_len = scan.valid_end;
                scan.partial_tail = None;
            }
        }
        if !records_prefix.read_through(&root, scan.physical_len)? {
            return Err(HookSpoolError::Io);
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
            // Appends never write metadata, so frames after its last write
            // (possibly already inside a rewritten checkpoint) carry the
            // sequence forward.
            advance_next_sequence(&mut meta, &scan.records)?;
            validate_meta_against_records(
                &meta,
                scan.records.iter().map(|record| record.sequence),
                config.limits,
            )?;
            if meta_was_missing {
                write_meta_after_records(&root, &meta)?;
            }
            Some(match (reusable_checkpoint, rewrite_checkpoint) {
                (Some(checkpoint), false) => checkpoint,
                _ => {
                    checkpoint_rewritten = true;
                    write_checkpoint(&root, config, &scan.records, &records_prefix)?
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
        let spool = Self {
            root,
            config,
            lease,
            lease_file: Some(lease_file),
            meta,
            checkpoint,
            observed_records_revision,
            records_prefix,
            pending,
            pending_by_session,
            physical_len: scan.physical_len,
            round_robin_after,
            replay_claims: BTreeMap::new(),
            recovery_required: false,
            uncommitted: None,

            _lease_hold: SpoolLeaseHoldObservationV1::enter(),
        };
        metrics::gauge!("hooks.spool.pending.frame_count").set(report.pending_records);
        metrics::gauge!("hooks.spool.pending.bytes").set((report.pending_bytes) as f64);
        Ok((spool, report))
    }

    pub fn lease(&self) -> HookSpoolWriterLeaseV1 {
        self.lease
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> HookSpoolConfigV1 {
        self.config
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

    /// Append one validated envelope under the writer lease. An exact pending
    /// `event_id` duplicate returns its existing record; reusing that ID for a
    /// different envelope is rejected. Only the frame and the rebuildable
    /// checkpoint transition are written here: the record, new or duplicate,
    /// is durable and may be acknowledged only once [`Self::commit`] returns.
    #[tracing::instrument(name = "hooks.spool.append", level = "trace", skip_all)]
    pub fn append(
        &mut self,
        envelope: HookEventEnvelopeV2,
        binding: &HookScopeBindingV1,
        now: UtcMicros,
    ) -> Result<HookSpoolRecordV1, HookSpoolError> {
        self.append_with_native_lifecycle(envelope, None, binding, now)
    }

    #[tracing::instrument(name = "hooks.spool.append_with_lifecycle", level = "trace", skip_all)]
    pub fn append_with_native_lifecycle(
        &mut self,
        envelope: HookEventEnvelopeV2,
        native_lifecycle: Option<NativeContextScoutLifecycleV1>,
        binding: &HookScopeBindingV1,
        now: UtcMicros,
    ) -> Result<HookSpoolRecordV1, HookSpoolError> {
        self.append_record(envelope, native_lifecycle, binding, now, true)
    }

    fn append_record(
        &mut self,
        envelope: HookEventEnvelopeV2,
        native_lifecycle: Option<NativeContextScoutLifecycleV1>,
        binding: &HookScopeBindingV1,
        now: UtcMicros,
        may_reclaim: bool,
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
        let encoded = encode_spool_payload(&envelope, native_lifecycle.as_ref())?;
        if encoded.is_empty() || encoded.len() > MAX_HOOK_PAYLOAD_BYTES {
            return Err(HookSpoolError::RecordTooLarge);
        }
        if let Some(index) = self
            .pending
            .iter()
            .position(|record| record.event_id == envelope.event_id)
        {
            let existing = self.hydrate(index)?;
            return if existing.envelope == envelope && existing.native_lifecycle == native_lifecycle
            {
                // Another writer may have appended it without committing yet.
                let end = self.pending[index]
                    .file_offset
                    .saturating_add(u64::from(self.pending[index].framed_len));
                self.note_uncommitted(end)?;
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
            if !may_reclaim || self.physical_len <= self.pending_bytes() {
                return Err(HookSpoolError::SpoolFull);
            }
            // Reclaiming reloads the spool, so the append restarts against it.
            self.reclaim(now)?;
            return self.append_record(envelope, native_lifecycle, binding, now, false);
        }
        if records_file_revision(&self.root)? != self.observed_records_revision
            || !self.records_prefix.matches_file(&self.root)?
        {
            self.recovery_required = true;
            return Err(HookSpoolError::MetadataCorrupted);
        }

        if let Err(error) = append_frame(&records_path(&self.root), &frame) {
            self.recovery_required = true;
            return Err(error);
        }
        self.records_prefix.extend(&frame);
        let record = decode_complete_frame(&frame, self.physical_len, self.config.host)?;
        // Reopen carries the sequence forward from the frame itself, so the
        // next metadata write persists it; an append writes no metadata.
        self.meta.next_sequence = sequence
            .checked_add(1)
            .ok_or(HookSpoolError::MetadataCorrupted)?;
        self.physical_len = self.physical_len.saturating_add(frame_len);
        self.note_pending(&record, self.physical_len.saturating_sub(frame_len))?;
        let Some(checkpoint) = self.checkpoint.as_ref() else {
            self.recovery_required = true;
            return Err(HookSpoolError::RecoveryRequired);
        };
        match write_transition(&self.root, checkpoint) {
            Ok(revision) if revision.length == self.physical_len => {
                self.observed_records_revision = Some(revision);
                self.note_uncommitted(self.physical_len)?;
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

        if observing() {
            metrics::gauge!("hooks.spool.append.frame_bytes").set((frame_len) as f64);
            metrics::gauge!("hooks.spool.pending.frame_count").set((self.pending.len()) as f64);
            metrics::gauge!("hooks.spool.pending.bytes").set((self.pending_bytes()) as f64);
        }
        Ok(record)
    }

    /// Release the writer lease, then make every frame this handle appended or
    /// deduplicated against durable. Concurrent committers share one sync of
    /// the records file, so the lease covers only writing frames; a caller
    /// acknowledges its records only after this returns.
    #[tracing::instrument(name = "hooks.spool.commit", level = "trace", skip_all)]
    pub fn commit(self) -> Result<(), HookSpoolError> {
        let root = self.root.clone();
        let uncommitted = self.uncommitted;
        drop(self);
        match uncommitted {
            Some(extent) => commit_records(&root, extent.identity, extent.end, COMMIT_WAIT),
            None => Ok(()),
        }
    }

    fn note_uncommitted(&mut self, end: u64) -> Result<(), HookSpoolError> {
        let identity = self
            .observed_records_revision
            .as_ref()
            .ok_or(HookSpoolError::MetadataCorrupted)?
            .identity;
        let end = match self.uncommitted {
            Some(extent) if extent.identity == identity => extent.end.max(end),
            _ => end,
        };
        self.uncommitted = Some(UncommittedExtentV1 { identity, end });
        Ok(())
    }

    /// Return up to four fair session batches. FIFO is preserved inside each
    /// session; a session with an in-flight claim is skipped until released.
    #[tracing::instrument(name = "hooks.spool.claim_replay", level = "trace", skip_all)]
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

        if observing() {
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
            metrics::gauge!("hooks.spool.replay.batch_count").set((batches.len()) as f64);
            metrics::gauge!("hooks.spool.replay.frame_count").set((frame_count) as f64);
            metrics::gauge!("hooks.spool.replay.frame_bytes").set((frame_bytes) as f64);
            metrics::gauge!("hooks.spool.queue_wait_micros").set((queue_wait_micros) as f64);
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

    /// Every pending record in append order, for adopting this spool's
    /// contents into another durable owner before [`Self::remove`].
    pub fn pending_records(&mut self) -> Result<Vec<HookSpoolRecordV1>, HookSpoolError> {
        self.ensure_healthy()?;
        let indices = (0..self.pending.len()).collect::<Vec<_>>();
        self.hydrate_many(&indices)
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
        metrics::gauge!("hooks.spool.expired.frame_count").set((expired.len()) as f64);
        Ok(expired)
    }

    /// Persist one daemon acknowledgement and compact logically deleted
    /// frames. Out-of-order session acknowledgements are supported so fair
    /// replay never waits behind another session's transient saturation.
    #[tracing::instrument(name = "hooks.spool.acknowledge", level = "trace", skip_all)]
    pub fn acknowledge(
        &mut self,
        acknowledgement: HookSpoolAckV1,
        now: UtcMicros,
    ) -> Result<bool, HookSpoolError> {
        self.acknowledge_many(&[acknowledgement], now)?
            .pop()
            .ok_or(HookSpoolError::AckConflict)?
    }

    /// Persist a replay pass's acknowledgements with one metadata publication
    /// and at most one reclaim.
    ///
    /// Live hook appends wait on the writer lease within their synchronous
    /// budget, so the lease is released across every durability barrier of
    /// the settlement and held only to validate and publish by rename.
    /// Each acknowledgement is validated on its own: a conflicting one is
    /// reported in its slot and leaves the others to persist. Only a failed
    /// publication fails the call, and then nothing was acknowledged.
    #[tracing::instrument(name = "hooks.spool.acknowledge_many", level = "trace", skip_all)]
    pub fn acknowledge_many(
        &mut self,
        acknowledgements: &[HookSpoolAckV1],
        now: UtcMicros,
    ) -> Result<Vec<Result<bool, HookSpoolError>>, HookSpoolError> {
        self.ensure_writable(now)?;
        let existing = acknowledged_map(&self.meta)?;
        let mut next_meta = self.meta.clone();
        let mut acknowledged_indices = Vec::new();
        let outcomes = acknowledgements
            .iter()
            .map(|acknowledgement| {
                if acknowledgement.sequence == 0 || acknowledgement.receipt_id == [0; 16] {
                    return Err(HookSpoolError::AckConflict);
                }
                if acknowledgement.sequence <= self.meta.committed_through {
                    return Ok(false);
                }
                let prior = existing.get(&acknowledgement.sequence).or_else(|| {
                    next_meta.acknowledged[self.meta.acknowledged.len()..]
                        .iter()
                        .find(|prior| prior.sequence == acknowledgement.sequence)
                });
                if let Some(prior) = prior {
                    return if prior.receipt_id == acknowledgement.receipt_id
                        && prior.disposition == acknowledgement.disposition
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
                next_meta.acknowledged.push(AcknowledgedSequenceV1 {
                    sequence: acknowledgement.sequence,
                    receipt_id: acknowledgement.receipt_id,
                    disposition: acknowledgement.disposition,
                });
                acknowledged_indices.push((index, acknowledgement.disposition));
                Ok(true)
            })
            .collect::<Vec<_>>();
        if acknowledged_indices.is_empty() {
            return Ok(outcomes);
        }
        normalize_acknowledgements(&mut next_meta)?;

        let settled = observing().then(|| {
            acknowledged_indices
                .iter()
                .map(|(index, disposition)| {
                    let record = &self.pending[*index];
                    (record.framed_len, record.queued_at, *disposition)
                })
                .collect::<Vec<_>>()
        });
        self.publish_meta(&next_meta, now)?;

        if let Some(settled) = settled {
            for (framed_len, queued_at, disposition) in settled {
                // A tombstone is a delivery that expired or was refused, not a
                // success; the disposition mix keeps those failures visible.
                metrics::gauge!(match disposition {
                    HookSpoolAckDispositionV1::Committed => "hooks.spool.ack.committed",
                    HookSpoolAckDispositionV1::TerminalTombstone => "hooks.spool.ack.tombstoned",
                })
                .increment(1);
                metrics::gauge!("hooks.spool.ack.frame_bytes").set((u64::from(framed_len)) as f64);
                metrics::gauge!("hooks.spool.queue_wait_micros")
                    .set((now.0.saturating_sub(queued_at.0)) as f64);
            }
            metrics::gauge!("hooks.spool.pending.frame_count").set((self.pending.len()) as f64);
            metrics::gauge!("hooks.spool.pending.bytes").set((self.pending_bytes()) as f64);
        }
        // Reclaim rewrites every remaining frame, so draining N records must
        // not rewrite the file once per acknowledgement (O(N^2) bytes).
        // Reclaim only when acknowledged frames occupy at least as much of
        // the file as the live ones; a fully drained spool always reclaims
        // to zero, and append reclaims on demand when the byte cap nears.
        if self.physical_len > self.pending_bytes().saturating_mul(2) {
            self.reclaim(now)?;
        }
        Ok(outcomes)
    }

    /// Replaces the metadata with `meta`, computed from the metadata this
    /// handle holds. The replacement and the records it names are made
    /// durable with the lease released; publication fails with
    /// [`HookSpoolError::WriterLeaseLost`] if another writer replaced the
    /// metadata meanwhile. Returns once the publication itself is durable.
    fn publish_meta(
        &mut self,
        meta: &HookSpoolMetaV1,
        now: UtcMicros,
    ) -> Result<(), HookSpoolError> {
        let bytes = encode_meta(meta)?;
        let settled_against = read_bounded(&meta_path(&self.root), MAX_META_BYTES)?;
        let records = self
            .observed_records_revision
            .as_ref()
            .map(|revision| (revision.identity, revision.length));
        let staged = self.without_lease(now, |root| {
            // Metadata may name only sequences the durable records file holds.
            if let Some((identity, end)) = records {
                commit_records(root, identity, end, COMMIT_WAIT)?;
            }
            {
                let _span = tracing::trace_span!("hooks.spool.fsync.meta").entered();
                {
                    StagedReplacement::stage(&meta_path(root), "meta", &bytes)
                        .map_err(|_| HookSpoolError::Io)
                }
            }
        })?;
        if read_bounded(&meta_path(&self.root), MAX_META_BYTES)? != settled_against {
            return Err(HookSpoolError::WriterLeaseLost);
        }
        staged.publish().map_err(|_| HookSpoolError::Io)?;
        self.without_lease(now, |root| {
            let _span = tracing::trace_span!("hooks.spool.fsync.directory").entered();
            shared_sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)
        })
    }

    /// Rewrites the records file down to its pending frames. The replacement
    /// is staged and synced with the lease released and published only if no
    /// writer changed the spool meanwhile; otherwise a later reclaim retries.
    /// A lost rename leaves the old file, which still holds every pending
    /// frame, and the first commit to the replacement makes its name durable.
    #[tracing::instrument(name = "hooks.spool.reclaim", level = "trace", skip_all)]
    fn reclaim(&mut self, now: UtcMicros) -> Result<(), HookSpoolError> {
        let (bytes, rebuilt) = self.pending_frames()?;
        let revision = self.observed_records_revision.clone();
        let sequences = |records: &[PendingRecordV1]| {
            records
                .iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>()
        };
        let reclaimed = sequences(&rebuilt);
        let staged = self.without_lease(now, |root| {
            forget_synced_extent(root, COMMIT_WAIT)?;
            {
                let _span = tracing::trace_span!("hooks.spool.fsync.compact").entered();
                {
                    StagedReplacement::stage(&records_path(root), "records", &bytes)
                        .map_err(|_| HookSpoolError::Io)
                }
            }
        })?;
        if self.observed_records_revision != revision || sequences(&self.pending) != reclaimed {
            metrics::gauge!("hooks.spool.compact.raced").increment(1);
            return Ok(());
        }
        staged.publish().map_err(|_| HookSpoolError::Io)?;
        let records_prefix = RecordsPrefixDigestV1::of(&bytes);
        let checkpoint = match write_checkpoint(&self.root, self.config, &rebuilt, &records_prefix)
        {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                self.recovery_required = true;
                return Err(error);
            }
        };
        self.pending = rebuilt;
        self.observed_records_revision = checkpoint.records_revision.clone();
        self.records_prefix = records_prefix;
        self.checkpoint = Some(checkpoint);
        self.physical_len = u64::try_from(bytes.len()).map_err(|_| HookSpoolError::SpoolFull)?;
        metrics::gauge!("hooks.spool.compact.frame_count").set((self.pending.len()) as f64);
        metrics::gauge!("hooks.spool.compact.bytes").set((self.physical_len) as f64);
        Ok(())
    }

    /// Runs `barrier` with the writer lease released, so no live writer waits
    /// on it, then reacquires the lease and reloads the spool, which writers
    /// may have appended to meanwhile. This handle's own appended frames are
    /// committed first, since the reload forgets them. A handle that could not
    /// get its lease back refuses further mutations.
    fn without_lease<T>(
        &mut self,
        now: UtcMicros,
        barrier: impl FnOnce(&Path) -> Result<T, HookSpoolError>,
    ) -> Result<T, HookSpoolError> {
        self.ensure_writable(now)?;
        let uncommitted = self.uncommitted.take();
        drop(self.lease_file.take());
        self.recovery_required = true;
        let outcome = uncommitted
            .map_or(Ok(()), |extent| {
                commit_records(&self.root, extent.identity, extent.end, COMMIT_WAIT)
            })
            .and_then(|()| barrier(&self.root));
        let (lease, lease_file) = acquire_lease_bounded(
            &self.root,
            self.config.writer_lease_micros,
            now,
            Some(self.config.writer_lease()),
        )?;
        let replay_claims = std::mem::take(&mut self.replay_claims);
        let (reloaded, _) =
            Self::open_after_lease(self.root.clone(), self.config, lease, lease_file, now)?;
        *self = reloaded;
        self.replay_claims = replay_claims;
        outcome
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
            // The entry came from the checkpoint index, so a frame it names
            // that no longer decodes is a stale index, not a verdict on the
            // file: the next open's full scan locates any corruption.
            let record = match decode_complete_frame(&frame, entry.file_offset, self.config.host) {
                Ok(record) if entry.matches_record(&record) => record,
                Ok(_) | Err(_) => return self.fail_checkpoint_mismatch(),
            };
            let pending = self
                .pending
                .get_mut(index)
                .ok_or(HookSpoolError::MetadataCorrupted)?;
            pending.envelope = Some(record.envelope.clone());
            pending.native_lifecycle = record.native_lifecycle.clone();
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

    fn pending_bytes(&self) -> u64 {
        self.pending_by_session
            .values()
            .map(|(_, bytes)| *bytes)
            .sum()
    }

    #[tracing::instrument(name = "hooks.spool.compact", level = "trace", skip_all)]
    /// The pending frames' bytes, in order, and their entries at the offsets
    /// a file holding only those bytes gives them.
    fn pending_frames(&mut self) -> Result<(Vec<u8>, Vec<PendingRecordV1>), HookSpoolError> {
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
        Ok((bytes, rebuilt))
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

fn records_path(root: &Path) -> PathBuf {
    root.join(RECORDS_FILE)
}

/// Metadata may name only sequences the durable records file holds, so every
/// frame written so far commits before a metadata write. The caller holds the
/// writer lease, so the file holds only complete frames.
fn write_meta_after_records(root: &Path, meta: &HookSpoolMetaV1) -> Result<(), HookSpoolError> {
    if let Some(revision) = records_file_revision(root)? {
        commit_records(root, revision.identity, revision.length, COMMIT_WAIT)?;
    }
    write_meta(root, meta)
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

fn replay_cursor_path(root: &Path) -> PathBuf {
    root.join(REPLAY_CURSOR_FILE)
}

fn ensure_root(root: &Path) -> Result<(), HookSpoolError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(HookSpoolError::UnsafePath);
        }
        // An existing root must already be private to the current owner; a
        // permissive or foreign-owned one is refused, never re-permissioned.
        Ok(_) => return ensure_existing_private_root(root),
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
            ensure_existing_private_root(root)?;
        }
        Err(_) => return Err(HookSpoolError::Io),
    }
    {
        let _span = tracing::trace_span!("hooks.spool.fsync.directory").entered();
        shared_sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)
    }
}

fn ensure_existing_private_root(root: &Path) -> Result<(), HookSpoolError> {
    match tracedecay_private_fs::validate_private_directory(root) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput
            ) =>
        {
            Err(HookSpoolError::UnsafePath)
        }
        Err(_) => Err(HookSpoolError::Io),
    }
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

/// The fair-replay cursor only orders sessions, so it is an accelerator: a
/// missing or torn one restarts the rotation.
fn read_replay_cursor(root: &Path) -> Result<Option<[u8; 32]>, HookSpoolError> {
    Ok(read_bounded(&replay_cursor_path(root), 32)?.and_then(|bytes| bytes.try_into().ok()))
}

fn write_replay_cursor(root: &Path, cursor: [u8; 32]) -> Result<(), HookSpoolError> {
    atomic_write_accelerator(&replay_cursor_path(root), "replay-cursor", &cursor)
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
