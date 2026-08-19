use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::UtcMicros;

use super::{
    HookSpoolError, HookSpoolLimitsV1, HookSpoolRecordV1, MAX_REPLAY_BATCH_BYTES,
    MAX_REPLAY_BATCH_RECORDS, MAX_SPOOL_AGE_MICROS,
};

pub(super) fn usage_by_session(
    records: &[HookSpoolRecordV1],
    limits: HookSpoolLimitsV1,
) -> Result<BTreeMap<[u8; 32], (u32, u64)>, HookSpoolError> {
    let mut usage = BTreeMap::<[u8; 32], (u32, u64)>::new();
    for record in records {
        let entry = usage.entry(record.protected_session_id).or_default();
        entry.0 = entry.0.checked_add(1).ok_or(HookSpoolError::SpoolFull)?;
        entry.1 = entry.1.saturating_add(u64::from(record.framed_len));
        if entry.0 > limits.max_session_records || entry.1 > limits.max_session_bytes {
            return Err(HookSpoolError::SpoolFull);
        }
    }
    Ok(usage)
}

pub(super) fn replayable_sessions(
    pending: &[HookSpoolRecordV1],
    now: UtcMicros,
) -> BTreeSet<[u8; 32]> {
    pending
        .iter()
        .filter(|record| !is_expired(record, now))
        .map(|record| record.protected_session_id)
        .collect()
}

pub(super) fn round_robin_after(
    sessions: &BTreeSet<[u8; 32]>,
    after: Option<[u8; 32]>,
) -> Vec<[u8; 32]> {
    let mut ordered = sessions.iter().copied().collect::<Vec<_>>();
    if let Some(after) = after
        && let Some(index) = ordered.iter().position(|session| *session > after)
    {
        ordered.rotate_left(index);
    }
    ordered
}

pub(super) fn batch_for_session(
    pending: &[HookSpoolRecordV1],
    session: [u8; 32],
    now: UtcMicros,
) -> Result<Vec<HookSpoolRecordV1>, HookSpoolError> {
    let mut records = Vec::new();
    let mut bytes = 0u32;
    for record in pending
        .iter()
        .filter(|record| record.protected_session_id == session && !is_expired(record, now))
    {
        let next = bytes
            .checked_add(record.framed_len)
            .ok_or(HookSpoolError::ReplayBatchExceeded)?;
        if records.len() >= MAX_REPLAY_BATCH_RECORDS as usize || next > MAX_REPLAY_BATCH_BYTES {
            break;
        }
        bytes = next;
        records.push(record.clone());
    }
    Ok(records)
}

pub(super) fn is_expired(record: &HookSpoolRecordV1, now: UtcMicros) -> bool {
    now.0.saturating_sub(record.queued_at.0) > MAX_SPOOL_AGE_MICROS
}
