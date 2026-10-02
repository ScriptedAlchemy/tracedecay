use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracedecay_domain::framed_log::checksum;

use super::bounds::SpoolBounds;
use super::frames::{
    CHECKSUM_BYTES, FORMAT_VERSION, FRAME_HEADER_BYTES, ScanResult, encode_frame, parse_header,
};
use super::fs_ops::{file_len, io_error, with_owned_temp_publish};
use super::types::{SpoolError, SpoolIntegrity, SpoolRecord};

pub(crate) const META_FILE: &str = "meta.json";
pub(crate) const MAX_META_BYTES: u64 = 4096;

#[cfg(test)]
pub(crate) static FAIL_META_WRITE_FOR: std::sync::Mutex<Option<(std::path::PathBuf, usize)>> =
    std::sync::Mutex::new(None);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SpoolMetaV1 {
    pub(crate) version: u16,
    pub(crate) committed_through: u64,
    pub(crate) next_seq: u64,
    pub(crate) integrity: SpoolIntegrity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) append_intent: Option<AppendIntentV1>,
}

/// One group-commit batch whose frames may be on disk before the metadata
/// that names them. `header` is the first frame's header and `checksum`
/// covers the whole batch's bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppendIntentV1 {
    pub(crate) seq: u64,
    pub(crate) records: u64,
    pub(crate) file_offset: u64,
    pub(crate) framed_len: u64,
    pub(crate) header: [u8; FRAME_HEADER_BYTES],
    pub(crate) checksum: [u8; CHECKSUM_BYTES],
}

impl SpoolMetaV1 {
    pub(crate) fn fresh() -> Self {
        Self {
            version: FORMAT_VERSION,
            committed_through: 0,
            next_seq: 1,
            integrity: SpoolIntegrity::Healthy,
            append_intent: None,
        }
    }
}

impl AppendIntentV1 {
    /// `batch` is `records` consecutive frames starting at `seq`.
    pub(crate) fn new(seq: u64, file_offset: u64, records: u64, batch: &[u8]) -> Self {
        let mut header = [0u8; FRAME_HEADER_BYTES];
        header.copy_from_slice(&batch[..FRAME_HEADER_BYTES]);
        Self {
            seq,
            records,
            file_offset,
            framed_len: batch.len() as u64,
            header,
            checksum: checksum(batch),
        }
    }

    /// One past the last sequence the batch names.
    pub(crate) fn end_seq(&self) -> u64 {
        self.seq.saturating_add(self.records)
    }

    /// One past the last byte the batch names.
    pub(crate) fn end_offset(&self) -> u64 {
        self.file_offset.saturating_add(self.framed_len)
    }

    /// The scanned records that belong to this batch, which must be the
    /// consecutive tail of the scan starting at the batch's first frame.
    pub(crate) fn written_prefix<'a>(
        &self,
        records: &'a [SpoolRecord],
    ) -> Option<&'a [SpoolRecord]> {
        let written = &records[records.partition_point(|record| record.seq < self.seq)..];
        let consecutive = written.len() as u64 <= self.records
            && written
                .iter()
                .zip(self.seq..)
                .all(|(record, seq)| record.seq == seq);
        let anchored = written.first().is_none_or(|first| {
            first.file_offset == self.file_offset
                && encode_frame(first.seq, first.source.as_bytes(), &first.payload)
                    .is_ok_and(|frame| frame[..FRAME_HEADER_BYTES] == self.header)
        });
        (consecutive && anchored).then_some(written)
    }

    fn matches_batch(&self, written: &[SpoolRecord]) -> Result<bool, SpoolError> {
        let mut batch = Vec::new();
        for record in written {
            batch.extend_from_slice(&encode_frame(
                record.seq,
                record.source.as_bytes(),
                &record.payload,
            )?);
        }
        Ok(batch.len() as u64 == self.framed_len && checksum(&batch) == self.checksum)
    }
}

