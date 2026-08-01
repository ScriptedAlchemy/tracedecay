//! Bounded daemon-local admission spool for non-replayable host events.
//!
//! The spool is not a remote/offline queue. The authority daemon owns it and
//! replays records through canonical capture. Active frames leave only after a
//! canonical commit is acknowledged or their exact bytes and typed terminal
//! reason are durably preserved in the bounded quarantine.
//!
//! Durable ack/quarantine publishes a metadata watermark (or quarantine frame)
//! first. Physical compaction of retained prefix bytes is lazy and batched so
//! repeated acknowledgements stay O(pending) amortized rather than rewriting
//! the full active file on every ack. Callers that bridge this sync I/O onto a
//! Tokio runtime must keep blocking open/append/ack/quarantine off worker
//! threads (for example via `spawn_blocking` or a dedicated serialized actor).

use std::collections::BTreeMap;
use std::io::Write;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::Mutex;

mod bounds;
mod frames;
mod fs_ops;
mod meta;
mod quarantine;
mod recovery;
mod types;

#[cfg(test)]
mod tests;

use bounds::{validate_bounds, validate_record_bounds};
use frames::{
    FORMAT_VERSION, append_frame_durable, encode_frame, is_proven_unpublished_active_tail,
    scan_records, validate_quarantined_active_frame,
};
use fs_ops::{
    file_len, io_error, sync_parent_directory, tighten_existing_file, truncate_file,
    with_owned_temp_publish,
};
use meta::{
    AppendIntentV1, META_FILE, SpoolMetaV1, append_intent_is_reconciled, read_meta,
    validate_append_intent, validate_meta_watermarks, write_meta_atomic,
};
use quarantine::TerminalQuarantine;
use recovery::recover_pending;

pub use bounds::SpoolBounds;
pub(crate) use bounds::{
    DEFAULT_MAX_RECORD_BYTES, DEFAULT_MAX_RECORDS, DEFAULT_MAX_SOURCE_BYTES,
    DEFAULT_MAX_SPOOL_BYTES, SpoolOverflowDisposition,
};
// Re-exported to preserve the pre-split `spool::DEFAULT_MAX_*_PER_SOURCE` paths;
// their only consumer (`SpoolBounds::default`) lives in `bounds` itself.
#[allow(unused_imports)]
pub(crate) use bounds::{DEFAULT_MAX_RECORDS_PER_SOURCE, DEFAULT_MAX_SPOOL_BYTES_PER_SOURCE};
pub(crate) use types::{SpoolError, SpoolIntegrity};
pub use types::{SpoolOpenReport, SpoolRecord, TerminalReason};

#[cfg(test)]
use frames::{CHECKSUM_BYTES, FRAME_HEADER_BYTES, FRAME_MAGIC};
#[cfg(test)]
use meta::{FAIL_META_WRITE_FOR, MAX_META_BYTES};

const RECORDS_FILE: &str = "records.bin";
const QUARANTINE_FILE: &str = "quarantine.bin";
/// Compact retained physical prefix once waste exceeds this multiple of the
/// logical pending byte count. Keeps ack paths metadata-only until a batch is
/// worthwhile, while still amortizing rewrites to linear in live bytes.
const COMPACT_WASTE_MULTIPLIER: u64 = 2;
#[cfg(test)]
static FAIL_TERMINAL_MOVE_AT: Mutex<Option<(PathBuf, TerminalMoveFailure)>> = Mutex::new(None);

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalMoveFailure {
    AfterQuarantinePublish,
    AfterActivePublish,
}

#[derive(Debug)]
pub(crate) struct HostAdmissionSpool {
    records_path: PathBuf,
    meta_path: PathBuf,
    bounds: SpoolBounds,
    meta: SpoolMetaV1,
    pending: Vec<SpoolRecord>,
    pending_bytes: usize,
    physical_len: u64,
    pending_by_source: BTreeMap<String, (usize, usize)>,
    cleanup_pending: bool,
    append_recovery_required: bool,
    quarantine_recovery_required: bool,
    quarantine: TerminalQuarantine,
}

