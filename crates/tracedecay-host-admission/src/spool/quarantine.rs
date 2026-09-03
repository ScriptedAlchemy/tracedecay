use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use super::{
    SpoolBounds, SpoolError, SpoolRecord, TerminalReason, append_frame_durable, encode_frame,
    file_len, tighten_existing_file, truncate_file, validate_quarantined_active_frame,
};

const MAGIC: &[u8; 4] = b"TDHQ";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 24;
const CHECKSUM_BYTES: usize = 32;
pub(super) const FRAME_OVERHEAD: usize = HEADER_BYTES + CHECKSUM_BYTES;

#[derive(Clone)]
pub(super) struct QuarantineEntry {
    pub(super) reason: TerminalReason,
    pub(super) active_frame: Vec<u8>,
}

pub(super) struct QuarantineOpenReport {
    pub(super) records: usize,
    pub(super) bytes: usize,
    pub(super) truncated_partial_tail_bytes: u64,
}

pub(super) struct TerminalQuarantine {
    path: PathBuf,
    max_bytes: usize,
    max_records: usize,
    entries: BTreeMap<u64, QuarantineEntry>,
    bytes: usize,
    partial_tail: Option<PartialTail>,
}

#[derive(Debug)]
struct PartialTail {
    offset: u64,
    bytes: Vec<u8>,
}

struct ParsedHeader {
    reason: TerminalReason,
    active_len: u64,
    seq: u64,
}

impl fmt::Debug for TerminalQuarantine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalQuarantine")
            .field("path", &self.path)
            .field("records", &self.entries.len())
            .field("bytes", &self.bytes)
            .field("max_records", &self.max_records)
            .field("max_bytes", &self.max_bytes)
            .field("partial_tail", &self.partial_tail)
            .finish()
    }
}

impl TerminalQuarantine {
    pub(super) fn open(
        path: PathBuf,
        bounds: SpoolBounds,
    ) -> Result<(Self, QuarantineOpenReport), SpoolError> {
        tighten_existing_file(&path)?;
        let physical_len = file_len(&path)?;
        if physical_len > bounds.max_quarantine_bytes as u64 {
            return Err(SpoolError::QuarantineFull);
        }
        if !path.exists() {
            return Ok((
                Self {
                    path,
                    max_bytes: bounds.max_quarantine_bytes,
                    max_records: bounds.max_quarantine_records,
                    entries: BTreeMap::new(),
                    bytes: 0,
                    partial_tail: None,
                },
                QuarantineOpenReport {
                    records: 0,
                    bytes: 0,
                    truncated_partial_tail_bytes: 0,
                },
            ));
        }

        let mut input = File::open(&path).map_err(|_| SpoolError::Io)?;
        let mut entries = BTreeMap::new();
        let mut offset = 0u64;
        let mut truncate_to = physical_len;
        while offset < physical_len {
            let remaining = physical_len - offset;
            if remaining < HEADER_BYTES as u64 {
                truncate_to = offset;
                break;
            }
            let mut header = [0u8; HEADER_BYTES];
            input.read_exact(&mut header).map_err(|_| SpoolError::Io)?;
            let parsed = parse_header(&header, offset)?;
            let framed_len = HEADER_BYTES
                .checked_add(
                    usize::try_from(parsed.active_len)
                        .map_err(|_| SpoolError::QuarantineCorrupted { at_offset: offset })?,
                )
                .and_then(|len| len.checked_add(CHECKSUM_BYTES))
                .ok_or(SpoolError::QuarantineCorrupted { at_offset: offset })?;
            if parsed.active_len > bounds.max_spool_bytes as u64 {
                return Err(SpoolError::QuarantineCorrupted { at_offset: offset });
            }
            if framed_len as u64 > remaining {
                truncate_to = offset;
                break;
            }
            if entries.len() >= bounds.max_quarantine_records {
                return Err(SpoolError::QuarantineFull);
            }

            let mut active_frame = vec![0u8; parsed.active_len as usize];
            let mut checksum = [0u8; CHECKSUM_BYTES];
            input
                .read_exact(&mut active_frame)
                .map_err(|_| SpoolError::Io)?;
            input
                .read_exact(&mut checksum)
                .map_err(|_| SpoolError::Io)?;
            let mut hasher = Sha256::new();
            hasher.update(header);
            hasher.update(&active_frame);
            if hasher.finalize().as_slice() != checksum
                || !validate_quarantined_active_frame(parsed.seq, &active_frame, bounds)
                || entries
                    .insert(
                        parsed.seq,
                        QuarantineEntry {
                            reason: parsed.reason,
                            active_frame,
                        },
                    )
                    .is_some()
            {
                return Err(SpoolError::QuarantineCorrupted { at_offset: offset });
            }
            offset += framed_len as u64;
        }

        let partial_tail = if truncate_to < physical_len {
            input
                .seek(SeekFrom::Start(truncate_to))
                .map_err(|_| SpoolError::Io)?;
            let mut bytes = Vec::with_capacity((physical_len - truncate_to) as usize);
            input.read_to_end(&mut bytes).map_err(|_| SpoolError::Io)?;
            Some(PartialTail {
                offset: truncate_to,
                bytes,
            })
        } else {
            None
        };
        let bytes = truncate_to as usize;
        let report = QuarantineOpenReport {
            records: entries.len(),
            bytes,
            truncated_partial_tail_bytes: 0,
        };
        Ok((
            Self {
                path,
                max_bytes: bounds.max_quarantine_bytes,
                max_records: bounds.max_quarantine_records,
                entries,
                bytes,
                partial_tail,
            },
            report,
        ))
    }

