use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::{FileExt, MetadataExt};
#[cfg(windows)]
use std::os::windows::fs::{FileExt, MetadataExt};
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    canonical_json_bytes,
    framed_log::{self, checksum as frame_checksum},
};
use tracedecay_private_fs::framed_log::atomic_write as shared_atomic_write;

use crate::{
    HookHostV1, MAX_HOOK_PAYLOAD_BYTES, MAX_SPOOL_BYTES_PER_HOST, MAX_SPOOL_RECORDS_PER_HOST,
};

use super::{
    CHECKPOINT_FILE, CHECKPOINT_FORMAT_VERSION, DIRECTORY_POLICY, HookSpoolConfigV1,
    HookSpoolError, TRANSITION_FILE, checkpoint_path, read_bounded, records_path, transition_path,
    types::PendingRecordV1, validate_regular_or_missing,
};

const MAX_CHECKPOINT_BYTES: usize = (MAX_SPOOL_BYTES_PER_HOST as usize) * 4;
const MAX_TRANSITION_BYTES: usize = 16 * 1024;
pub(super) const CHECKPOINT_MAGIC: &[u8; 4] = b"TDHC";
pub(super) const CHECKPOINT_HEADER_BYTES: usize = 4 + 2 + 8;
pub(super) const CHECKPOINT_ENTRY_BYTES: usize = 100;
// Bound suffix validation by both ordinary frame count and unusually large
// payload bytes while amortizing each full checkpoint publication.
pub(super) const CHECKPOINT_REWRITE_FRAME_THRESHOLD: u32 = 64;
pub(super) const CHECKPOINT_REWRITE_BYTE_THRESHOLD: u64 = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecordsFileRevisionV1 {
    #[serde(with = "revision_identity")]
    pub(super) identity: [u8; 32],
    #[serde(with = "revision_length")]
    pub(super) length: u64,
    #[serde(with = "revision_time")]
    pub(super) modified: [i64; 2],
    #[serde(with = "revision_time")]
    pub(super) changed: [i64; 2],
}

mod revision_identity {
    use super::{DeError, Deserialize, Deserializer, Serializer};

    const HEX: &[u8; 16] = b"0123456789abcdef";

    pub(super) fn serialize<S: Serializer>(
        identity: &[u8; 32],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut encoded = String::with_capacity(64);
        for byte in identity {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        serializer.serialize_str(&encoded)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; 32], D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() != 64 {
            return Err(D::Error::custom("expected 64 hexadecimal identity digits"));
        }
        let mut identity = [0u8; 32];
        for (index, output) in identity.iter_mut().enumerate() {
            let at = index * 2;
            *output = u8::from_str_radix(&encoded[at..at + 2], 16).map_err(D::Error::custom)?;
        }
        Ok(identity)
    }
}

mod revision_length {
    use super::{DeError, Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(length: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{length:016x}"))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() != 16 {
            return Err(D::Error::custom("expected 16 hexadecimal length digits"));
        }
        u64::from_str_radix(&encoded, 16).map_err(D::Error::custom)
    }
}

