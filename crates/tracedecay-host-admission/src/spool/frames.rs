use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use tracedecay_domain::framed_log::{self, checksum};

use super::bounds::{SpoolBounds, SpoolOverflowDisposition};
use super::fs_ops::{self, file_len, io_error};
use super::meta::SpoolMetaV1;
use super::quarantine::TerminalQuarantine;
use super::types::{SpoolError, SpoolIntegrity, SpoolRecord};

pub(crate) const FRAME_MAGIC: &[u8; 4] = b"TDHA";

pub(crate) const FORMAT_VERSION: u16 = 1;

pub(crate) const FRAME_HEADER_BYTES: usize = 20;

pub(crate) use framed_log::CHECKSUM_BYTES;

pub(crate) struct ParsedHeader {
    pub(crate) seq: u64,
    pub(crate) source_len: usize,
    pub(crate) payload_len: usize,
    pub(crate) framed_len: usize,
}

pub(crate) fn parse_header(
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

pub(crate) fn encode_frame(seq: u64, source: &[u8], payload: &[u8]) -> Result<Vec<u8>, SpoolError> {
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
    frame.extend_from_slice(&checksum(&frame));
    Ok(frame)
}

pub(crate) use fs_ops::append_frame_durable;

pub(crate) struct ScanResult {
    pub(crate) records: Vec<SpoolRecord>,
    pub(crate) truncate_to: u64,
    pub(crate) file_len: u64,
    pub(crate) integrity: SpoolIntegrity,
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

pub(crate) fn validate_quarantined_active_frame(
    seq: u64,
    frame: &[u8],
    bounds: SpoolBounds,
) -> bool {
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
    if checksum(&frame[..checksum_at]) != frame[checksum_at..] {
        return false;
    }
    let source_end = FRAME_HEADER_BYTES + parsed.source_len;
    std::str::from_utf8(&frame[FRAME_HEADER_BYTES..source_end]).is_ok()
}

/// Stream frames one at a time. File size and header bounds are checked before
/// any source/payload allocation.
pub(crate) fn scan_records(
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
        let mut stored_checksum = [0u8; CHECKSUM_BYTES];
        input.read_exact(&mut source).map_err(io_error)?;
        input.read_exact(&mut payload).map_err(io_error)?;
        input.read_exact(&mut stored_checksum).map_err(io_error)?;

        let mut body = Vec::with_capacity(
            FRAME_HEADER_BYTES
                .checked_add(parsed.source_len)
                .and_then(|len| len.checked_add(parsed.payload_len))
                .unwrap_or(FRAME_HEADER_BYTES),
        );
        body.extend_from_slice(&header);
        body.extend_from_slice(&source);
        body.extend_from_slice(&payload);
        if checksum(&body) != stored_checksum {
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

pub(crate) fn is_proven_unpublished_active_tail(
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

    let Some(intent) = &meta.append_intent else {
        return Ok(false);
    };
    let tail_len = scan.file_len - scan.truncate_to;
    if intent.file_offset != scan.truncate_to || intent.framed_len <= tail_len {
        return Ok(false);
    }
    let mut input = File::open(path).map_err(io_error)?;
    input
        .seek(SeekFrom::Start(scan.truncate_to))
        .map_err(io_error)?;
    let prefix_len = (tail_len as usize).min(FRAME_HEADER_BYTES);
    let mut header_prefix = [0u8; FRAME_HEADER_BYTES];
    input
        .read_exact(&mut header_prefix[..prefix_len])
        .map_err(io_error)?;
    if header_prefix[..prefix_len] != intent.header[..prefix_len] {
        return Ok(false);
    }
    Ok(true)
}
