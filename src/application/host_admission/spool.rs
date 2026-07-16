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
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{HostAdmissionOutcome, HostAdmissionStatus};

mod quarantine;

use quarantine::{FRAME_OVERHEAD as QUARANTINE_FRAME_OVERHEAD, TerminalQuarantine};

const FRAME_MAGIC: &[u8; 4] = b"TDHA";
const FORMAT_VERSION: u16 = 1;
const FRAME_HEADER_BYTES: usize = 20;
const CHECKSUM_BYTES: usize = 32;
const RECORDS_FILE: &str = "records.bin";
const META_FILE: &str = "meta.json";
const QUARANTINE_FILE: &str = "quarantine.bin";
const MAX_META_BYTES: u64 = 4096;
/// Compact retained physical prefix once waste exceeds this multiple of the
/// logical pending byte count. Keeps ack paths metadata-only until a batch is
/// worthwhile, while still amortizing rewrites to linear in live bytes.
const COMPACT_WASTE_MULTIPLIER: u64 = 2;
#[cfg(test)]
static FAIL_META_WRITE_FOR: Mutex<Option<PathBuf>> = Mutex::new(None);
#[cfg(test)]
static FAIL_TERMINAL_MOVE_AT: Mutex<Option<(PathBuf, TerminalMoveFailure)>> = Mutex::new(None);

pub(crate) const DEFAULT_MAX_RECORD_BYTES: usize = 1024 * 1024;
pub(crate) const DEFAULT_MAX_SOURCE_BYTES: usize = 256;
pub(crate) const DEFAULT_MAX_SPOOL_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_RECORDS: usize = 4096;
pub(crate) const DEFAULT_MAX_SPOOL_BYTES_PER_SOURCE: usize = 4 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_RECORDS_PER_SOURCE: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub(crate) struct SpoolBounds {
    pub(crate) max_record_bytes: usize,
    pub(crate) max_source_bytes: usize,
    pub(crate) max_spool_bytes: usize,
    pub(crate) max_records: usize,
    pub(crate) max_spool_bytes_per_source: usize,
    pub(crate) max_records_per_source: usize,
    pub(crate) max_quarantine_bytes: usize,
    pub(crate) max_quarantine_records: usize,
}

impl SpoolBounds {
    pub(crate) const fn new(
        max_record_bytes: usize,
        max_source_bytes: usize,
        max_spool_bytes: usize,
        max_records: usize,
    ) -> Self {
        Self {
            max_record_bytes,
            max_source_bytes,
            max_spool_bytes,
            max_records,
            max_spool_bytes_per_source: max_spool_bytes,
            max_records_per_source: max_records,
            max_quarantine_bytes: max_spool_bytes
                .saturating_add(max_records.saturating_mul(QUARANTINE_FRAME_OVERHEAD)),
            max_quarantine_records: max_records,
        }
    }

    pub(crate) const fn with_source_limits(
        mut self,
        max_spool_bytes_per_source: usize,
        max_records_per_source: usize,
    ) -> Self {
        self.max_spool_bytes_per_source = max_spool_bytes_per_source;
        self.max_records_per_source = max_records_per_source;
        self
    }

    #[cfg(test)]
    pub(crate) const fn with_quarantine_limits(
        mut self,
        max_quarantine_bytes: usize,
        max_quarantine_records: usize,
    ) -> Self {
        self.max_quarantine_bytes = max_quarantine_bytes;
        self.max_quarantine_records = max_quarantine_records;
        self
    }
}

