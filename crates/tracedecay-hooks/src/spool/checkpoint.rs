use std::fs::File;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    canonical_json_bytes,
    framed_log::{self, checksum as frame_checksum},
};
use tracedecay_private_fs::framed_log::atomic_write as shared_atomic_write;

use crate::{HookHostV1, MAX_SPOOL_BYTES_PER_HOST, MAX_SPOOL_RECORDS_PER_HOST};

use super::{
    CHECKPOINT_FILE, CHECKPOINT_FORMAT_VERSION, DIRECTORY_POLICY, HookSpoolConfigV1,
    HookSpoolError, HookSpoolRecordV1, TRANSITION_FILE, checkpoint_path, read_bounded,
    records_path, transition_path, validate_regular_or_missing,
};

const MAX_CHECKPOINT_BYTES: usize = (MAX_SPOOL_BYTES_PER_HOST as usize) * 4;
const MAX_TRANSITION_BYTES: usize = 16 * 1024;
const CHECKPOINT_MAGIC: &[u8; 4] = b"TDHC";
const CHECKPOINT_HEADER_BYTES: usize = 4 + 2 + 8;
// Bound suffix validation by both ordinary frame count and unusually large
// payload bytes while amortizing each full checkpoint publication.
pub(super) const CHECKPOINT_REWRITE_FRAME_THRESHOLD: u32 = 64;
pub(super) const CHECKPOINT_REWRITE_BYTE_THRESHOLD: u64 = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecordsFileRevisionV1 {
    pub(super) identity: [u8; 32],
    pub(super) length: u64,
    pub(super) modified: [i64; 2],
    pub(super) changed: [i64; 2],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookSpoolCheckpointBodyV1 {
    version: u16,
    host: HookHostV1,
    records_revision: Option<RecordsFileRevisionV1>,
    records: Vec<HookSpoolRecordV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ValidatedCheckpointV1 {
    pub(super) records_revision: Option<RecordsFileRevisionV1>,
    pub(super) records: Vec<HookSpoolRecordV1>,
    pub(super) checksum: [u8; 32],
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
    let Ok(body) = serde_json::from_slice::<HookSpoolCheckpointBodyV1>(
        &bytes[CHECKPOINT_HEADER_BYTES..checksum_at],
    ) else {
        return Ok(None);
    };
    if body.version != CHECKPOINT_FORMAT_VERSION
        || body.host != config.host
        || !valid_cached_records(&body.records, body.records_revision.as_ref(), config)
    {
        return Ok(None);
    }
    Ok(Some(ValidatedCheckpointV1 {
        records_revision: body.records_revision,
        records: body.records,
        checksum,
    }))
}

pub(super) fn write_checkpoint(
    root: &Path,
    config: HookSpoolConfigV1,
    records: &[HookSpoolRecordV1],
) -> Result<CheckpointAnchorV1, HookSpoolError> {
    let records_revision = records_file_revision(root)?;
    if !valid_cached_records(records, records_revision.as_ref(), config) {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    let body = HookSpoolCheckpointBodyV1 {
        version: CHECKPOINT_FORMAT_VERSION,
        host: config.host,
        records_revision: records_revision.clone(),
        records: records.to_vec(),
    };
    let body_bytes = canonical_json_bytes(&body).map_err(|_| HookSpoolError::MetadataCorrupted)?;
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

fn read_accelerator(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, HookSpoolError> {
    match read_bounded(path, maximum) {
        Ok(bytes) => Ok(bytes),
        Err(HookSpoolError::MetadataCorrupted) => Ok(None),
        Err(error) => Err(error),
    }
}

fn valid_cached_records(
    records: &[HookSpoolRecordV1],
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
    for record in records {
        if record.sequence == 0
            || previous.is_some_and(|sequence| record.sequence <= sequence)
            || record.framed_len == 0
            || record.encoded_len == 0
            || record.envelope.producer != config.host
            || record.envelope.protected_session_id != record.protected_session_id
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
    use std::os::unix::fs::MetadataExt;

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
    use std::os::windows::fs::MetadataExt;

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
