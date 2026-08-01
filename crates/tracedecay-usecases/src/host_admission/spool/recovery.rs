use std::collections::BTreeMap;

use super::bounds::{SpoolBounds, SpoolOverflowDisposition};
use super::frames::encode_frame;
use super::meta::SpoolMetaV1;
use super::quarantine::TerminalQuarantine;
use super::types::{SpoolError, SpoolIntegrity, SpoolRecord};

pub(crate) struct PendingRecovery {
    pub(crate) pending: Vec<SpoolRecord>,
    pub(crate) pending_bytes: usize,
    pub(crate) pending_by_source: BTreeMap<String, (usize, usize)>,
    pub(crate) recovered_next_seq: Option<u64>,
}

pub(crate) fn recover_pending(
    records: Vec<SpoolRecord>,
    quarantine: &TerminalQuarantine,
    meta: &SpoolMetaV1,
    bounds: SpoolBounds,
) -> Result<PendingRecovery, SpoolError> {
    for record in &records {
        if let Some(entry) = quarantine.entry(record.seq) {
            let active_frame = encode_frame(record.seq, record.source.as_bytes(), &record.payload)?;
            if entry.active_frame != active_frame {
                return Err(SpoolError::QuarantineCorrupted {
                    at_offset: record.file_offset,
                });
            }
        }
    }
    if let Some(first) = records
        .iter()
        .find(|record| record.seq > meta.committed_through)
        && first.seq > meta.committed_through + 1
    {
        let missing = first.seq - meta.committed_through - 1;
        if missing > quarantine.len() as u64
            || (meta.committed_through + 1..first.seq).any(|seq| !quarantine.contains(seq))
        {
            return Err(SpoolError::Corrupted {
                at_offset: first.file_offset,
            });
        }
    }

    let highest_unresolved = records
        .iter()
        .map(|record| record.seq)
        .chain(quarantine.iter().map(|(seq, _)| *seq))
        .filter(|seq| *seq > meta.committed_through)
        .max();
    let recovered_next_seq = if matches!(meta.integrity, SpoolIntegrity::Corrupted { .. }) {
        None
    } else {
        match highest_unresolved {
            Some(highest) if highest == meta.next_seq => {
                if quarantine.contains(highest)
                    || records.last().map(|record| record.seq) != Some(highest)
                {
                    return Err(SpoolError::MetadataCorrupted);
                }
                Some(
                    highest
                        .checked_add(1)
                        .ok_or(SpoolError::MetadataCorrupted)?,
                )
            }
            Some(highest) if highest.checked_add(1) == Some(meta.next_seq) => None,
            None if meta.next_seq == meta.committed_through + 1 => None,
            Some(_) | None => return Err(SpoolError::MetadataCorrupted),
        }
    };
    let effective_next_seq = recovered_next_seq.unwrap_or(meta.next_seq);
    if quarantine
        .iter()
        .any(|(seq, _)| *seq == 0 || *seq == u64::MAX || *seq >= effective_next_seq)
    {
        return Err(SpoolError::QuarantineCorrupted { at_offset: 0 });
    }

    if matches!(meta.integrity, SpoolIntegrity::Healthy) {
        let evidence_count = records
            .iter()
            .filter(|record| record.seq > meta.committed_through)
            .count()
            + quarantine
                .iter()
                .filter(|(seq, _)| {
                    **seq > meta.committed_through
                        && records
                            .binary_search_by_key(&**seq, |record| record.seq)
                            .is_err()
                })
                .count();
        let expected_count = effective_next_seq - meta.committed_through - 1;
        if expected_count > evidence_count as u64 {
            return Err(SpoolError::MetadataCorrupted);
        }
        for seq in meta.committed_through + 1..effective_next_seq {
            if !quarantine.contains(seq)
                && records
                    .binary_search_by_key(&seq, |record| record.seq)
                    .is_err()
            {
                return Err(SpoolError::Corrupted { at_offset: 0 });
            }
        }
    }

    let mut pending = Vec::new();
    let mut pending_bytes = 0usize;
    let mut pending_by_source = BTreeMap::<String, (usize, usize)>::new();
    for record in records {
        if record.seq <= meta.committed_through || quarantine.contains(record.seq) {
            continue;
        }
        if pending.len() >= bounds.max_records {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxRecords));
        }
        pending_bytes = pending_bytes
            .checked_add(record.framed_len)
            .ok_or(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes))?;
        if pending_bytes > bounds.max_spool_bytes {
            return Err(SpoolError::Overflow(SpoolOverflowDisposition::MaxBytes));
        }
        let source_usage = pending_by_source.entry(record.source.clone()).or_default();
        source_usage.0 += 1;
        source_usage.1 =
            source_usage
                .1
                .checked_add(record.framed_len)
                .ok_or(SpoolError::Overflow(
                    SpoolOverflowDisposition::MaxBytesPerSource,
                ))?;
        if source_usage.0 > bounds.max_records_per_source {
            return Err(SpoolError::Overflow(
                SpoolOverflowDisposition::MaxRecordsPerSource,
            ));
        }
        if source_usage.1 > bounds.max_spool_bytes_per_source {
            return Err(SpoolError::Overflow(
                SpoolOverflowDisposition::MaxBytesPerSource,
            ));
        }
        pending.push(record);
    }
    Ok(PendingRecovery {
        pending,
        pending_bytes,
        pending_by_source,
        recovered_next_seq,
    })
}
