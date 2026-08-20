use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use tracedecay_domain::framed_log::partial_tail_matches_prefix;
use tracedecay_domain::{canonical_json_bytes, framed_log::checksum as frame_checksum};
use tracedecay_private_fs::framed_log::atomic_write as shared_atomic_write;

use crate::HookHostV1;
use serde_json::Value;

use super::frame::{decode_complete_frame, encode_frame};
use super::types::{
    AcknowledgedSequenceV1, AppendIntentV1, HookSpoolLimitsV1, HookSpoolMetaV1, HookSpoolRecordV1,
};
use super::{
    DIRECTORY_POLICY, FRAME_CHECKSUM_BYTES, FRAME_HEADER_BYTES, FRAME_LENGTH_BYTES, HookSpoolError,
    MAX_META_BYTES, SPOOL_MAGIC, meta_path, read_bounded,
};

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

pub(super) fn write_meta(root: &Path, meta: &HookSpoolMetaV1) -> Result<(), HookSpoolError> {
    let bytes = serde_json::to_vec(meta).map_err(|_| HookSpoolError::MetadataCorrupted)?;
    if bytes.len() > MAX_META_BYTES {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    shared_atomic_write(&meta_path(root), "meta", &bytes, DIRECTORY_POLICY)
        .map_err(|_| HookSpoolError::Io)
}

pub(super) fn append_intent(
    sequence: u64,
    file_offset: u64,
    frame: &[u8],
) -> Result<AppendIntentV1, HookSpoolError> {
    Ok(AppendIntentV1 {
        sequence,
        file_offset,
        framed_len: u32::try_from(frame.len()).map_err(|_| HookSpoolError::MetadataCorrupted)?,
        frame: frame.to_vec(),
    })
}

pub(super) fn partial_tail_matches_intent(
    meta: &HookSpoolMetaV1,
    offset: u64,
    partial: &[u8],
) -> bool {
    let Some(intent) = &meta.append_intent else {
        return false;
    };
    intent.sequence == meta.next_sequence
        && intent.file_offset == offset
        && partial_tail_matches_prefix(partial, &intent.frame, intent.framed_len as usize)
}

pub(super) fn reconcile_append_intent(
    meta: &mut HookSpoolMetaV1,
    records: &[HookSpoolRecordV1],
    host: HookHostV1,
) -> Result<(), HookSpoolError> {
    let Some(intent) = meta.append_intent.clone() else {
        return Ok(());
    };
    if intent.sequence != meta.next_sequence || !valid_append_intent(&intent, host) {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    if let Some(record) = records
        .iter()
        .find(|record| record.sequence == intent.sequence)
    {
        let payload = canonical_json_bytes(&record.envelope)
            .map_err(|_| HookSpoolError::MetadataCorrupted)?;
        let frame = encode_frame(
            record.sequence,
            record.queued_at,
            record.protected_session_id,
            &payload,
        )?;
        if record.framed_len != intent.framed_len || frame != intent.frame {
            return Err(HookSpoolError::MetadataCorrupted);
        }
        meta.next_sequence = meta
            .next_sequence
            .checked_add(1)
            .ok_or(HookSpoolError::MetadataCorrupted)?;
    }
    meta.append_intent = None;
    Ok(())
}

pub(super) fn validate_meta(
    meta: &HookSpoolMetaV1,
    limits: HookSpoolLimitsV1,
    host: HookHostV1,
) -> Result<(), HookSpoolError> {
    if meta.next_sequence == 0
        || meta.next_sequence <= meta.committed_through
        || meta.acknowledged.len() > limits.max_host_records as usize
    {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    let _ = acknowledged_map(meta)?;
    if let Some(intent) = &meta.append_intent
        && (intent.sequence != meta.next_sequence
            || intent.framed_len
                < (FRAME_LENGTH_BYTES + FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES) as u32
            || !valid_append_intent(intent, host)
            || intent
                .file_offset
                .checked_add(u64::from(intent.framed_len))
                .is_none())
    {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    Ok(())
}

pub(super) fn valid_append_intent(intent: &AppendIntentV1, host: HookHostV1) -> bool {
    let minimum = FRAME_LENGTH_BYTES + FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES;
    if intent.sequence == 0
        || intent.frame.len() < minimum
        || intent.frame.len() != intent.framed_len as usize
        || intent.frame.get(4..8) != Some(SPOOL_MAGIC.as_slice())
    {
        return false;
    }
    let Some(sequence) = intent.frame.get(10..18) else {
        return false;
    };
    let Ok(sequence) = <[u8; 8]>::try_from(sequence) else {
        return false;
    };
    let checksum_at = intent.frame.len() - FRAME_CHECKSUM_BYTES;
    let Some(checksum) = intent.frame.get(checksum_at..) else {
        return false;
    };
    intent.sequence == u64::from_le_bytes(sequence)
        && frame_checksum(&intent.frame[..checksum_at]) == checksum
        && decode_complete_frame(&intent.frame, intent.file_offset, host).is_ok()
}

pub(super) fn validate_meta_against_records(
    meta: &HookSpoolMetaV1,
    records: &[HookSpoolRecordV1],
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
    let present = records
        .iter()
        .map(|record| record.sequence)
        .collect::<BTreeSet<_>>();
    if records
        .iter()
        .any(|record| record.sequence >= meta.next_sequence)
    {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    for sequence in meta.committed_through.saturating_add(1)..meta.next_sequence {
        if !acknowledged.contains_key(&sequence) && !present.contains(&sequence) {
            return Err(HookSpoolError::MetadataCorrupted);
        }
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