mod revision_time {
    use super::{DeError, Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        time: &[i64; 2],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!(
            "{:016x}:{:016x}",
            u64::from_ne_bytes(time[0].to_ne_bytes()),
            u64::from_ne_bytes(time[1].to_ne_bytes())
        ))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[i64; 2], D::Error> {
        let encoded = String::deserialize(deserializer)?;
        let Some((seconds, subsecond)) = encoded.split_once(':') else {
            return Err(D::Error::custom("expected two hexadecimal time values"));
        };
        if seconds.len() != 16 || subsecond.len() != 16 {
            return Err(D::Error::custom("expected fixed-width time values"));
        }
        Ok([
            i64::from_ne_bytes(
                u64::from_str_radix(seconds, 16)
                    .map_err(D::Error::custom)?
                    .to_ne_bytes(),
            ),
            i64::from_ne_bytes(
                u64::from_str_radix(subsecond, 16)
                    .map_err(D::Error::custom)?
                    .to_ne_bytes(),
            ),
        ])
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookSpoolCheckpointHeaderV1 {
    version: u16,
    host: HookHostV1,
    records_revision: Option<RecordsFileRevisionV1>,
    record_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ValidatedCheckpointV1 {
    pub(super) records_revision: Option<RecordsFileRevisionV1>,
    pub(super) records: Vec<PendingRecordV1>,
    pub(super) checksum: [u8; 32],
    pub(super) bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CheckpointAnchorV1 {
    pub(super) records_revision: Option<RecordsFileRevisionV1>,
    pub(super) checksum: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookSpoolTransitionBodyV1 {
    version: u16,
    checkpoint_checksum: [u8; 32],
    checkpoint_revision: Option<RecordsFileRevisionV1>,
    current_revision: RecordsFileRevisionV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookSpoolTransitionFileV1 {
    body: HookSpoolTransitionBodyV1,
    checksum: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ValidatedTransitionV1 {
    pub(super) checkpoint_checksum: [u8; 32],
    pub(super) checkpoint_revision: Option<RecordsFileRevisionV1>,
    pub(super) current_revision: RecordsFileRevisionV1,
}

pub(super) fn read_checkpoint(
    root: &Path,
    config: HookSpoolConfigV1,
) -> Result<Option<ValidatedCheckpointV1>, HookSpoolError> {
    let Some(bytes) = read_accelerator(&checkpoint_path(root), MAX_CHECKPOINT_BYTES)? else {
        return Ok(None);
    };
    let minimum = CHECKPOINT_HEADER_BYTES + framed_log::CHECKSUM_BYTES;
    if bytes.len() < minimum
        || &bytes[..4] != CHECKPOINT_MAGIC
        || u16::from_le_bytes([bytes[4], bytes[5]]) != CHECKPOINT_FORMAT_VERSION
    {
        return Ok(None);
    }
    let Ok(length_bytes) = bytes[6..CHECKPOINT_HEADER_BYTES].try_into() else {
        return Ok(None);
    };
    let Ok(body_len) = usize::try_from(u64::from_le_bytes(length_bytes)) else {
        return Ok(None);
    };
    let Some(checksum_at) = CHECKPOINT_HEADER_BYTES.checked_add(body_len) else {
        return Ok(None);
    };
    let Some(expected_len) = checksum_at.checked_add(framed_log::CHECKSUM_BYTES) else {
        return Ok(None);
    };
    if expected_len != bytes.len() {
        return Ok(None);
    }
    let Ok(checksum) = bytes[checksum_at..].try_into() else {
        return Ok(None);
    };
    if frame_checksum(&bytes[..checksum_at]) != checksum {
        return Ok(None);
    }
    let body = &bytes[CHECKPOINT_HEADER_BYTES..checksum_at];
    let Some(header_len_bytes) = body.get(..4) else {
        return Ok(None);
    };
    let Ok(header_len_bytes) = <[u8; 4]>::try_from(header_len_bytes) else {
        return Ok(None);
    };
    let Ok(header_len) = usize::try_from(u32::from_le_bytes(header_len_bytes)) else {
        return Ok(None);
    };
    let Some(header_end) = 4usize.checked_add(header_len) else {
        return Ok(None);
    };
    let Some(header_bytes) = body.get(4..header_end) else {
        return Ok(None);
    };
    let Ok(header) = serde_json::from_slice::<HookSpoolCheckpointHeaderV1>(header_bytes) else {
        return Ok(None);
    };
    let Ok(record_count) = usize::try_from(header.record_count) else {
        return Ok(None);
    };
    let Some(entries_len) = record_count.checked_mul(CHECKPOINT_ENTRY_BYTES) else {
        return Ok(None);
    };
    let Some(exact_body_len) = header_end.checked_add(entries_len) else {
        return Ok(None);
    };
    if exact_body_len != body.len()
        || header.version != CHECKPOINT_FORMAT_VERSION
        || header.host != config.host
        || header.record_count > config.limits.max_host_records
        || header.record_count > MAX_SPOOL_RECORDS_PER_HOST
    {
        return Ok(None);
    }
    let mut records = Vec::with_capacity(record_count);
    let mut file_offset = 0u64;
    for entry_bytes in body[header_end..].chunks_exact(CHECKPOINT_ENTRY_BYTES) {
        let Some(record) = decode_checkpoint_entry(entry_bytes, file_offset) else {
            return Ok(None);
        };
        let Some(next_offset) = file_offset.checked_add(u64::from(record.framed_len)) else {
            return Ok(None);
        };
        records.push(record);
        file_offset = next_offset;
    }
    if !valid_cached_records(&records, header.records_revision.as_ref(), config) {
        return Ok(None);
    }
    Ok(Some(ValidatedCheckpointV1 {
        records_revision: header.records_revision,
        records,
        checksum,
        bytes: u64::try_from(bytes.len()).map_err(|_| HookSpoolError::MetadataCorrupted)?,
    }))
}

pub(super) fn write_checkpoint(
    root: &Path,
    config: HookSpoolConfigV1,
    records: &[PendingRecordV1],
) -> Result<CheckpointAnchorV1, HookSpoolError> {
    let records_revision = records_file_revision(root)?;
    if !valid_cached_records(records, records_revision.as_ref(), config) {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    let record_count =
        u32::try_from(records.len()).map_err(|_| HookSpoolError::MetadataCorrupted)?;
    let header = HookSpoolCheckpointHeaderV1 {
        version: CHECKPOINT_FORMAT_VERSION,
        host: config.host,
        records_revision: records_revision.clone(),
        record_count,
    };
    let header_bytes =
        canonical_json_bytes(&header).map_err(|_| HookSpoolError::MetadataCorrupted)?;
    let header_len =
        u32::try_from(header_bytes.len()).map_err(|_| HookSpoolError::MetadataCorrupted)?;
    let entry_bytes = records
        .len()
        .checked_mul(CHECKPOINT_ENTRY_BYTES)
        .ok_or(HookSpoolError::MetadataCorrupted)?;
    let body_capacity = 4usize
        .checked_add(header_bytes.len())
        .and_then(|length| length.checked_add(entry_bytes))
        .ok_or(HookSpoolError::MetadataCorrupted)?;
    let mut body_bytes = Vec::with_capacity(body_capacity);
    body_bytes.extend_from_slice(&header_len.to_le_bytes());
    body_bytes.extend_from_slice(&header_bytes);
    for record in records {
        encode_checkpoint_entry(record, &mut body_bytes);
    }
    let body_len =
        u64::try_from(body_bytes.len()).map_err(|_| HookSpoolError::MetadataCorrupted)?;
    let framed_len = CHECKPOINT_HEADER_BYTES
        .checked_add(body_bytes.len())
        .and_then(|length| length.checked_add(framed_log::CHECKSUM_BYTES))
        .ok_or(HookSpoolError::MetadataCorrupted)?;
    if framed_len > MAX_CHECKPOINT_BYTES {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    let mut bytes = Vec::with_capacity(framed_len);
    bytes.extend_from_slice(CHECKPOINT_MAGIC);
    bytes.extend_from_slice(&CHECKPOINT_FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&body_len.to_le_bytes());
    bytes.extend_from_slice(&body_bytes);
    let checksum = frame_checksum(&bytes);
    bytes.extend_from_slice(&checksum);
    shared_atomic_write(
        &checkpoint_path(root),
        CHECKPOINT_FILE,
        &bytes,
        DIRECTORY_POLICY,
    )
    .map_err(|_| HookSpoolError::Io)?;
    Ok(CheckpointAnchorV1 {
        records_revision,
        checksum,
    })
}

pub(super) fn read_transition(
    root: &Path,
) -> Result<Option<ValidatedTransitionV1>, HookSpoolError> {
    let Some(bytes) = read_accelerator(&transition_path(root), MAX_TRANSITION_BYTES)? else {
        return Ok(None);
    };
    let Ok(file) = serde_json::from_slice::<HookSpoolTransitionFileV1>(&bytes) else {
        return Ok(None);
    };
    let Ok(body_bytes) = canonical_json_bytes(&file.body) else {
        return Ok(None);
    };
    if file.body.version != CHECKPOINT_FORMAT_VERSION
        || frame_checksum(&body_bytes) != file.checksum
    {
        return Ok(None);
    }
    Ok(Some(ValidatedTransitionV1 {
        checkpoint_checksum: file.body.checkpoint_checksum,
        checkpoint_revision: file.body.checkpoint_revision,
        current_revision: file.body.current_revision,
    }))
}

pub(super) fn write_transition(
    root: &Path,
    checkpoint: &CheckpointAnchorV1,
) -> Result<RecordsFileRevisionV1, HookSpoolError> {
    let current_revision = records_file_revision(root)?.ok_or(HookSpoolError::MetadataCorrupted)?;
    let body = HookSpoolTransitionBodyV1 {
        version: CHECKPOINT_FORMAT_VERSION,
        checkpoint_checksum: checkpoint.checksum,
        checkpoint_revision: checkpoint.records_revision.clone(),
        current_revision: current_revision.clone(),
    };
    let body_bytes = canonical_json_bytes(&body).map_err(|_| HookSpoolError::MetadataCorrupted)?;
    let checksum = frame_checksum(&body_bytes);
    let bytes = serde_json::to_vec(&HookSpoolTransitionFileV1 { body, checksum })
        .map_err(|_| HookSpoolError::MetadataCorrupted)?;
    shared_atomic_write(
        &transition_path(root),
        TRANSITION_FILE,
        &bytes,
        DIRECTORY_POLICY,
    )
    .map_err(|_| HookSpoolError::Io)?;
    Ok(current_revision)
}

pub(super) fn records_file_revision(
    root: &Path,
) -> Result<Option<RecordsFileRevisionV1>, HookSpoolError> {
    let path = records_path(root);
    if !validate_regular_or_missing(&path)? {
        return Ok(None);
    }
    let named = path.metadata().map_err(|_| HookSpoolError::Io)?;
    let file = File::open(&path).map_err(|_| HookSpoolError::Io)?;
    let opened = file.metadata().map_err(|_| HookSpoolError::Io)?;
    let named_revision = revision_for_file(&file, &named)?;
    let opened_revision = revision_for_file(&file, &opened)?;
    if named_revision != opened_revision {
        return Err(HookSpoolError::Io);
    }
    Ok(Some(opened_revision))
}

#[cfg(unix)]
pub(super) fn read_frame_at(
    file: &File,
    file_offset: u64,
    framed_len: u32,
) -> Result<Vec<u8>, HookSpoolError> {
    let mut frame = vec![0u8; framed_len as usize];
    file.read_exact_at(&mut frame, file_offset)
        .map_err(|_| HookSpoolError::Io)?;
    Ok(frame)
}

#[cfg(windows)]
pub(super) fn read_frame_at(
    file: &File,
    file_offset: u64,
    framed_len: u32,
) -> Result<Vec<u8>, HookSpoolError> {
    let mut frame = vec![0u8; framed_len as usize];
    let mut read = 0usize;
    while read < frame.len() {
        let offset = file_offset
            .checked_add(u64::try_from(read).map_err(|_| HookSpoolError::Io)?)
            .ok_or(HookSpoolError::Io)?;
        let count = file
            .seek_read(&mut frame[read..], offset)
            .map_err(|_| HookSpoolError::Io)?;
        if count == 0 {
            return Err(HookSpoolError::Io);
        }
        read = read.checked_add(count).ok_or(HookSpoolError::Io)?;
    }
    Ok(frame)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn read_frame_at(
    _file: &File,
    _file_offset: u64,
    _framed_len: u32,
) -> Result<Vec<u8>, HookSpoolError> {
    Err(HookSpoolError::Io)
}

fn read_accelerator(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, HookSpoolError> {
    match read_bounded(path, maximum) {
        Ok(bytes) => Ok(bytes),
        Err(HookSpoolError::MetadataCorrupted) => Ok(None),
        Err(error) => Err(error),
    }
}

fn encode_checkpoint_entry(record: &PendingRecordV1, output: &mut Vec<u8>) {
    output.extend_from_slice(&record.sequence.to_le_bytes());
    output.extend_from_slice(&record.queued_at.0.to_le_bytes());
    output.extend_from_slice(&record.framed_len.to_le_bytes());
    output.extend_from_slice(&record.protected_session_id);
    output.extend_from_slice(&record.event_id);
    output.extend_from_slice(&record.checksum);
}

fn decode_checkpoint_entry(bytes: &[u8], file_offset: u64) -> Option<PendingRecordV1> {
    if bytes.len() != CHECKPOINT_ENTRY_BYTES {
        return None;
    }
    Some(PendingRecordV1 {
        sequence: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
        queued_at: tracedecay_domain::UtcMicros(i64::from_le_bytes(bytes[8..16].try_into().ok()?)),
        framed_len: u32::from_le_bytes(bytes[16..20].try_into().ok()?),
        protected_session_id: bytes[20..52].try_into().ok()?,
        file_offset,
        event_id: bytes[52..68].try_into().ok()?,
        checksum: bytes[68..100].try_into().ok()?,
        envelope: None,
    })
}

fn valid_cached_records(
    records: &[PendingRecordV1],
    revision: Option<&RecordsFileRevisionV1>,
    config: HookSpoolConfigV1,
) -> bool {
    if records.len() > config.limits.max_host_records as usize
        || records.len() > MAX_SPOOL_RECORDS_PER_HOST as usize
    {
        return false;
    }
    let mut end = 0u64;
    let mut previous = None;
    let minimum_framed_len =
        (super::FRAME_LENGTH_BYTES + super::FRAME_HEADER_BYTES + super::FRAME_CHECKSUM_BYTES + 1)
            as u64;
    let maximum_framed_len = (super::FRAME_LENGTH_BYTES
        + super::FRAME_HEADER_BYTES
        + super::FRAME_CHECKSUM_BYTES
        + MAX_HOOK_PAYLOAD_BYTES) as u64;
    for record in records {
        if record.sequence == 0
            || previous.is_some_and(|sequence| record.sequence <= sequence)
            || u64::from(record.framed_len) < minimum_framed_len
            || u64::from(record.framed_len) > maximum_framed_len
        {
            return false;
        }
        previous = Some(record.sequence);
        end = match end.checked_add(u64::from(record.framed_len)) {
            Some(end) => end,
            None => return false,
        };
    }
    match revision {
        Some(revision) => revision.length == end && revision.length <= config.limits.max_host_bytes,
        None => records.is_empty(),
    }
}

#[cfg(unix)]
fn revision_for_file(
    _file: &File,
    metadata: &std::fs::Metadata,
) -> Result<RecordsFileRevisionV1, HookSpoolError> {
    let mut hasher = Sha256::new();
    hasher.update(b"hook-spool-records-unix-v1");
    hasher.update(metadata.dev().to_le_bytes());
    hasher.update(metadata.ino().to_le_bytes());
    Ok(RecordsFileRevisionV1 {
        identity: hasher.finalize().into(),
        length: metadata.len(),
        modified: [metadata.mtime(), metadata.mtime_nsec()],
        changed: [metadata.ctime(), metadata.ctime_nsec()],
    })
}

#[cfg(windows)]
fn revision_for_file(
    file: &File,
    metadata: &std::fs::Metadata,
) -> Result<RecordsFileRevisionV1, HookSpoolError> {
    let information =
        tracedecay_private_fs::windows_file::information(file).map_err(|_| HookSpoolError::Io)?;
    let mut hasher = Sha256::new();
    hasher.update(b"hook-spool-records-windows-v1");
    hasher.update(information.volume_serial_number.to_le_bytes());
    hasher.update(information.file_index.to_le_bytes());
    Ok(RecordsFileRevisionV1 {
        identity: hasher.finalize().into(),
        length: metadata.file_size(),
        modified: [
            i64::try_from(metadata.last_write_time()).map_err(|_| HookSpoolError::Io)?,
            0,
        ],
        changed: [
            i64::try_from(metadata.creation_time()).map_err(|_| HookSpoolError::Io)?,
            0,
        ],
    })
}

#[cfg(not(any(unix, windows)))]
fn revision_for_file(
    _file: &File,
    _metadata: &std::fs::Metadata,
) -> Result<RecordsFileRevisionV1, HookSpoolError> {
    Err(HookSpoolError::Io)
}
