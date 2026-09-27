use std::collections::BTreeMap;
use std::path::Path;

use tracedecay_private_fs::framed_log::atomic_write as shared_atomic_write;

use serde_json::Value;

use super::types::{AcknowledgedSequenceV1, HookSpoolLimitsV1, HookSpoolMetaV1, PendingRecordV1};
use super::{DIRECTORY_POLICY, HookSpoolError, MAX_META_BYTES, meta_path, read_bounded};

pub(super) fn read_meta(root: &Path) -> Result<Option<HookSpoolMetaV1>, HookSpoolError> {
    read_bounded(&meta_path(root), MAX_META_BYTES)?
        .map(|bytes| decode_exact_meta(&bytes))
        .transpose()
}

fn decode_exact_meta(bytes: &[u8]) -> Result<HookSpoolMetaV1, HookSpoolError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| HookSpoolError::MetadataCorrupted)?;
    let Some(found) = value.get("version").and_then(Value::as_u64) else {
        return Err(HookSpoolError::ResetRequired {
            reason: super::HookSpoolResetReasonV1::MetadataShape,
        });
    };
    let found = u16::try_from(found).map_err(|_| HookSpoolError::ResetRequired {
        reason: super::HookSpoolResetReasonV1::MetadataShape,
    })?;
    if found != super::SPOOL_META_VERSION {
        return Err(HookSpoolError::ResetRequired {
            reason: super::HookSpoolResetReasonV1::MetadataVersion {
                found,
                expected: super::SPOOL_META_VERSION,
            },
        });
    }
    serde_json::from_value(value).map_err(|_| HookSpoolError::ResetRequired {
        reason: super::HookSpoolResetReasonV1::MetadataShape,
    })
}

#[hotpath::measure(label = "hooks.spool.write_meta")]
pub(super) fn write_meta(root: &Path, meta: &HookSpoolMetaV1) -> Result<(), HookSpoolError> {
    let bytes = serde_json::to_vec(meta).map_err(|_| HookSpoolError::MetadataCorrupted)?;
    if bytes.len() > MAX_META_BYTES {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    hotpath::gauge!("hooks.spool.meta.bytes").set(bytes.len());
    hotpath::measure_block!("hooks.spool.fsync.meta", {
        shared_atomic_write(&meta_path(root), "meta", &bytes, DIRECTORY_POLICY)
            .map_err(|_| HookSpoolError::Io)
    })
}

/// Records appended after the last metadata write continue its sequence
/// contiguously; the next metadata write persists the advanced value.
pub(super) fn advance_next_sequence(
    meta: &mut HookSpoolMetaV1,
    records: &[PendingRecordV1],
) -> Result<(), HookSpoolError> {
    let first_unrecorded = meta.next_sequence;
    for record in records
        .iter()
        .filter(|record| record.sequence >= first_unrecorded)
    {
        if record.sequence != meta.next_sequence {
            return Err(HookSpoolError::MetadataCorrupted);
        }
        meta.next_sequence = meta
            .next_sequence
            .checked_add(1)
            .ok_or(HookSpoolError::MetadataCorrupted)?;
    }
    Ok(())
}

pub(super) fn validate_meta(
    meta: &HookSpoolMetaV1,
    limits: HookSpoolLimitsV1,
) -> Result<(), HookSpoolError> {
    if meta.next_sequence == 0
        || meta.next_sequence <= meta.committed_through
        || meta.acknowledged.len() > limits.max_host_records as usize
    {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    let _ = acknowledged_map(meta)?;
    Ok(())
}

pub(super) fn validate_meta_against_records(
    meta: &HookSpoolMetaV1,
    sequences: impl IntoIterator<Item = u64>,
    limits: HookSpoolLimitsV1,
) -> Result<(), HookSpoolError> {
    let outstanding = meta
        .next_sequence
        .checked_sub(meta.committed_through)
        .and_then(|distance| distance.checked_sub(1))
        .ok_or(HookSpoolError::MetadataCorrupted)?;
    if outstanding > limits.max_host_records as u64 {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    let acknowledged = acknowledged_map(meta)?;
    let mut present = sequences.into_iter().peekable();
    let mut acknowledged = acknowledged.keys().copied().peekable();
    for sequence in meta.committed_through.saturating_add(1)..meta.next_sequence {
        while present.peek().is_some_and(|present| *present < sequence) {
            present.next();
        }
        while acknowledged
            .peek()
            .is_some_and(|acknowledged| *acknowledged < sequence)
        {
            acknowledged.next();
        }
        if present.peek().is_some_and(|present| *present == sequence) {
            present.next();
        } else if acknowledged
            .peek()
            .is_some_and(|acknowledged| *acknowledged == sequence)
        {
            acknowledged.next();
        } else {
            return Err(HookSpoolError::MetadataCorrupted);
        }
    }
    while present
        .peek()
        .is_some_and(|present| *present < meta.next_sequence)
    {
        present.next();
    }
    if present.next().is_some() {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    Ok(())
}

pub(super) fn acknowledged_map(
    meta: &HookSpoolMetaV1,
) -> Result<BTreeMap<u64, AcknowledgedSequenceV1>, HookSpoolError> {
    let mut entries = BTreeMap::new();
    for entry in &meta.acknowledged {
        if entry.sequence <= meta.committed_through
            || entry.sequence >= meta.next_sequence
            || entry.receipt_id == [0; 16]
            || entries.insert(entry.sequence, *entry).is_some()
        {
            return Err(HookSpoolError::MetadataCorrupted);
        }
    }
    Ok(entries)
}

pub(super) fn normalize_acknowledgements(meta: &mut HookSpoolMetaV1) -> Result<(), HookSpoolError> {
    let mut map = acknowledged_map(meta)?;
    while let Some(next) = meta.committed_through.checked_add(1) {
        if map.remove(&next).is_some() {
            meta.committed_through = next;
        } else {
            break;
        }
    }
    meta.acknowledged = map.into_values().collect();
    Ok(())
}
