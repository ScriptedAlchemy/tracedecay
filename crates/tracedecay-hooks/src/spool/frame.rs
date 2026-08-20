use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use tracedecay_domain::{UtcMicros, framed_log::checksum as frame_checksum};
use tracedecay_private_fs::framed_log::{append_durable, truncate_file as shared_truncate_file};

use crate::{HOOK_EVENT_SCHEMA_VERSION, HookEventEnvelopeV2, HookHostV1, MAX_HOOK_PAYLOAD_BYTES};
use serde_json::Value;

use super::types::{HookSpoolRecordV1, ScanResult};
use super::{
    DIRECTORY_POLICY, FRAME_CHECKSUM_BYTES, FRAME_HEADER_BYTES, FRAME_LENGTH_BYTES,
    HookSpoolConfigV1, HookSpoolError, SPOOL_FORMAT_VERSION, SPOOL_MAGIC, records_path,
    validate_regular_or_missing,
};

pub(super) fn append_frame(path: &Path, frame: &[u8]) -> Result<(), HookSpoolError> {
    append_durable(path, frame, DIRECTORY_POLICY)
        .map(|_| ())
        .map_err(|_| HookSpoolError::Io)
}

pub(super) fn truncate_records(root: &Path, length: u64) -> Result<(), HookSpoolError> {
    shared_truncate_file(&records_path(root), length, DIRECTORY_POLICY)
        .map_err(|_| HookSpoolError::Io)
}

pub(super) fn scan_records(
    root: &Path,
    config: HookSpoolConfigV1,
) -> Result<ScanResult, HookSpoolError> {
    let path = records_path(root);
    if !validate_regular_or_missing(&path)? {
        return Ok(ScanResult {
            records: Vec::new(),
            valid_end: 0,
            physical_len: 0,
            partial_tail: None,
            corruption: None,
        });
    }
    let physical_len = fs::metadata(&path).map_err(|_| HookSpoolError::Io)?.len();
    if physical_len > config.limits.max_host_bytes {
        return Err(HookSpoolError::SpoolFull);
    }
    let mut file = File::open(&path).map_err(|_| HookSpoolError::Io)?;
    let mut records = Vec::new();
    let mut offset = 0u64;
    let mut previous_sequence = None;
    while offset < physical_len {
        let remaining = physical_len - offset;
        if remaining < FRAME_LENGTH_BYTES as u64 {
            return partial_scan(records, offset, physical_len, &mut file);
        }
        let mut prefix = [0u8; FRAME_LENGTH_BYTES];
        file.read_exact(&mut prefix)
            .map_err(|_| HookSpoolError::Io)?;
        let declared = u32::from_le_bytes(prefix) as usize;
        let minimum = FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES;
        let maximum = minimum
            .checked_add(MAX_HOOK_PAYLOAD_BYTES)
            .ok_or(HookSpoolError::MetadataCorrupted)?;
        if declared < minimum || declared > maximum {
            return Ok(corrupt_scan(records, offset, physical_len));
        }
        let frame_len = FRAME_LENGTH_BYTES
            .checked_add(declared)
            .ok_or(HookSpoolError::MetadataCorrupted)?;
        if frame_len as u64 > remaining {
            file.seek(SeekFrom::Start(offset))
                .map_err(|_| HookSpoolError::Io)?;
            return partial_scan(records, offset, physical_len, &mut file);
        }
        let mut frame = Vec::with_capacity(frame_len);
        frame.extend_from_slice(&prefix);
        let mut body = vec![0u8; declared];
        file.read_exact(&mut body).map_err(|_| HookSpoolError::Io)?;
        frame.extend_from_slice(&body);
        let record = match decode_complete_frame(&frame, offset, config.host) {
            Ok(record) => record,
            Err(HookSpoolError::Corrupted { .. }) | Err(HookSpoolError::MetadataCorrupted) => {
                return Ok(corrupt_scan(records, offset, physical_len));
            }
            Err(error) => return Err(error),
        };
        if previous_sequence.is_some_and(|previous| record.sequence <= previous)
            || records.len() >= config.limits.max_host_records as usize
        {
            return Ok(corrupt_scan(records, offset, physical_len));
        }
        previous_sequence = Some(record.sequence);
        offset = offset.saturating_add(u64::from(record.framed_len));
        records.push(record);
    }
    Ok(ScanResult {
        records,
        valid_end: offset,
        physical_len,
        partial_tail: None,
        corruption: None,
    })
}

pub(super) fn partial_scan(
    records: Vec<HookSpoolRecordV1>,
    offset: u64,
    physical_len: u64,
    file: &mut File,
) -> Result<ScanResult, HookSpoolError> {
    let mut partial_tail = Vec::with_capacity((physical_len - offset) as usize);
    file.read_to_end(&mut partial_tail)
        .map_err(|_| HookSpoolError::Io)?;
    Ok(ScanResult {
        records,
        valid_end: offset,
        physical_len,
        partial_tail: Some(partial_tail),
        corruption: None,
    })
}

pub(super) fn corrupt_scan(
    records: Vec<HookSpoolRecordV1>,
    offset: u64,
    physical_len: u64,
) -> ScanResult {
    ScanResult {
        records,
        valid_end: offset,
        physical_len,
        partial_tail: None,
        corruption: Some(offset),
    }
}

