use serde::{Deserialize, Serialize};

use super::super::{HostAdmissionOutcome, HostAdmissionStatus};
use super::SpoolOverflowDisposition;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpoolIntegrity {
    Healthy,
    Corrupted { at_offset: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalReason {
    MalformedPayload,
    StaleBranchAuthorization,
}

impl TerminalReason {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::MalformedPayload => 1,
            Self::StaleBranchAuthorization => 3,
        }
    }

    pub(crate) const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::MalformedPayload),
            3 => Some(Self::StaleBranchAuthorization),
            _ => None,
        }
    }
}

/// Stable internal errors. No variant contains a path, provider payload, or raw
/// parser/OS error string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpoolError {
    Io,
    Overflow(SpoolOverflowDisposition),
    Corrupted { at_offset: u64 },
    MetadataCorrupted,
    UnsupportedVersion(u16),
    AckOutOfOrder { expected: u64, got: u64 },
    AckUnknown { seq: u64 },
    AppendRecoveryRequired,
    QuarantineFull,
    QuarantineCorrupted { at_offset: u64 },
    QuarantineRecoveryRequired,
}

impl SpoolError {
    pub(crate) const fn to_outcome(&self) -> HostAdmissionOutcome {
        match self {
            Self::Overflow(SpoolOverflowDisposition::RecordTooLarge) => {
                HostAdmissionOutcome::spool_record_too_large()
            }
            Self::Overflow(SpoolOverflowDisposition::SourceTooLarge) => {
                HostAdmissionOutcome::spool_source_too_large()
            }
            Self::Overflow(
                SpoolOverflowDisposition::MaxBytes
                | SpoolOverflowDisposition::MaxRecords
                | SpoolOverflowDisposition::MaxBytesPerSource
                | SpoolOverflowDisposition::MaxRecordsPerSource,
            ) => HostAdmissionOutcome::spool_overflow(),
            Self::UnsupportedVersion(_) => HostAdmissionOutcome::spool_unsupported_version(),
            Self::Corrupted { .. } | Self::MetadataCorrupted => {
                HostAdmissionOutcome::spool_corrupted()
            }
            Self::AckOutOfOrder { .. } | Self::AckUnknown { .. } => {
                HostAdmissionOutcome::spool_ack_conflict()
            }
            Self::AppendRecoveryRequired => HostAdmissionOutcome::spool_recovery_required(),
            Self::QuarantineFull => HostAdmissionOutcome::quarantine_full(),
            Self::QuarantineCorrupted { .. } => HostAdmissionOutcome::quarantine_corrupted(),
            Self::QuarantineRecoveryRequired => {
                HostAdmissionOutcome::quarantine_recovery_required()
            }
            Self::Io => HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("spool_io_failed"),
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpoolRecord {
    pub seq: u64,
    pub source: String,
    pub payload: Vec<u8>,
    pub(crate) file_offset: u64,
    pub(crate) framed_len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpoolOpenReport {
    pub(crate) pending_records: usize,
    pub(crate) truncated_partial_tail_bytes: u64,
    pub(crate) integrity: SpoolIntegrity,
    pub(crate) committed_through: u64,
    pub(crate) next_seq: u64,
    pub(crate) quarantined_records: usize,
    pub(crate) quarantine_bytes: usize,
    pub(crate) quarantine_truncated_partial_tail_bytes: u64,
}