pub(crate) fn validate_meta_watermarks(meta: &SpoolMetaV1) -> Result<(), SpoolError> {
    if meta.committed_through == u64::MAX
        || meta.next_seq == 0
        || meta.next_seq <= meta.committed_through
    {
        return Err(SpoolError::MetadataCorrupted);
    }
    Ok(())
}

pub(crate) fn validate_append_intent(
    meta: &SpoolMetaV1,
    bounds: SpoolBounds,
) -> Result<(), SpoolError> {
    let Some(intent) = &meta.append_intent else {
        return Ok(());
    };
    let parsed = parse_header(&intent.header, bounds).map_err(|_| SpoolError::MetadataCorrupted)?;
    let first_len = parsed.framed_len as u64;
    if intent.seq != meta.next_seq
        || parsed.seq != intent.seq
        || intent.records == 0
        || intent.records > bounds.max_records as u64
        || intent
            .seq
            .checked_add(intent.records)
            .is_none_or(|end| end == u64::MAX)
        || first_len > intent.framed_len
        || (intent.records == 1 && first_len != intent.framed_len)
        || intent
            .file_offset
            .checked_add(intent.framed_len)
            .is_none_or(|end| end > bounds.max_spool_bytes as u64)
    {
        return Err(SpoolError::MetadataCorrupted);
    }
    Ok(())
}

pub(crate) fn append_intent_is_reconciled(
    scan: &ScanResult,
    meta: &SpoolMetaV1,
    truncated_partial_tail_bytes: u64,
) -> Result<bool, SpoolError> {
    let Some(intent) = &meta.append_intent else {
        return Ok(false);
    };
    if !matches!(scan.integrity, SpoolIntegrity::Healthy) {
        return Ok(false);
    }
    if truncated_partial_tail_bytes > 0 {
        // Open already proved the torn tail lies inside this batch.
        return Ok(intent.file_offset <= scan.truncate_to && scan.truncate_to < intent.end_offset());
    }
    let Some(written) = intent.written_prefix(&scan.records) else {
        return Err(SpoolError::MetadataCorrupted);
    };
    let Some(last) = written.last() else {
        return if scan.file_len == intent.file_offset {
            Ok(true)
        } else {
            Err(SpoolError::MetadataCorrupted)
        };
    };
    let written_end = last.file_offset + last.framed_len as u64;
    // A crash may persist a frame-aligned prefix of the batch; its complete
    // frames were never acknowledged and stay pending for replay.
    let consistent = if written.len() as u64 == intent.records {
        written_end == intent.end_offset() && intent.matches_batch(written)?
    } else {
        written_end < intent.end_offset()
    };
    if !consistent || written_end != scan.file_len {
        return Err(SpoolError::MetadataCorrupted);
    }
    Ok(true)
}

pub(crate) fn read_meta(path: &Path) -> Result<Option<SpoolMetaV1>, SpoolError> {
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

pub(crate) fn write_meta_atomic(path: &Path, meta: &SpoolMetaV1) -> Result<(), SpoolError> {
    #[cfg(test)]
    {
        let mut failure = FAIL_META_WRITE_FOR.lock().map_err(|_| SpoolError::Io)?;
        let should_fail = match failure.as_mut() {
            Some((failure_path, writes_before_failure)) if failure_path == path => {
                if *writes_before_failure == 0 {
                    true
                } else {
                    *writes_before_failure -= 1;
                    false
                }
            }
            _ => false,
        };
        if should_fail {
            *failure = None;
            return Err(SpoolError::Io);
        }
    }
    let bytes = serde_json::to_vec(meta).map_err(|_| SpoolError::MetadataCorrupted)?;
    hotpath::measure_block!("usecases.admission.fsync.meta", {
        with_owned_temp_publish(path, "meta", "host admission spool metadata", |output| {
            output.write_all(&bytes).map_err(io_error)?;
            Ok(())
        })
    })
}