impl HostAdmissionSpool {
    pub(crate) fn open(
        dir: impl Into<PathBuf>,
        bounds: SpoolBounds,
    ) -> Result<(Self, SpoolOpenReport), SpoolError> {
        validate_bounds(bounds)?;
        let dir = dir.into();
        let dir_existed = dir.exists();
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&dir).map_err(io_error)?;
        if !dir_existed {
            sync_parent_directory(&dir)?;
        }
        let records_path = dir.join(RECORDS_FILE);
        let meta_path = dir.join(META_FILE);
        let quarantine_path = dir.join(QUARANTINE_FILE);
        tighten_existing_file(&records_path)?;
        tighten_existing_file(&meta_path)?;
        tighten_existing_file(&quarantine_path)?;
        let meta_existed = meta_path.exists();
        let mut meta = read_meta(&meta_path)?.unwrap_or_else(SpoolMetaV1::fresh);
        if meta.version != FORMAT_VERSION {
            return Err(SpoolError::UnsupportedVersion(meta.version));
        }
        validate_meta_watermarks(&meta)?;
        validate_append_intent(&meta, bounds)?;

        let (mut quarantine, mut quarantine_report) =
            TerminalQuarantine::open(quarantine_path, bounds)?;
        let mut scan = scan_records(&records_path, bounds, &quarantine)?;
        quarantine_report.truncated_partial_tail_bytes = quarantine.recover_partial_tail(
            &scan.records,
            meta.committed_through,
            meta.next_seq,
        )?;
        if matches!(scan.integrity, SpoolIntegrity::Healthy)
            && scan.truncate_to < scan.file_len
            && !is_proven_unpublished_active_tail(&records_path, &scan, &meta, &quarantine, bounds)?
        {
            scan.integrity = SpoolIntegrity::Corrupted {
                at_offset: scan.truncate_to,
            };
        }
        // Only a partial append proven by metadata and active/quarantine sequence
        // evidence may be discarded. Every other suffix stays intact for forensics.
        let truncated_partial_tail_bytes = match &scan.integrity {
            SpoolIntegrity::Healthy if scan.truncate_to < scan.file_len => {
                truncate_file(&records_path, scan.truncate_to)?;
                scan.file_len.saturating_sub(scan.truncate_to)
            }
            SpoolIntegrity::Healthy | SpoolIntegrity::Corrupted { .. } => 0,
        };
        if let SpoolIntegrity::Corrupted { at_offset } = &scan.integrity {
            meta.integrity = SpoolIntegrity::Corrupted {
                at_offset: *at_offset,
            };
            write_meta_atomic(&meta_path, &meta)?;
        }

        let clear_append_intent =
            append_intent_is_reconciled(&scan, &meta, truncated_partial_tail_bytes)?;
        let recovery = recover_pending(scan.records, &quarantine, &meta, bounds)?;
        let mut meta_changed = false;
        if let Some(next_seq) = recovery.recovered_next_seq {
            meta.next_seq = next_seq;
            meta_changed = true;
        }
        if clear_append_intent {
            meta.append_intent = None;
            meta_changed = true;
        }
        if meta_changed || !meta_existed {
            write_meta_atomic(&meta_path, &meta)?;
        }