pub(super) fn encode_frame(
    sequence: u64,
    queued_at: UtcMicros,
    protected_session_id: [u8; 32],
    payload: &[u8],
) -> Result<Vec<u8>, HookSpoolError> {
    if sequence == 0 || payload.is_empty() || payload.len() > MAX_HOOK_PAYLOAD_BYTES {
        return Err(HookSpoolError::RecordTooLarge);
    }
    let body_len = FRAME_HEADER_BYTES
        .checked_add(payload.len())
        .and_then(|length| length.checked_add(FRAME_CHECKSUM_BYTES))
        .ok_or(HookSpoolError::RecordTooLarge)?;
    let body_len = u32::try_from(body_len).map_err(|_| HookSpoolError::RecordTooLarge)?;
    let mut frame = Vec::with_capacity(FRAME_LENGTH_BYTES + body_len as usize);
    frame.extend_from_slice(&body_len.to_le_bytes());
    frame.extend_from_slice(SPOOL_MAGIC);
    frame.extend_from_slice(&SPOOL_FORMAT_VERSION.to_le_bytes());
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&queued_at.0.to_le_bytes());
    frame.extend_from_slice(&protected_session_id);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    let checksum = frame_checksum(&frame);
    frame.extend_from_slice(&checksum);
    Ok(frame)
}

pub(super) fn decode_complete_frame(
    frame: &[u8],
    file_offset: u64,
    host: HookHostV1,
) -> Result<HookSpoolRecordV1, HookSpoolError> {
    let minimum = FRAME_LENGTH_BYTES + FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES;
    if frame.len() < minimum {
        return Err(HookSpoolError::Corrupted {
            at_offset: file_offset,
        });
    }
    let declared = u32::from_le_bytes(
        frame[..4]
            .try_into()
            .map_err(|_| HookSpoolError::MetadataCorrupted)?,
    ) as usize;
    if declared + FRAME_LENGTH_BYTES != frame.len() {
        return Err(HookSpoolError::Corrupted {
            at_offset: file_offset,
        });
    }
    let found_magic: [u8; 4] = frame[4..8]
        .try_into()
        .map_err(|_| HookSpoolError::MetadataCorrupted)?;
    let found_version = u16::from_le_bytes([frame[8], frame[9]]);
    if found_magic != *SPOOL_MAGIC || found_version != SPOOL_FORMAT_VERSION {
        return Err(HookSpoolError::ResetRequired {
            reason: super::HookSpoolResetReasonV1::FrameFormat {
                found_magic,
                found_version,
                expected_magic: *SPOOL_MAGIC,
                expected_version: SPOOL_FORMAT_VERSION,
            },
        });
    }
    let checksum_at = frame.len() - FRAME_CHECKSUM_BYTES;
    let checksum: [u8; 32] = frame[checksum_at..]
        .try_into()
        .map_err(|_| HookSpoolError::MetadataCorrupted)?;
    if frame_checksum(&frame[..checksum_at]) != checksum {
        return Err(HookSpoolError::Corrupted {
            at_offset: file_offset,
        });
    }
    let sequence = u64::from_le_bytes(
        frame[10..18]
            .try_into()
            .map_err(|_| HookSpoolError::MetadataCorrupted)?,
    );
    let queued_at = UtcMicros(i64::from_le_bytes(
        frame[18..26]
            .try_into()
            .map_err(|_| HookSpoolError::MetadataCorrupted)?,
    ));
    let protected_session_id = frame[26..58]
        .try_into()
        .map_err(|_| HookSpoolError::MetadataCorrupted)?;
    let payload_len = u32::from_le_bytes(
        frame[58..62]
            .try_into()
            .map_err(|_| HookSpoolError::MetadataCorrupted)?,
    ) as usize;
    if sequence == 0
        || payload_len == 0
        || payload_len > MAX_HOOK_PAYLOAD_BYTES
        || 62usize.saturating_add(payload_len) != checksum_at
    {
        return Err(HookSpoolError::Corrupted {
            at_offset: file_offset,
        });
    }
    let payload = &frame[62..checksum_at];
    let envelope = decode_exact_envelope(payload, file_offset)?;
    if envelope.producer != host || envelope.protected_session_id != protected_session_id {
        return Err(HookSpoolError::Corrupted {
            at_offset: file_offset,
        });
    }
    Ok(HookSpoolRecordV1 {
        sequence,
        protected_session_id,
        queued_at,
        envelope,
        encoded_len: u32::try_from(payload_len).map_err(|_| HookSpoolError::MetadataCorrupted)?,
        checksum,
        framed_len: u32::try_from(frame.len()).map_err(|_| HookSpoolError::MetadataCorrupted)?,
    })
}

fn decode_exact_envelope(
    payload: &[u8],
    file_offset: u64,
) -> Result<HookEventEnvelopeV2, HookSpoolError> {
    let value: Value = serde_json::from_slice(payload).map_err(|_| HookSpoolError::Corrupted {
        at_offset: file_offset,
    })?;
    let Some(found) = value.get("schema_version").and_then(Value::as_u64) else {
        return Err(HookSpoolError::ResetRequired {
            reason: super::HookSpoolResetReasonV1::EnvelopeShape,
        });
    };
    let found = u16::try_from(found).map_err(|_| HookSpoolError::ResetRequired {
        reason: super::HookSpoolResetReasonV1::EnvelopeShape,
    })?;
    if found != HOOK_EVENT_SCHEMA_VERSION {
        return Err(HookSpoolError::ResetRequired {
            reason: super::HookSpoolResetReasonV1::EnvelopeVersion {
                found,
                expected: HOOK_EVENT_SCHEMA_VERSION,
            },
        });
    }
    serde_json::from_value(value).map_err(|_| HookSpoolError::ResetRequired {
        reason: super::HookSpoolResetReasonV1::EnvelopeShape,
    })
}