    pub(super) fn recover_partial_tail(
        &mut self,
        active_records: &[SpoolRecord],
        committed_through: u64,
        next_seq: u64,
    ) -> Result<u64, SpoolError> {
        let Some(tail) = self.partial_tail.as_ref() else {
            return Ok(0);
        };
        let evidence_count = active_records.len().saturating_add(self.entries.len()) as u64;
        let Some(expected_evidence) = next_seq
            .checked_sub(committed_through)
            .and_then(|distance| distance.checked_sub(1))
        else {
            return Err(SpoolError::QuarantineCorrupted {
                at_offset: tail.offset,
            });
        };
        if expected_evidence > evidence_count
            || active_records.iter().any(|record| record.seq >= next_seq)
            || self.entries.keys().any(|seq| *seq >= next_seq)
        {
            return Err(SpoolError::QuarantineCorrupted {
                at_offset: tail.offset,
            });
        }
        for seq in committed_through + 1..next_seq {
            if !self.entries.contains_key(&seq)
                && active_records
                    .binary_search_by_key(&seq, |record| record.seq)
                    .is_err()
            {
                return Err(SpoolError::QuarantineCorrupted {
                    at_offset: tail.offset,
                });
            }
        }
        if tail.bytes.len() >= 6 && &tail.bytes[..4] == MAGIC {
            let version = u16::from_le_bytes([tail.bytes[4], tail.bytes[5]]);
            if version != VERSION {
                return Err(SpoolError::UnsupportedVersion(version));
            }
        }
        if tail.bytes.len() < HEADER_BYTES {
            return Err(SpoolError::QuarantineCorrupted {
                at_offset: tail.offset,
            });
        }
        let mut header = [0u8; HEADER_BYTES];
        header.copy_from_slice(&tail.bytes[..HEADER_BYTES]);
        let parsed = parse_header(&header, tail.offset)?;
        if parsed.seq <= committed_through
            || parsed.seq >= next_seq
            || self.entries.contains_key(&parsed.seq)
        {
            return Err(SpoolError::QuarantineCorrupted {
                at_offset: tail.offset,
            });
        }
        let Some(active) = active_records
            .iter()
            .find(|record| record.seq == parsed.seq)
        else {
            return Err(SpoolError::QuarantineCorrupted {
                at_offset: tail.offset,
            });
        };
        let active_frame = encode_frame(active.seq, active.source.as_bytes(), &active.payload)?;
        if parsed.active_len != active_frame.len() as u64 {
            return Err(SpoolError::QuarantineCorrupted {
                at_offset: tail.offset,
            });
        }
        let expected = encode(parsed.seq, parsed.reason, &active_frame)?;
        if tail.bytes.len() >= expected.len() || !expected.starts_with(&tail.bytes) {
            return Err(SpoolError::QuarantineCorrupted {
                at_offset: tail.offset,
            });
        }
        let truncated = tail.bytes.len() as u64;
        truncate_file(&self.path, tail.offset)?;
        self.partial_tail = None;
        Ok(truncated)
    }

