use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppendIntentV1 {
    pub(crate) seq: u64,
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
    pub(crate) fn new(seq: u64, file_offset: u64, frame: &[u8]) -> Self {
        let mut header = [0u8; FRAME_HEADER_BYTES];
        header.copy_from_slice(&frame[..FRAME_HEADER_BYTES]);
        let mut checksum = [0u8; CHECKSUM_BYTES];
        checksum.copy_from_slice(&frame[frame.len() - CHECKSUM_BYTES..]);
        Self {
            seq,
            file_offset,
            framed_len: frame.len() as u64,
            header,
            checksum,
        }
    }

    pub(crate) fn matches_record(&self, record: &SpoolRecord) -> Result<bool, SpoolError> {
        let frame = encode_frame(record.seq, record.source.as_bytes(), &record.payload)?;
        Ok(self.seq == record.seq
            && self.file_offset == record.file_offset
            && self.framed_len == record.framed_len as u64
            && self.header.as_slice() == &frame[..FRAME_HEADER_BYTES]
            && self.checksum.as_slice() == &frame[frame.len() - CHECKSUM_BYTES..])
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
    if intent.seq != meta.next_seq
        || parsed.seq != intent.seq
        || parsed.framed_len as u64 != intent.framed_len
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
        return Ok(intent.file_offset == scan.truncate_to);
    }
    if scan.file_len == intent.file_offset {
        return Ok(true);
    }
    let Some(record) = scan.records.iter().find(|record| record.seq == intent.seq) else {
        return Err(SpoolError::MetadataCorrupted);
    };
    if scan.records.last().map(|record| record.seq) != Some(intent.seq)
        || intent.file_offset.checked_add(intent.framed_len) != Some(scan.file_len)
        || !intent.matches_record(record)?
    {
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
    with_owned_temp_publish(path, "meta", "host admission spool metadata", |output| {
        output.write_all(&bytes).map_err(io_error)?;
        Ok(())
    })
}
