pub(crate) const DEFAULT_MAX_RECORD_BYTES: usize = 1024 * 1024;

pub(crate) const DEFAULT_MAX_SOURCE_BYTES: usize = 256;

pub(crate) const DEFAULT_MAX_SPOOL_BYTES: usize = 16 * 1024 * 1024;

pub(crate) const DEFAULT_MAX_RECORDS: usize = 4096;

pub(crate) const DEFAULT_MAX_SPOOL_BYTES_PER_SOURCE: usize = 4 * 1024 * 1024;

use super::frames::{CHECKSUM_BYTES, FRAME_HEADER_BYTES};
use super::quarantine::FRAME_OVERHEAD as QUARANTINE_FRAME_OVERHEAD;
use super::types::SpoolError;

pub(crate) const DEFAULT_MAX_RECORDS_PER_SOURCE: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub struct SpoolBounds {
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
    pub const fn new(
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

    pub const fn with_source_limits(
        mut self,
        max_spool_bytes_per_source: usize,
        max_records_per_source: usize,
    ) -> Self {
        self.max_spool_bytes_per_source = max_spool_bytes_per_source;
        self.max_records_per_source = max_records_per_source;
        self
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub const fn with_quarantine_limits(
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

pub(crate) fn validate_bounds(bounds: SpoolBounds) -> Result<(), SpoolError> {
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

pub(crate) fn validate_record_bounds(
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