    pub(super) fn preserve(
        &mut self,
        seq: u64,
        reason: TerminalReason,
        active_frame: &[u8],
    ) -> Result<bool, SpoolError> {
        if let Some(existing) = self.entries.get(&seq) {
            if existing.reason == reason && existing.active_frame == active_frame {
                return Ok(false);
            }
            return Err(SpoolError::QuarantineCorrupted { at_offset: 0 });
        }
        if self.entries.len() >= self.max_records {
            return Err(SpoolError::QuarantineFull);
        }
        let frame = encode(seq, reason, active_frame)?;
        if self.bytes.saturating_add(frame.len()) > self.max_bytes {
            return Err(SpoolError::QuarantineFull);
        }
        append_frame_durable(&self.path, &frame)?;
        self.bytes += frame.len();
        self.entries.insert(
            seq,
            QuarantineEntry {
                reason,
                active_frame: active_frame.to_vec(),
            },
        );
        Ok(true)
    }

    pub(super) fn contains(&self, seq: u64) -> bool {
        self.entries.contains_key(&seq)
    }

    pub(super) fn entry(&self, seq: u64) -> Option<&QuarantineEntry> {
        self.entries.get(&seq)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&u64, &QuarantineEntry)> {
        self.entries.iter()
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

fn parse_header(header: &[u8; HEADER_BYTES], at_offset: u64) -> Result<ParsedHeader, SpoolError> {
    if &header[0..4] != MAGIC || header[7] != 0 {
        return Err(SpoolError::QuarantineCorrupted { at_offset });
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != VERSION {
        return Err(SpoolError::UnsupportedVersion(version));
    }
    let Some(reason) = TerminalReason::from_code(header[6]) else {
        return Err(SpoolError::QuarantineCorrupted { at_offset });
    };
    Ok(ParsedHeader {
        reason,
        active_len: u64::from_le_bytes([
            header[8], header[9], header[10], header[11], header[12], header[13], header[14],
            header[15],
        ]),
        seq: u64::from_le_bytes([
            header[16], header[17], header[18], header[19], header[20], header[21], header[22],
            header[23],
        ]),
    })
}

pub(super) fn encode(
    seq: u64,
    reason: TerminalReason,
    active_frame: &[u8],
) -> Result<Vec<u8>, SpoolError> {
    let active_len = u64::try_from(active_frame.len()).map_err(|_| SpoolError::QuarantineFull)?;
    let capacity = HEADER_BYTES
        .checked_add(active_frame.len())
        .and_then(|len| len.checked_add(CHECKSUM_BYTES))
        .ok_or(SpoolError::QuarantineFull)?;
    let mut frame = Vec::with_capacity(capacity);
    frame.extend_from_slice(MAGIC);
    frame.extend_from_slice(&VERSION.to_le_bytes());
    frame.push(reason.code());
    frame.push(0);
    frame.extend_from_slice(&active_len.to_le_bytes());
    frame.extend_from_slice(&seq.to_le_bytes());
    frame.extend_from_slice(active_frame);
    frame.extend_from_slice(&Sha256::digest(&frame));
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_header_parser_preserves_fields_and_corruption_offset() {
        let frame = encode(7, TerminalReason::MalformedPayload, b"active").unwrap();
        let header = <[u8; HEADER_BYTES]>::try_from(&frame[..HEADER_BYTES]).unwrap();
        let parsed = parse_header(&header, 41).unwrap();
        assert_eq!(parsed.seq, 7);
        assert_eq!(parsed.active_len, 6);
        assert!(matches!(parsed.reason, TerminalReason::MalformedPayload));

        let mut corrupted = header;
        corrupted[7] = 1;
        assert!(matches!(
            parse_header(&corrupted, 41),
            Err(SpoolError::QuarantineCorrupted { at_offset: 41 })
        ));
    }
}
