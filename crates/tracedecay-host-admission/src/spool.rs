//! Bounded daemon-local admission spool for non-replayable host events.
//!
//! The spool is not a remote/offline queue. The authority daemon owns it and
//! replays records through canonical capture. Active frames leave only after a
//! canonical commit is acknowledged or their exact bytes and typed terminal
//! reason are durably preserved in the bounded quarantine.
//!
//! Appends are group-committed: one batch of records shares one append-intent
//! publish and one frame sync. Acknowledgements advance the watermark in
//! memory; it becomes durable with the next metadata publish, so a crash can
//! only replay an acknowledged record again, never lose an admitted one.
//! Quarantine publishes its frame first. Physical compaction of retained
//! prefix bytes is lazy, batched, and always follows a durable watermark, so
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
pub(crate) use bounds::SpoolOverflowDisposition;
#[cfg(test)]
pub(crate) use types::take_cloned_payload_bytes;
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
/// Retained waste must also reach this fraction of the spool bound, so a
/// spool that drains after every event rewrites once per many events instead
/// of paying a metadata publish and a rewrite on each drain.
const COMPACT_MIN_WASTE_DIVISOR: u64 = 16;
#[cfg(test)]
static FAIL_TERMINAL_MOVE_AT: Mutex<Option<(PathBuf, TerminalMoveFailure)>> = Mutex::new(None);

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalMoveFailure {
    AfterQuarantinePublish,
    AfterActivePublish,
}

/// Items admitted into one group-commit batch, not yet published.
struct PlannedBatch<'a> {
    next_seq: u64,
    records: usize,
    bytes: Vec<u8>,
    by_source: BTreeMap<&'a str, (usize, usize)>,
}