impl Default for SpoolBounds {
    fn default() -> Self {
        Self::new(
            DEFAULT_MAX_RECORD_BYTES,
            DEFAULT_MAX_SOURCE_BYTES,
            DEFAULT_MAX_SPOOL_BYTES,
            DEFAULT_MAX_RECORDS,
        )
        .with_source_limits(
            DEFAULT_MAX_SPOOL_BYTES_PER_SOURCE,
            DEFAULT_MAX_RECORDS_PER_SOURCE,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpoolOverflowDisposition {
    RecordTooLarge,
    SourceTooLarge,
    MaxBytes,
    MaxRecords,
    MaxBytesPerSource,
    MaxRecordsPerSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpoolIntegrity {
    Healthy,
    Corrupted { at_offset: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalReason {
    MalformedPayload,
    StaleBranchAuthorization,
}

impl TerminalReason {
    const fn code(self) -> u8 {
        match self {
            Self::MalformedPayload => 1,
            Self::StaleBranchAuthorization => 3,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::MalformedPayload),
            3 => Some(Self::StaleBranchAuthorization),
            _ => None,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalMoveFailure {
    AfterQuarantinePublish,
    AfterActivePublish,
}

/// Stable internal errors. No variant contains a path, provider payload, or raw
/// parser/OS error string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpoolError {
    Io,
    Overflow(SpoolOverflowDisposition),
    Corrupted { at_offset: u64 },
    MetadataCorrupted,
    UnsupportedVersion(u16),
    AckOutOfOrder { expected: u64, got: u64 },
    AckUnknown { seq: u64 },
    AppendRecoveryRequired,
    QuarantineFull,
    QuarantineCorrupted { at_offset: u64 },
    QuarantineRecoveryRequired,
}

impl SpoolError {
    pub(crate) const fn to_outcome(&self) -> HostAdmissionOutcome {
        match self {
            Self::Overflow(SpoolOverflowDisposition::RecordTooLarge) => {
                HostAdmissionOutcome::spool_record_too_large()
            }
            Self::Overflow(SpoolOverflowDisposition::SourceTooLarge) => {
                HostAdmissionOutcome::spool_source_too_large()
            }
            Self::Overflow(
                SpoolOverflowDisposition::MaxBytes
                | SpoolOverflowDisposition::MaxRecords
                | SpoolOverflowDisposition::MaxBytesPerSource
                | SpoolOverflowDisposition::MaxRecordsPerSource,
            ) => HostAdmissionOutcome::spool_overflow(),
            Self::UnsupportedVersion(_) => HostAdmissionOutcome::spool_unsupported_version(),
            Self::Corrupted { .. } | Self::MetadataCorrupted => {
                HostAdmissionOutcome::spool_corrupted()
            }
            Self::AckOutOfOrder { .. } | Self::AckUnknown { .. } => {
                HostAdmissionOutcome::spool_ack_conflict()
            }
            Self::AppendRecoveryRequired => HostAdmissionOutcome::spool_recovery_required(),
            Self::QuarantineFull => HostAdmissionOutcome::quarantine_full(),
            Self::QuarantineCorrupted { .. } => HostAdmissionOutcome::quarantine_corrupted(),
            Self::QuarantineRecoveryRequired => {
                HostAdmissionOutcome::quarantine_recovery_required()
            }
            Self::Io => HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("spool_io_failed"),
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpoolRecord {
    pub(crate) seq: u64,
    pub(crate) source: String,
    pub(crate) payload: Vec<u8>,
    pub(crate) file_offset: u64,
    pub(crate) framed_len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpoolOpenReport {
    pub(crate) pending_records: usize,
    pub(crate) truncated_partial_tail_bytes: u64,
    pub(crate) integrity: SpoolIntegrity,
    pub(crate) committed_through: u64,
    pub(crate) next_seq: u64,
    pub(crate) quarantined_records: usize,
    pub(crate) quarantine_bytes: usize,
    pub(crate) quarantine_truncated_partial_tail_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SpoolMetaV1 {
    version: u16,
    committed_through: u64,
    next_seq: u64,
    integrity: SpoolIntegrity,
}

impl SpoolMetaV1 {
    fn fresh() -> Self {
        Self {
            version: FORMAT_VERSION,
            committed_through: 0,
            next_seq: 1,
            integrity: SpoolIntegrity::Healthy,
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
        crate::storage::PrivateStoreIo::create_dir_all(&dir).map_err(io_error)?;
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

        let recovery = recover_pending(scan.records, &quarantine, &meta, bounds)?;
        if let Some(next_seq) = recovery.recovered_next_seq {
            meta.next_seq = next_seq;
            write_meta_atomic(&meta_path, &meta)?;
        } else if !meta_existed {
            write_meta_atomic(&meta_path, &meta)?;
        }

        let cleanup_pending = matches!(scan.integrity, SpoolIntegrity::Healthy)
            && scan.truncate_to > recovery.pending_bytes as u64;
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

    #[cfg(test)]
    pub(crate) fn quarantine_count(&self) -> usize {
        self.quarantine.len()
    }

    #[cfg(test)]
    pub(crate) fn quarantined_record(&self, seq: u64) -> Option<(TerminalReason, &[u8])> {
        self.quarantine
            .entry(seq)
            .map(|entry| (entry.reason, entry.active_frame.as_slice()))
    }

    /// Durably append a frame before publishing the next-sequence metadata.
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
        if self
            .pending
            .iter()
            .filter(|record| record.source == source)
            .count()
            >= self.bounds.max_records_per_source
        {
            return Err(SpoolError::Overflow(
                SpoolOverflowDisposition::MaxRecordsPerSource,
            ));
        }
        if self.meta.next_seq == 0 || self.meta.next_seq == u64::MAX {
            return Err(SpoolError::MetadataCorrupted);
        }

        let seq = self.meta.next_seq;
        let frame = encode_frame(seq, source.as_bytes(), payload)?;
        let source_pending_bytes = self
            .pending
            .iter()
            .filter(|record| record.source == source)
            .try_fold(0usize, |bytes, record| bytes.checked_add(record.framed_len))
            .ok_or(SpoolError::Overflow(
                SpoolOverflowDisposition::MaxBytesPerSource,
            ))?;
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
        let physical_len = file_len(&self.records_path)?;
        if physical_len.saturating_add(frame.len() as u64) > self.bounds.max_spool_bytes as u64 {
            self.compact_pending()?;
        }
        let physical_len = file_len(&self.records_path)?;
        if physical_len.saturating_add(frame.len() as u64) > self.bounds.max_spool_bytes as u64 {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes));
        }

        let file_offset = match append_frame_durable(&self.records_path, &frame) {
            Ok(offset) => offset,
            Err(error) => {
                // Any publication error is ambiguous: write or sync may have
                // persisted a prefix/full frame. Reopen is the only safe path.
                self.append_recovery_required = true;
                return Err(error);
            }
        };
        let record = SpoolRecord {
            seq,
            source: source.to_owned(),
            payload: payload.to_vec(),
            file_offset,
            framed_len: frame.len(),
        };
        let mut next_meta = self.meta.clone();
        next_meta.next_seq = seq + 1;
        if let Err(error) = write_meta_atomic(&self.meta_path, &next_meta) {
            self.append_recovery_required = true;
            return Err(error);
        }
        self.meta = next_meta;
        self.pending_bytes += record.framed_len;
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
        let mut next_meta = self.meta.clone();
        next_meta.committed_through = through;
        write_meta_atomic(&self.meta_path, &next_meta)?;

        self.meta = next_meta;
        self.pending.drain(..count);
        self.pending_bytes = self.pending_bytes.saturating_sub(removed_bytes);
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
        let should_compact = match self.should_compact_retained_prefix() {
            Ok(should_compact) => should_compact,
            Err(_) if !fence_compact_failure => return Ok(false),
            Err(_) => {
                self.quarantine_recovery_required = true;
                return Err(SpoolError::QuarantineRecoveryRequired);
            }
        };
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

    fn should_compact_retained_prefix(&self) -> Result<bool, SpoolError> {
        if !self.cleanup_pending {
            return Ok(false);
        }
        if self.pending.is_empty() {
            return Ok(true);
        }
        let physical = file_len(&self.records_path)?;
        let pending = self.pending_bytes as u64;
        Ok(physical > pending.saturating_mul(COMPACT_WASTE_MULTIPLIER))
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

fn validate_bounds(bounds: SpoolBounds) -> Result<(), SpoolError> {
    let minimum_frame = FRAME_HEADER_BYTES
        .checked_add(CHECKSUM_BYTES)
        .ok_or(SpoolError::MetadataCorrupted)?;
    if bounds.max_record_bytes > u32::MAX as usize
        || bounds.max_source_bytes > u16::MAX as usize
        || bounds.max_spool_bytes < minimum_frame
        || bounds.max_records == 0
        || bounds.max_spool_bytes_per_source < minimum_frame
        || bounds.max_spool_bytes_per_source > bounds.max_spool_bytes
        || bounds.max_records_per_source == 0
        || bounds.max_records_per_source > bounds.max_records
        || bounds.max_quarantine_bytes == 0
        || bounds.max_quarantine_records == 0
    {
        return Err(SpoolError::MetadataCorrupted);
    }
    Ok(())
}

fn validate_record_bounds(
    source: &[u8],
    payload: &[u8],
    bounds: SpoolBounds,
) -> Result<(), SpoolError> {
    if source.len() > bounds.max_source_bytes {
        return Err(SpoolError::Overflow(
            SpoolOverflowDisposition::SourceTooLarge,
        ));
    }
    if payload.len() > bounds.max_record_bytes {
        return Err(SpoolError::Overflow(
            SpoolOverflowDisposition::RecordTooLarge,
        ));
    }
    Ok(())
}

fn validate_meta_watermarks(meta: &SpoolMetaV1) -> Result<(), SpoolError> {
    if meta.committed_through == u64::MAX
        || meta.next_seq == 0
        || meta.next_seq <= meta.committed_through
    {
        return Err(SpoolError::MetadataCorrupted);
    }
    Ok(())
}

struct ScanResult {
    records: Vec<SpoolRecord>,
    truncate_to: u64,
    file_len: u64,
    integrity: SpoolIntegrity,
}

struct ParsedHeader {
    seq: u64,
    source_len: usize,
    payload_len: usize,
    framed_len: usize,
}

/// Stream frames one at a time. File size and header bounds are checked before
/// any source/payload allocation.
fn scan_records(
    path: &Path,
    bounds: SpoolBounds,
    quarantine: &TerminalQuarantine,
) -> Result<ScanResult, SpoolError> {
    if !path.exists() {
        return Ok(ScanResult {
            records: Vec::new(),
            truncate_to: 0,
            file_len: 0,
            integrity: SpoolIntegrity::Healthy,
        });
    }
    let file_len = file_len(path)?;
    if file_len > bounds.max_spool_bytes as u64 {
        return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes));
    }
    let mut input = File::open(path).map_err(io_error)?;
    let mut records = Vec::new();
    let mut offset = 0u64;
    let mut previous_seq = None;

    while offset < file_len {
        let remaining = file_len - offset;
        if remaining < FRAME_HEADER_BYTES as u64 {
            return Ok(partial_tail(records, offset, file_len));
        }
        let mut header = [0u8; FRAME_HEADER_BYTES];
        input.read_exact(&mut header).map_err(io_error)?;
        let parsed = match parse_header(&header, bounds) {
            Ok(parsed) => parsed,
            Err(SpoolError::UnsupportedVersion(version)) => {
                return Err(SpoolError::UnsupportedVersion(version));
            }
            Err(SpoolError::Corrupted { .. }) => {
                return Ok(corrupted_prefix(records, offset, file_len));
            }
            Err(error) => return Err(error),
        };
        if parsed.framed_len as u64 > remaining {
            return Ok(partial_tail(records, offset, file_len));
        }
        if records.len() >= bounds.max_records {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxRecords));
        }
        if parsed.seq == 0 || parsed.seq == u64::MAX {
            return Ok(corrupted_prefix(records, offset, file_len));
        }
        if let Some(previous) = previous_seq {
            if parsed.seq <= previous {
                return Ok(corrupted_prefix(records, offset, file_len));
            }
            let missing = parsed.seq - previous - 1;
            if missing > quarantine.len() as u64
                || (previous + 1..parsed.seq).any(|seq| !quarantine.contains(seq))
            {
                return Ok(corrupted_prefix(records, offset, file_len));
            }
        }

        let mut source = vec![0u8; parsed.source_len];
        let mut payload = vec![0u8; parsed.payload_len];
        let mut checksum = [0u8; CHECKSUM_BYTES];
        input.read_exact(&mut source).map_err(io_error)?;
        input.read_exact(&mut payload).map_err(io_error)?;
        input.read_exact(&mut checksum).map_err(io_error)?;

        let mut hasher = Sha256::new();
        hasher.update(header);
        hasher.update(&source);
        hasher.update(&payload);
        if hasher.finalize().as_slice() != checksum {
            return Ok(corrupted_prefix(records, offset, file_len));
        }
        let Ok(source) = String::from_utf8(source) else {
            return Ok(corrupted_prefix(records, offset, file_len));
        };
        previous_seq = Some(parsed.seq);
        records.push(SpoolRecord {
            seq: parsed.seq,
            source,
            payload,
            file_offset: offset,
            framed_len: parsed.framed_len,
        });
        offset += parsed.framed_len as u64;
    }

    Ok(ScanResult {
        records,
        truncate_to: file_len,
        file_len,
        integrity: SpoolIntegrity::Healthy,
    })
}

fn is_proven_unpublished_active_tail(
    path: &Path,
    scan: &ScanResult,
    meta: &SpoolMetaV1,
    quarantine: &TerminalQuarantine,
    bounds: SpoolBounds,
) -> Result<bool, SpoolError> {
    if !matches!(meta.integrity, SpoolIntegrity::Healthy) || scan.truncate_to >= scan.file_len {
        return Ok(false);
    }

    let Some(expected_evidence) = meta
        .next_seq
        .checked_sub(meta.committed_through)
        .and_then(|distance| distance.checked_sub(1))
    else {
        return Ok(false);
    };
    if expected_evidence
        > bounds
            .max_records
            .saturating_add(bounds.max_quarantine_records) as u64
        || scan
            .records
            .iter()
            .any(|record| record.seq >= meta.next_seq)
        || quarantine.iter().any(|(seq, _)| *seq >= meta.next_seq)
    {
        return Ok(false);
    }
    for seq in meta.committed_through + 1..meta.next_seq {
        if !quarantine.contains(seq)
            && scan
                .records
                .binary_search_by_key(&seq, |record| record.seq)
                .is_err()
        {
            return Ok(false);
        }
    }

    let tail_len = scan.file_len - scan.truncate_to;
    if tail_len < FRAME_HEADER_BYTES as u64 {
        return Ok(false);
    }
    let mut header = [0u8; FRAME_HEADER_BYTES];
    let mut input = File::open(path).map_err(io_error)?;
    input
        .seek(SeekFrom::Start(scan.truncate_to))
        .map_err(io_error)?;
    input.read_exact(&mut header).map_err(io_error)?;
    let parsed = parse_header(&header, bounds)?;
    if parsed.seq != meta.next_seq || parsed.framed_len as u64 <= tail_len {
        return Ok(false);
    }
    Ok(true)
}

fn parse_header(
    header: &[u8; FRAME_HEADER_BYTES],
    bounds: SpoolBounds,
) -> Result<ParsedHeader, SpoolError> {
    if &header[0..4] != FRAME_MAGIC {
        return Err(SpoolError::Corrupted { at_offset: 0 });
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != FORMAT_VERSION {
        return Err(SpoolError::UnsupportedVersion(version));
    }
    let source_len = u16::from_le_bytes([header[6], header[7]]) as usize;
    let payload_len = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
    if source_len > bounds.max_source_bytes || payload_len > bounds.max_record_bytes {
        return Err(SpoolError::Corrupted { at_offset: 0 });
    }
    let framed_len = FRAME_HEADER_BYTES
        .checked_add(source_len)
        .and_then(|len| len.checked_add(payload_len))
        .and_then(|len| len.checked_add(CHECKSUM_BYTES))
        .ok_or(SpoolError::Corrupted { at_offset: 0 })?;
    if framed_len > bounds.max_spool_bytes {
        return Err(SpoolError::Corrupted { at_offset: 0 });
    }
    Ok(ParsedHeader {
        seq: u64::from_le_bytes([
            header[12], header[13], header[14], header[15], header[16], header[17], header[18],
            header[19],
        ]),
        source_len,
        payload_len,
        framed_len,
    })
}

fn validate_quarantined_active_frame(seq: u64, frame: &[u8], bounds: SpoolBounds) -> bool {
    if frame.len() < FRAME_HEADER_BYTES + CHECKSUM_BYTES {
        return false;
    }
    let Ok(header) = <[u8; FRAME_HEADER_BYTES]>::try_from(&frame[..FRAME_HEADER_BYTES]) else {
        return false;
    };
    let Ok(parsed) = parse_header(&header, bounds) else {
        return false;
    };
    if parsed.seq != seq || parsed.framed_len != frame.len() {
        return false;
    }
    let checksum_at = frame.len() - CHECKSUM_BYTES;
    if Sha256::digest(&frame[..checksum_at]).as_slice() != &frame[checksum_at..] {
        return false;
    }
    let source_end = FRAME_HEADER_BYTES + parsed.source_len;
    std::str::from_utf8(&frame[FRAME_HEADER_BYTES..source_end]).is_ok()
}

struct PendingRecovery {
    pending: Vec<SpoolRecord>,
    pending_bytes: usize,
    recovered_next_seq: Option<u64>,
}

fn recover_pending(
    records: Vec<SpoolRecord>,
    quarantine: &TerminalQuarantine,
    meta: &SpoolMetaV1,
    bounds: SpoolBounds,
) -> Result<PendingRecovery, SpoolError> {
    for record in &records {
        if let Some(entry) = quarantine.entry(record.seq) {
            let active_frame = encode_frame(record.seq, record.source.as_bytes(), &record.payload)?;
            if entry.active_frame != active_frame {
                return Err(SpoolError::QuarantineCorrupted {
                    at_offset: record.file_offset,
                });
            }
        }
    }
    if let Some(first) = records
        .iter()
        .find(|record| record.seq > meta.committed_through)
        && first.seq > meta.committed_through + 1
    {
        let missing = first.seq - meta.committed_through - 1;
        if missing > quarantine.len() as u64
            || (meta.committed_through + 1..first.seq).any(|seq| !quarantine.contains(seq))
        {
            return Err(SpoolError::Corrupted {
                at_offset: first.file_offset,
            });
        }
    }

    let highest_unresolved = records
        .iter()
        .map(|record| record.seq)
        .chain(quarantine.iter().map(|(seq, _)| *seq))
        .filter(|seq| *seq > meta.committed_through)
        .max();
    let recovered_next_seq = if matches!(meta.integrity, SpoolIntegrity::Corrupted { .. }) {
        None
    } else {
        match highest_unresolved {
            Some(highest) if highest == meta.next_seq => {
                if quarantine.contains(highest)
                    || records.last().map(|record| record.seq) != Some(highest)
                {
                    return Err(SpoolError::MetadataCorrupted);
                }
                Some(
                    highest
                        .checked_add(1)
                        .ok_or(SpoolError::MetadataCorrupted)?,
                )
            }
            Some(highest) if highest.checked_add(1) == Some(meta.next_seq) => None,
            None if meta.next_seq == meta.committed_through + 1 => None,
            Some(_) | None => return Err(SpoolError::MetadataCorrupted),
        }
    };
    let effective_next_seq = recovered_next_seq.unwrap_or(meta.next_seq);
    if quarantine
        .iter()
        .any(|(seq, _)| *seq == 0 || *seq == u64::MAX || *seq >= effective_next_seq)
    {
        return Err(SpoolError::QuarantineCorrupted { at_offset: 0 });
    }

    if matches!(meta.integrity, SpoolIntegrity::Healthy) {
        let evidence_count = records
            .iter()
            .filter(|record| record.seq > meta.committed_through)
            .count()
            + quarantine
                .iter()
                .filter(|(seq, _)| {
                    **seq > meta.committed_through
                        && records
                            .binary_search_by_key(&**seq, |record| record.seq)
                            .is_err()
                })
                .count();
        let expected_count = effective_next_seq - meta.committed_through - 1;
        if expected_count > evidence_count as u64 {
            return Err(SpoolError::MetadataCorrupted);
        }
        for seq in meta.committed_through + 1..effective_next_seq {
            if !quarantine.contains(seq)
                && records
                    .binary_search_by_key(&seq, |record| record.seq)
                    .is_err()
            {
                return Err(SpoolError::Corrupted { at_offset: 0 });
            }
        }
    }

    let mut pending = Vec::new();
    let mut pending_bytes = 0usize;
    let mut pending_by_source = BTreeMap::<String, (usize, usize)>::new();
    for record in records {
        if record.seq <= meta.committed_through || quarantine.contains(record.seq) {
            continue;
        }
        if pending.len() >= bounds.max_records {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxRecords));
        }
        pending_bytes = pending_bytes
            .checked_add(record.framed_len)
            .ok_or(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes))?;
        if pending_bytes > bounds.max_spool_bytes {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes));
        }
        let source_usage = pending_by_source.entry(record.source.clone()).or_default();
        source_usage.0 += 1;
        source_usage.1 =
            source_usage
                .1
                .checked_add(record.framed_len)
                .ok_or(SpoolError::Overflow(
                    SpoolOverflowDisposition::MaxBytesPerSource,
                ))?;
        if source_usage.0 > bounds.max_records_per_source {
            return Err(SpoolError::Overflow(
                SpoolOverflowDisposition::MaxRecordsPerSource,
            ));
        }
        if source_usage.1 > bounds.max_spool_bytes_per_source {
            return Err(SpoolError::Overflow(
                SpoolOverflowDisposition::MaxBytesPerSource,
            ));
        }
        pending.push(record);
    }
    Ok(PendingRecovery {
        pending,
        pending_bytes,
        recovered_next_seq,
    })
}

fn encode_frame(seq: u64, source: &[u8], payload: &[u8]) -> Result<Vec<u8>, SpoolError> {
    if seq == 0 || seq == u64::MAX {
        return Err(SpoolError::MetadataCorrupted);
    }
    if source.len() > u16::MAX as usize {
        return Err(SpoolError::Overflow(
            SpoolOverflowDisposition::SourceTooLarge,
        ));
    }
    if payload.len() > u32::MAX as usize {
        return Err(SpoolError::Overflow(
            SpoolOverflowDisposition::RecordTooLarge,
        ));
    }
    let capacity = FRAME_HEADER_BYTES
        .checked_add(source.len())
        .and_then(|len| len.checked_add(payload.len()))
        .and_then(|len| len.checked_add(CHECKSUM_BYTES))
        .ok_or(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes))?;
    let mut frame = Vec::with_capacity(capacity);
    frame.extend_from_slice(FRAME_MAGIC);
    frame.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    frame.extend_from_slice(&(source.len() as u16).to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&seq.to_le_bytes());
    frame.extend_from_slice(source);
    frame.extend_from_slice(payload);
    frame.extend_from_slice(&Sha256::digest(&frame));
    Ok(frame)
}

fn append_frame_durable(path: &Path, frame: &[u8]) -> Result<u64, SpoolError> {
    tighten_existing_file(path)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(path).map_err(io_error)?;
    tighten_existing_file(path)?;
    let offset = output.seek(SeekFrom::End(0)).map_err(io_error)?;
    output.write_all(frame).map_err(io_error)?;
    output.sync_all().map_err(io_error)?;
    sync_parent_directory(path)?;
    Ok(offset)
}

fn read_meta(path: &Path) -> Result<Option<SpoolMetaV1>, SpoolError> {
    if !path.exists() {
        return Ok(None);
    }
    let len = file_len(path)?;
    if len == 0 || len > MAX_META_BYTES {
        return Err(SpoolError::MetadataCorrupted);
    }
    let mut bytes = Vec::with_capacity(len as usize);
    File::open(path)
        .map_err(io_error)?
        .take(MAX_META_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| SpoolError::MetadataCorrupted)
}

fn write_meta_atomic(path: &Path, meta: &SpoolMetaV1) -> Result<(), SpoolError> {
    #[cfg(test)]
    {
        let mut failure = FAIL_META_WRITE_FOR.lock().map_err(|_| SpoolError::Io)?;
        if failure.as_deref() == Some(path) {
            *failure = None;
            return Err(SpoolError::Io);
        }
    }
    let bytes = serde_json::to_vec(meta).map_err(|_| SpoolError::MetadataCorrupted)?;
    with_owned_temp_publish(path, "meta", "host admission spool metadata", |output| {
        output.write_all(&bytes).map_err(io_error)?;
        Ok(())
    })
}

fn truncate_file(path: &Path, len: u64) -> Result<(), SpoolError> {
    tighten_existing_file(path)?;
    let output = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(io_error)?;
    output.set_len(len).map_err(io_error)?;
    output.sync_all().map_err(io_error)?;
    tighten_existing_file(path)?;
    sync_parent_directory(path)
}

fn partial_tail(records: Vec<SpoolRecord>, offset: u64, file_len: u64) -> ScanResult {
    ScanResult {
        records,
        truncate_to: offset,
        file_len,
        integrity: SpoolIntegrity::Healthy,
    }
}

fn corrupted_prefix(records: Vec<SpoolRecord>, offset: u64, file_len: u64) -> ScanResult {
    // `truncate_to` marks the last valid frame boundary for reporting only.
    // Open must not set_len here: the corrupted suffix is preserved read-only.
    ScanResult {
        records,
        truncate_to: offset,
        file_len,
        integrity: SpoolIntegrity::Corrupted { at_offset: offset },
    }
}

fn replace_file_atomically(
    temporary: &Path,
    destination: &Path,
    label: &str,
) -> Result<(), SpoolError> {
    crate::db::DatabaseAuthority::replace_file_atomically(temporary, destination, label)
        .map_err(|_| SpoolError::Io)
}

fn create_owned_temp(destination: &Path, kind: &str) -> Result<(PathBuf, File), SpoolError> {
    for _ in 0..64 {
        let path = temporary_path(destination, kind);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Err(SpoolError::Io)
}

fn with_owned_temp_publish<T>(
    destination: &Path,
    kind: &str,
    label: &str,
    write: impl FnOnce(&mut File) -> Result<T, SpoolError>,
) -> Result<T, SpoolError> {
    let (temporary, mut output) = create_owned_temp(destination, kind)?;
    let publish = (|| {
        let value = write(&mut output)?;
        output.sync_all().map_err(io_error)?;
        drop(output);
        replace_file_atomically(&temporary, destination, label)?;
        tighten_existing_file(destination)?;
        sync_parent_directory(destination)?;
        Ok(value)
    })();
    if publish.is_err() {
        remove_owned_temp(&temporary);
    }
    publish
}

fn tighten_existing_file(path: &Path) -> Result<(), SpoolError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    if !metadata.file_type().is_file() {
        return Err(SpoolError::Io);
    }
    set_private_file_permissions(path)
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), SpoolError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io_error)
}

#[cfg(not(unix))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the cross-platform permission hook shares a fallible call contract"
)]
fn set_private_file_permissions(_path: &Path) -> Result<(), SpoolError> {
    // This matches the repository's current private-store convention on
    // non-Unix hosts; no ad-hoc ACL implementation is introduced here.
    Ok(())
}

fn remove_owned_temp(path: &Path) {
    let _ = fs::remove_file(path);
}

fn temporary_path(path: &Path, kind: &str) -> PathBuf {
    static NONCE: AtomicU64 = AtomicU64::new(1);
    let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(
        ".{}.{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("spool"),
        kind,
        std::process::id(),
        nonce
    ))
}