        let cleanup_pending = matches!(scan.integrity, SpoolIntegrity::Healthy)
            && scan.truncate_to > recovery.pending_bytes as u64;
        let physical_len = file_len(&records_path)?;
        let report = SpoolOpenReport {
            pending_records: recovery.pending.len(),
            truncated_partial_tail_bytes,
            integrity: meta.integrity.clone(),
            committed_through: meta.committed_through,
            next_seq: meta.next_seq,
            quarantined_records: quarantine_report.records,
            quarantine_bytes: quarantine_report.bytes,
            quarantine_truncated_partial_tail_bytes: quarantine_report.truncated_partial_tail_bytes,
        };
        Ok((
            Self {
                records_path,
                meta_path,
                bounds,
                meta,
                pending: recovery.pending,
                pending_bytes: recovery.pending_bytes,
                physical_len,
                pending_by_source: recovery.pending_by_source,
                cleanup_pending,
                append_recovery_required: false,
                quarantine_recovery_required: false,
                quarantine,
            },
            report,
        ))
    }

    pub(crate) fn bounds(&self) -> SpoolBounds {
        self.bounds
    }

    #[cfg(test)]
    pub(crate) fn integrity(&self) -> &SpoolIntegrity {
        &self.meta.integrity
    }

    pub(crate) fn committed_through(&self) -> u64 {
        self.meta.committed_through
    }

    pub(crate) fn pending_records(&self) -> &[SpoolRecord] {
        &self.pending
    }

    pub(crate) fn pending_record(&self, seq: u64) -> Option<&SpoolRecord> {
        self.pending
            .binary_search_by_key(&seq, |record| record.seq)
            .ok()
            .map(|index| &self.pending[index])
    }

    pub(crate) fn ensure_replay_allowed(&self) -> Result<(), SpoolError> {
        self.ensure_mutations_allowed()
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// True when a frame publication may have completed without its metadata
    /// update. Pending reads remain the last known metadata view until reopen.
    #[cfg(test)]
    pub(crate) fn recovery_required(&self) -> bool {
        self.append_recovery_required || self.quarantine_recovery_required
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub(crate) fn quarantine_count(&self) -> usize {
        self.quarantine.len()
    }

    #[cfg(test)]
    pub(crate) fn quarantined_record(&self, seq: u64) -> Option<(TerminalReason, &[u8])> {
        self.quarantine
            .entry(seq)
            .map(|entry| (entry.reason, entry.active_frame.as_slice()))
    }

    /// Durably publish append intent, then the frame, then the next sequence.
    ///
    /// If metadata publication fails after frame sync, this process refuses more
    /// appends. Reopen performs the exact append-crash recovery and advances the
    /// sequence once without duplicating the frame.
    pub(crate) fn append(
        &mut self,
        source: &str,
        payload: &[u8],
    ) -> Result<SpoolRecord, SpoolError> {
        self.ensure_mutations_allowed()?;
        if let SpoolIntegrity::Corrupted { at_offset } = self.meta.integrity {
            return Err(SpoolError::Corrupted { at_offset });
        }
        validate_record_bounds(source.as_bytes(), payload, self.bounds)?;
        if self.pending.len() >= self.bounds.max_records {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxRecords));
        }
        let (source_pending_count, source_pending_bytes) = self
            .pending_by_source
            .get(source)
            .copied()
            .unwrap_or((0, 0));
        if source_pending_count >= self.bounds.max_records_per_source {
            return Err(SpoolError::Overflow(
                SpoolOverflowDisposition::MaxRecordsPerSource,
            ));
        }
        if self.meta.next_seq == 0 || self.meta.next_seq == u64::MAX {
            return Err(SpoolError::MetadataCorrupted);
        }

        let seq = self.meta.next_seq;
        let frame = encode_frame(seq, source.as_bytes(), payload)?;
        let source_next_bytes =
            source_pending_bytes
                .checked_add(frame.len())
                .ok_or(SpoolError::Overflow(
                    SpoolOverflowDisposition::MaxBytesPerSource,
                ))?;
        if source_next_bytes > self.bounds.max_spool_bytes_per_source {
            return Err(SpoolError::Overflow(
                SpoolOverflowDisposition::MaxBytesPerSource,
            ));
        }
        if self.pending_bytes.saturating_add(frame.len()) > self.bounds.max_spool_bytes {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes));
        }
        if self.physical_len.saturating_add(frame.len() as u64) > self.bounds.max_spool_bytes as u64
        {
            self.compact_pending()?;
        }
        if self.physical_len.saturating_add(frame.len() as u64) > self.bounds.max_spool_bytes as u64
        {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes));
        }
        let physical_len = self.physical_len;

        let intent = AppendIntentV1::new(seq, physical_len, &frame);
        let mut intent_meta = self.meta.clone();
        intent_meta.append_intent = Some(intent);
        if let Err(error) = write_meta_atomic(&self.meta_path, &intent_meta) {
            self.append_recovery_required = true;
            return Err(error);
        }
        self.meta = intent_meta;

        let file_offset = match append_frame_durable(&self.records_path, &frame) {
            Ok(offset) => offset,
            Err(error) => {
                // Any publication error is ambiguous: write or sync may have
                // persisted a prefix/full frame. Reopen is the only safe path.
                self.append_recovery_required = true;
                return Err(error);
            }
        };
        if file_offset != physical_len {
            self.append_recovery_required = true;
            return Err(SpoolError::Corrupted {
                at_offset: physical_len,
            });
        }
        let record = SpoolRecord {
            seq,
            source: source.to_owned(),
            payload: payload.to_vec(),
            file_offset,
            framed_len: frame.len(),
        };
        let mut next_meta = self.meta.clone();
        next_meta.next_seq = seq + 1;
        next_meta.append_intent = None;
        if let Err(error) = write_meta_atomic(&self.meta_path, &next_meta) {
            self.append_recovery_required = true;
            return Err(error);
        }
        self.meta = next_meta;
        self.pending_bytes += record.framed_len;
        self.physical_len += record.framed_len as u64;
        let source_usage = self.pending_by_source.entry(source.to_owned()).or_default();
        source_usage.0 += 1;
        source_usage.1 += record.framed_len;
        self.pending.push(record.clone());
        Ok(record)
    }

    /// Preserve a terminal record in the bounded checksummed quarantine before
    /// removing it from active replay and capacity accounting.
    pub(crate) fn quarantine(
        &mut self,
        seq: u64,
        reason: TerminalReason,
    ) -> Result<(), SpoolError> {
        self.ensure_mutations_allowed()?;
        let Some(index) = self.pending.iter().position(|record| record.seq == seq) else {
            if let Some(entry) = self.quarantine.entry(seq) {
                if entry.reason == reason {
                    return Ok(());
                }
                self.quarantine_recovery_required = true;
                return Err(SpoolError::QuarantineCorrupted { at_offset: 0 });
            }
            return Err(SpoolError::AckUnknown { seq });
        };
        let record = self.pending[index].clone();
        let active_frame = encode_frame(record.seq, record.source.as_bytes(), &record.payload)?;
        match self.quarantine.preserve(seq, reason, &active_frame) {
            Ok(_) => {}
            Err(SpoolError::Io) => {
                self.quarantine_recovery_required = true;
                return Err(SpoolError::QuarantineRecoveryRequired);
            }
            Err(error @ SpoolError::QuarantineCorrupted { .. }) => {
                self.quarantine_recovery_required = true;
                return Err(error);
            }
            Err(error) => return Err(error),
        }

        #[cfg(test)]
        if fail_terminal_move_at(
            &self.records_path,
            TerminalMoveFailure::AfterQuarantinePublish,
        )? {
            self.quarantine_recovery_required = true;
            return Err(SpoolError::QuarantineRecoveryRequired);
        }

        self.pending.remove(index);
        self.pending_bytes = self.pending_bytes.saturating_sub(record.framed_len);
        self.source_usage_release(&record.source, record.framed_len);
        let compacted = self.publish_logical_deletion_cleanup(true)?;

        #[cfg(not(test))]
        let _ = compacted;

        #[cfg(test)]
        if compacted
            && fail_terminal_move_at(&self.records_path, TerminalMoveFailure::AfterActivePublish)?
        {
            self.quarantine_recovery_required = true;
            return Err(SpoolError::QuarantineRecoveryRequired);
        }
        Ok(())
    }

    /// Acknowledge the oldest record only after canonical commit is durable.
    ///
    /// The metadata watermark is written first. Once it succeeds, retained
    /// physical bytes are logically deleted even if compaction is deferred,
    /// fails, or crashes before the compacted file is published.
    #[cfg(test)]
    pub(crate) fn ack(&mut self, seq: u64) -> Result<SpoolRecord, SpoolError> {
        self.ensure_mutations_allowed()?;
        let Some(head) = self.pending.first() else {
            return Err(SpoolError::AckUnknown { seq });
        };
        if head.seq != seq {
            return Err(SpoolError::AckOutOfOrder {
                expected: head.seq,
                got: seq,
            });
        }
        let committed = head.clone();
        let mut next_meta = self.meta.clone();
        next_meta.committed_through = seq;
        write_meta_atomic(&self.meta_path, &next_meta)?;

        self.meta = next_meta;
        self.pending.remove(0);
        self.pending_bytes = self.pending_bytes.saturating_sub(committed.framed_len);
        self.source_usage_release(&committed.source, committed.framed_len);
        let _compacted = self.publish_logical_deletion_cleanup(false)?;
        Ok(committed)
    }

    pub(crate) fn ack_through(&mut self, through: u64) -> Result<usize, SpoolError> {
        self.ensure_mutations_allowed()?;
        if through <= self.meta.committed_through {
            // Already-committed watermarks are idempotent no-ops.
            return Ok(0);
        }
        let Some(tail) = self.pending.last() else {
            return Err(SpoolError::AckUnknown { seq: through });
        };
        if through > tail.seq {
            return Err(SpoolError::AckUnknown { seq: through });
        }
        let Some(last_index) = self.pending.iter().position(|record| record.seq == through) else {
            let expected = self.pending.first().map_or(through, |record| record.seq);
            return Err(SpoolError::AckOutOfOrder {
                expected,
                got: through,
            });
        };
        let count = last_index + 1;
        let removed_bytes = self.pending[..count]
            .iter()
            .map(|record| record.framed_len)
            .sum::<usize>();
        let released = self.pending[..count]
            .iter()
            .map(|record| (record.source.clone(), record.framed_len))
            .collect::<Vec<_>>();
        let mut next_meta = self.meta.clone();
        next_meta.committed_through = through;
        write_meta_atomic(&self.meta_path, &next_meta)?;

        self.meta = next_meta;
        self.pending.drain(..count);
        self.pending_bytes = self.pending_bytes.saturating_sub(removed_bytes);
        for (source, framed_len) in released {
            self.source_usage_release(&source, framed_len);
        }
        let _compacted = self.publish_logical_deletion_cleanup(false)?;
        Ok(count)
    }

    /// After a durable logical deletion, optionally rewrite the active file.
    ///
    /// Returns whether a successful compaction ran. Compaction is deferred while
    /// retained waste is below [`COMPACT_WASTE_MULTIPLIER`] times live pending
    /// bytes; an empty live prefix always compacts. Append forces cleanup only
    /// when retained physical bytes would otherwise exceed the spool bound.
    fn publish_logical_deletion_cleanup(
        &mut self,
        fence_compact_failure: bool,
    ) -> Result<bool, SpoolError> {
        self.cleanup_pending = true;
        let should_compact = self.should_compact_retained_prefix();
        if !should_compact {
            return Ok(false);
        }
        if self.compact_pending().is_ok() {
            return Ok(true);
        }
        self.cleanup_pending = true;
        if fence_compact_failure {
            self.quarantine_recovery_required = true;
            return Err(SpoolError::QuarantineRecoveryRequired);
        }
        Ok(false)
    }

    fn should_compact_retained_prefix(&self) -> bool {
        if !self.cleanup_pending {
            return false;
        }
        if self.pending.is_empty() {
            return true;
        }
        let pending = self.pending_bytes as u64;
        self.physical_len > pending.saturating_mul(COMPACT_WASTE_MULTIPLIER)
    }

    fn source_usage_release(&mut self, source: &str, framed_len: usize) {
        if let Some(entry) = self.pending_by_source.get_mut(source) {
            entry.0 = entry.0.saturating_sub(1);
            entry.1 = entry.1.saturating_sub(framed_len);
            if entry.0 == 0 {
                self.pending_by_source.remove(source);
            }
        }
    }

    fn compact_pending(&mut self) -> Result<(), SpoolError> {
        self.ensure_mutations_allowed()?;
        let rebuilt = with_owned_temp_publish(
            &self.records_path,
            "compact",
            "host admission spool",
            |output| {
                let mut rebuilt = Vec::with_capacity(self.pending.len());
                let mut offset = 0u64;
                for record in &self.pending {
                    let frame =
                        encode_frame(record.seq, record.source.as_bytes(), &record.payload)?;
                    output.write_all(&frame).map_err(io_error)?;
                    rebuilt.push(SpoolRecord {
                        seq: record.seq,
                        source: record.source.clone(),
                        payload: record.payload.clone(),
                        file_offset: offset,
                        framed_len: frame.len(),
                    });
                    offset += frame.len() as u64;
                }
                Ok(rebuilt)
            },
        )?;
        self.pending = rebuilt;
        self.pending_bytes = self.pending.iter().map(|record| record.framed_len).sum();
        self.physical_len = self.pending_bytes as u64;
        self.cleanup_pending = false;
        Ok(())
    }

    fn ensure_mutations_allowed(&self) -> Result<(), SpoolError> {
        // Corrupted active files are forensic evidence: never compact, append,
        // ack, or quarantine-move while the on-disk suffix is still intact.
        if let SpoolIntegrity::Corrupted { at_offset } = self.meta.integrity {
            Err(SpoolError::Corrupted { at_offset })
        } else if self.quarantine_recovery_required {
            Err(SpoolError::QuarantineRecoveryRequired)
        } else if self.append_recovery_required {
            Err(SpoolError::AppendRecoveryRequired)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
fn fail_terminal_move_at(path: &Path, point: TerminalMoveFailure) -> Result<bool, SpoolError> {
    let mut failure = FAIL_TERMINAL_MOVE_AT.lock().map_err(|_| SpoolError::Io)?;
    if failure
        .as_ref()
        .is_some_and(|(failure_path, failure_point)| {
            failure_path == path && *failure_point == point
        })
    {
        *failure = None;
        Ok(true)
    } else {
        Ok(false)
    }
}