impl PlannedBatch<'_> {
    fn new(next_seq: u64) -> Self {
        Self {
            next_seq,
            records: 0,
            bytes: Vec::new(),
            by_source: BTreeMap::new(),
        }
    }
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
    /// The published metadata trails the in-memory view: an acknowledgement,
    /// or a batch whose append intent is still published, has not been
    /// written since.
    meta_unpublished: bool,
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
        let mut scan = {
            let _span = tracing::trace_span!("usecases.admission.scan").entered();
            scan_records(&records_path, bounds, &quarantine)
        }?;
        // Complete frames of a published append intent were issued even
        // though `next_seq` does not count them yet.
        let issued_next_seq = meta
            .append_intent
            .as_ref()
            .and_then(|intent| intent.written_prefix(&scan.records))
            .and_then(<[SpoolRecord]>::last)
            .map_or(meta.next_seq, |last| meta.next_seq.max(last.seq + 1));
        quarantine_report.truncated_partial_tail_bytes = quarantine.recover_partial_tail(
            &scan.records,
            meta.committed_through,
            issued_next_seq,
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
                meta_unpublished: false,
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

    #[cfg(test)]
    pub(crate) fn append(
        &mut self,
        source: &str,
        payload: &[u8],
    ) -> Result<SpoolRecord, SpoolError> {
        self.append_batch(&[(source, payload)])
            .pop()
            .unwrap_or(Err(SpoolError::MetadataCorrupted))
    }

    /// Durably append one group-commit batch, one result per item in order.
    ///
    /// Every admissible item shares one append-intent publish, one frame
    /// write, and one frame sync; an item that exceeds a bound is refused on
    /// its own without consuming a sequence. The intent stays published after
    /// success: reopen reconciles it against the frames and the next metadata
    /// publish supersedes it, so a durable batch needs no second metadata
    /// barrier.
    ///
    /// Any publication failure is ambiguous for the whole batch: this process
    /// refuses more mutations, and reopen performs the exact append-crash
    /// recovery without duplicating a frame.
    pub(crate) fn append_batch(
        &mut self,
        items: &[(&str, &[u8])],
    ) -> Vec<Result<SpoolRecord, SpoolError>> {
        if let Err(error) = self.ensure_mutations_allowed() {
            return vec![Err(error); items.len()];
        }
        if self.meta.next_seq == 0 || self.meta.next_seq == u64::MAX {
            return vec![Err(SpoolError::MetadataCorrupted); items.len()];
        }
        // Acknowledged bytes are reclaimed here, amortized across many
        // batches, so no commit waits on a rewrite.
        if self.should_compact_retained_prefix()
            && let Err(error) = self.compact_pending()
        {
            return vec![Err(error); items.len()];
        }
        let mut batch = PlannedBatch::new(self.meta.next_seq);
        let planned = items
            .iter()
            .map(|(source, payload)| self.plan_append(&mut batch, source, payload))
            .collect::<Vec<_>>();
        if batch.records > 0 {
            metrics::gauge!("usecases.admission.batch_records").set((batch.records) as f64);
            if let Err(error) = self.publish_batch(&batch) {
                self.append_recovery_required = true;
                return planned
                    .into_iter()
                    .map(|planned| planned.and(Err(error.clone())))
                    .collect();
            }
        }

        let mut file_offset = self.physical_len;
        let results = planned
            .into_iter()
            .zip(items)
            .map(|(planned, (source, payload))| {
                let (seq, framed_len) = planned?;
                let record = SpoolRecord {
                    seq,
                    source: (*source).to_owned(),
                    payload: payload.to_vec(),
                    file_offset,
                    framed_len,
                };
                file_offset += framed_len as u64;
                let source_usage = self
                    .pending_by_source
                    .entry(record.source.clone())
                    .or_default();
                source_usage.0 += 1;
                source_usage.1 += framed_len;
                self.pending.push(record.clone());
                Ok(record)
            })
            .collect();
        self.pending_bytes += batch.bytes.len();
        self.physical_len = file_offset;
        // Reopen reconciles the still-published intent; in memory the batch's
        // frames are already published.
        self.meta.next_seq = batch.next_seq;
        results
    }

    /// Admit one item into `batch`, returning its sequence and framed length.
    fn plan_append<'a>(
        &mut self,
        batch: &mut PlannedBatch<'a>,
        source: &'a str,
        payload: &[u8],
    ) -> Result<(u64, usize), SpoolError> {
        validate_record_bounds(source.as_bytes(), payload, self.bounds)?;
        if self.pending.len() + batch.records >= self.bounds.max_records {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxRecords));
        }
        let (durable_count, durable_bytes) = self
            .pending_by_source
            .get(source)
            .copied()
            .unwrap_or((0, 0));
        let (batch_count, batch_bytes) = batch.by_source.get(source).copied().unwrap_or((0, 0));
        if durable_count + batch_count >= self.bounds.max_records_per_source {
            return Err(SpoolError::Overflow(
                SpoolOverflowDisposition::MaxRecordsPerSource,
            ));
        }
        if batch.next_seq == u64::MAX {
            return Err(SpoolError::MetadataCorrupted);
        }
        let frame = encode_frame(batch.next_seq, source.as_bytes(), payload)?;
        let source_next_bytes = (durable_bytes + batch_bytes)
            .checked_add(frame.len())
            .ok_or(SpoolError::Overflow(
                SpoolOverflowDisposition::MaxBytesPerSource,
            ))?;
        if source_next_bytes > self.bounds.max_spool_bytes_per_source {
            return Err(SpoolError::Overflow(
                SpoolOverflowDisposition::MaxBytesPerSource,
            ));
        }
        let unwritten = batch.bytes.len() + frame.len();
        if self.pending_bytes.saturating_add(unwritten) > self.bounds.max_spool_bytes {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes));
        }
        let exceeds_physical = |spool: &Self| {
            spool.physical_len.saturating_add(unwritten as u64)
                > spool.bounds.max_spool_bytes as u64
        };
        if exceeds_physical(self) {
            self.compact_pending()?;
        }
        if exceeds_physical(self) {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes));
        }
        let seq = batch.next_seq;
        let framed_len = frame.len();
        batch.bytes.extend_from_slice(&frame);
        batch.records += 1;
        batch.next_seq += 1;
        let usage = batch.by_source.entry(source).or_default();
        usage.0 += 1;
        usage.1 += framed_len;
        Ok((seq, framed_len))
    }

    /// Publish the batch's append intent, then write and sync its frames.
    fn publish_batch(&mut self, batch: &PlannedBatch<'_>) -> Result<(), SpoolError> {
        let physical_len = self.physical_len;
        let mut intent_meta = self.meta.clone();
        intent_meta.append_intent = Some(AppendIntentV1::new(
            self.meta.next_seq,
            physical_len,
            batch.records as u64,
            &batch.bytes,
        ));
        write_meta_atomic(&self.meta_path, &intent_meta)?;
        // The intent carried every in-memory acknowledgement with it, and
        // names the frames by offset until the next publish replaces it.
        self.meta_unpublished = true;
        let file_offset = append_frame_durable(&self.records_path, &batch.bytes)?;
        if file_offset != physical_len {
            return Err(SpoolError::Corrupted {
                at_offset: physical_len,
            });
        }
        Ok(())
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
        let active_frame = {
            let record = &self.pending[index];
            encode_frame(record.seq, record.source.as_bytes(), &record.payload)?
        };
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

        let record = self.pending.remove(index);
        self.pending_bytes = self.pending_bytes.saturating_sub(record.framed_len);
        self.source_usage_release(&record.source, record.framed_len);
        self.cleanup_pending = true;
        let compacted = self.should_compact_retained_prefix();
        if compacted && self.compact_pending().is_err() {
            self.cleanup_pending = true;
            self.quarantine_recovery_required = true;
            return Err(SpoolError::QuarantineRecoveryRequired);
        }

        #[cfg(test)]
        if compacted
            && fail_terminal_move_at(&self.records_path, TerminalMoveFailure::AfterActivePublish)?
        {
            self.quarantine_recovery_required = true;
            return Err(SpoolError::QuarantineRecoveryRequired);
        }
        Ok(())
    }

    /// Acknowledge the oldest record only after canonical commit is durable,
    /// then publish the watermark.
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
        self.ack_through(seq)?;
        self.publish_acknowledgements()?;
        Ok(committed)
    }

    /// Acknowledge the pending prefix through `through` after its canonical
    /// commit is durable.
    ///
    /// The watermark advances in memory and becomes durable with the next
    /// metadata publish, normally the next batch's append intent. A crash
    /// before then replays the acknowledged records once more, which
    /// canonical capture resolves as exact duplicates; an acknowledgement is
    /// repeated, never lost.
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
        self.meta.committed_through = through;
        self.meta_unpublished = true;
        for record in self.pending.drain(..count).collect::<Vec<_>>() {
            self.pending_bytes = self.pending_bytes.saturating_sub(record.framed_len);
            self.source_usage_release(&record.source, record.framed_len);
        }
        self.cleanup_pending = true;
        Ok(count)
    }

    /// Durably publish the in-memory watermark, then reclaim acknowledged
    /// physical bytes once the retained prefix is worth rewriting.
    #[cfg(test)]
    pub(crate) fn publish_acknowledgements(&mut self) -> Result<(), SpoolError> {
        self.ensure_mutations_allowed()?;
        self.publish_meta()?;
        if self.should_compact_retained_prefix() {
            self.compact_pending()?;
        }
        Ok(())
    }

    fn publish_meta(&mut self) -> Result<(), SpoolError> {
        if self.meta_unpublished {
            write_meta_atomic(&self.meta_path, &self.meta)?;
            self.meta_unpublished = false;
        }
        Ok(())
    }

    fn should_compact_retained_prefix(&self) -> bool {
        if !self.cleanup_pending {
            return false;
        }
        let pending = self.pending_bytes as u64;
        let waste = self.physical_len.saturating_sub(pending);
        waste >= self.bounds.max_spool_bytes as u64 / COMPACT_MIN_WASTE_DIVISOR
            && self.physical_len > pending.saturating_mul(COMPACT_WASTE_MULTIPLIER)
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

    /// Rewrite the active file down to its pending frames. Metadata is
    /// published first: reopen must never find acknowledged frames gone
    /// while the durable watermark still names them pending, nor an append
    /// intent naming offsets the rewrite moved.
    fn compact_pending(&mut self) -> Result<(), SpoolError> {
        self.ensure_mutations_allowed()?;
        self.publish_meta()?;
        if self.pending.is_empty() {
            {
                let _span = tracing::trace_span!("usecases.admission.compact").entered();
                truncate_file(&self.records_path, 0)
            }?;
            self.pending_bytes = 0;
            self.physical_len = 0;
            self.cleanup_pending = false;
            return Ok(());
        }
        let rebuilt = {
            let _span = tracing::trace_span!("usecases.admission.compact").entered();
            {
                with_owned_temp_publish(
                    &self.records_path,
                    "compact",
                    "host admission spool",
                    |output| {
                        let mut rebuilt = Vec::with_capacity(self.pending.len());
                        let mut offset = 0u64;
                        for record in &self.pending {
                            let frame = encode_frame(
                                record.seq,
                                record.source.as_bytes(),
                                &record.payload,
                            )?;
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
                )
            }
        }?;
        self.pending = rebuilt;
        self.pending_bytes = self.pending.iter().map(|record| record.framed_len).sum();
        self.physical_len = self.pending_bytes as u64;
        self.cleanup_pending = false;
        Ok(())
    }

    pub(crate) fn ensure_mutations_allowed(&self) -> Result<(), SpoolError> {
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