fn file_len(path: &Path) -> Result<u64, SpoolError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(io_error(error)),
    }
}

fn sync_parent_directory(path: &Path) -> Result<(), SpoolError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    match File::open(parent).and_then(|directory| directory.sync_all()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput || cfg!(windows) => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn io_error(_error: impl ToString) -> SpoolError {
    SpoolError::Io
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

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
        drop(spool);
        let records = temp.path().join(RECORDS_FILE);
        let mut bytes = fs::read(&records).unwrap();
        let unpublished = encode_frame(2, b"a", b"partial").unwrap();
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
    fn unproven_active_tail_is_preserved_as_forensic_corruption() {
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
        let bounded =
            SpoolBounds::new(64, 16, frame_len * 4, 4).with_source_limits(frame_len * 2, 2);
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
        drop(spool);
        let frame = encode_frame(1, b"a", b"crash-window").unwrap();
        append_frame_durable(&temp.path().join(RECORDS_FILE), &frame).unwrap();
        let (spool, report) = HostAdmissionSpool::open(temp.path(), bounds()).unwrap();
        assert_eq!(report.next_seq, 2);
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
        let old_meta = fs::read(&meta_path).unwrap();
        let before_second = fs::read(&records_path).unwrap();
        *FAIL_META_WRITE_FOR.lock().unwrap() = Some(meta_path.clone());

        assert_eq!(spool.append("b", b"two"), Err(SpoolError::Io));
        assert!(spool.recovery_required());
        let ambiguous_bytes = fs::read(&records_path).unwrap();
        assert!(ambiguous_bytes.len() > before_second.len());
        assert_eq!(fs::read(&meta_path).unwrap(), old_meta);
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
        const N: usize = 4096;
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
}
