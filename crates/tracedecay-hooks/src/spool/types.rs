use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::UtcMicros;

use crate::{
    HookContractError, HookEventEnvelopeV2, HookHostV1, MAX_SPOOL_BYTES_PER_HOST,
    MAX_SPOOL_BYTES_PER_SESSION, MAX_SPOOL_RECORDS_PER_HOST, MAX_SPOOL_RECORDS_PER_SESSION,
};

/// Per-host and per-session bounds. Callers may narrow these for a host test
/// or constrained installation but can never widen the checked-in limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolLimitsV1 {
    pub max_host_records: u32,
    pub max_host_bytes: u64,
    pub max_session_records: u32,
    pub max_session_bytes: u64,
}

impl HookSpoolLimitsV1 {
    pub const fn stock() -> Self {
        Self {
            max_host_records: MAX_SPOOL_RECORDS_PER_HOST,
            max_host_bytes: MAX_SPOOL_BYTES_PER_HOST,
            max_session_records: MAX_SPOOL_RECORDS_PER_SESSION,
            max_session_bytes: MAX_SPOOL_BYTES_PER_SESSION,
        }
    }

    pub(super) fn validate(self) -> Result<(), HookSpoolError> {
        if self.max_host_records == 0
            || self.max_host_records > MAX_SPOOL_RECORDS_PER_HOST
            || self.max_host_bytes == 0
            || self.max_host_bytes > MAX_SPOOL_BYTES_PER_HOST
            || self.max_session_records == 0
            || self.max_session_records > self.max_host_records
            || self.max_session_records > MAX_SPOOL_RECORDS_PER_SESSION
            || self.max_session_bytes == 0
            || self.max_session_bytes > self.max_host_bytes
            || self.max_session_bytes > MAX_SPOOL_BYTES_PER_SESSION
        {
            return Err(HookSpoolError::InvalidLimits);
        }
        Ok(())
    }
}

/// Configuration owned by the thin host adapter. Time is caller-provided so
/// the spool does not read a clock or invent a product timing policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolConfigV1 {
    pub host: HookHostV1,
    pub limits: HookSpoolLimitsV1,
    pub writer_lease_micros: i64,
}

impl HookSpoolConfigV1 {
    pub const fn stock(host: HookHostV1) -> Self {
        Self {
            host,
            limits: HookSpoolLimitsV1::stock(),
            writer_lease_micros: 5_000_000,
        }
    }

    pub(super) fn validate(self) -> Result<(), HookSpoolError> {
        self.limits.validate()?;
        if self.writer_lease_micros <= 0 {
            return Err(HookSpoolError::InvalidLease);
        }
        Ok(())
    }
}

/// A durable replay record. `envelope` is the exact canonical payload framed
/// on disk; `framed_len` includes the length prefix and checksum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolRecordV1 {
    pub sequence: u64,
    pub protected_session_id: [u8; 32],
    pub queued_at: UtcMicros,
    pub envelope: HookEventEnvelopeV2,
    pub encoded_len: u32,
    pub checksum: [u8; 32],
    pub framed_len: u32,
}

/// The opened spool's bounded recovery report. Corrupt bytes are never
/// discarded unless a matching append intent proves they are an unpublished
/// partial tail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolOpenReportV1 {
    pub pending_records: u32,
    pub pending_bytes: u64,
    pub committed_through: u64,
    pub next_sequence: u64,
    pub truncated_partial_tail_bytes: u64,
    pub corrupted_at_offset: Option<u64>,
}

/// Opaque lease evidence held by exactly one local writer at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolWriterLeaseV1 {
    pub token: [u8; 16],
    pub expires_at: UtcMicros,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookSpoolAckDispositionV1 {
    Committed,
    TerminalTombstone,
}

/// A daemon receipt acknowledgement. The receipt is opaque transport evidence
/// and does not assert a business/application effect by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpoolAckV1 {
    pub sequence: u64,
    pub receipt_id: [u8; 16],
    pub disposition: HookSpoolAckDispositionV1,
}

/// A fair per-session replay lease. The caller reauthorizes every record with
/// the daemon before transmission; one batch never contains two sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookReplayBatchV1 {
    pub claim_id: [u8; 16],
    pub protected_session_id: [u8; 32],
    pub records: Vec<HookSpoolRecordV1>,
    pub byte_count: u32,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HookSpoolResetReasonV1 {
    #[error("metadata revision {found} does not match final revision {expected}")]
    MetadataVersion { found: u16, expected: u16 },
    #[error("metadata does not match the exact final shape")]
    MetadataShape,
    #[error(
        "frame format {found_magic:?}/{found_version} does not match final format {expected_magic:?}/{expected_version}"
    )]
    FrameFormat {
        found_magic: [u8; 4],
        found_version: u16,
        expected_magic: [u8; 4],
        expected_version: u16,
    },
    #[error("envelope revision {found} does not match final revision {expected}")]
    EnvelopeVersion { found: u16, expected: u16 },
    #[error("envelope does not match the exact final shape")]
    EnvelopeShape,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HookSpoolError {
    #[error("hook spool filesystem operation failed")]
    Io,
    #[error("hook spool root or member path is unsafe")]
    UnsafePath,
    #[error("hook spool limits are invalid")]
    InvalidLimits,
    #[error("hook spool writer lease is invalid")]
    InvalidLease,
    #[error("another live hook spool writer owns this host")]
    WriterLeaseHeld,
    #[error("hook spool writer lease was lost or expired")]
    WriterLeaseLost,
    #[error("hook spool must be reset or recreated: {reason}")]
    ResetRequired { reason: HookSpoolResetReasonV1 },
    #[error("hook spool metadata is malformed or internally inconsistent")]
    MetadataCorrupted,
    #[error("hook spool publication was interrupted; reopen is required before another mutation")]
    RecoveryRequired,
    #[error("hook spool frame is corrupt at offset {at_offset}")]
    Corrupted { at_offset: u64 },
    #[error("hook spool record exceeds the bounded payload limit")]
    RecordTooLarge,
    #[error("hook spool quota is full")]
    SpoolFull,
    #[error("hook envelope is invalid for the supplied daemon binding")]
    EnvelopeRejected(HookContractError),
    #[error("hook event ID conflicts with a different pending envelope")]
    EventIdConflict,
    #[error("hook spool acknowledgement is unknown or conflicts with prior receipt evidence")]
    AckConflict,
    #[error("hook spool replay claim is unknown")]
    ReplayClaimUnknown,
    #[error("hook spool replay batch exceeds a checked-in bound")]
    ReplayBatchExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HookSpoolMetaV1 {
    pub(super) version: u16,
    pub(super) committed_through: u64,
    pub(super) next_sequence: u64,
    pub(super) acknowledged: Vec<AcknowledgedSequenceV1>,
    pub(super) integrity: SpoolIntegrityV1,
    pub(super) append_intent: Option<AppendIntentV1>,
}

impl HookSpoolMetaV1 {
    pub(super) const fn fresh() -> Self {
        Self {
            version: super::SPOOL_META_VERSION,
            committed_through: 0,
            next_sequence: 1,
            acknowledged: Vec::new(),
            integrity: SpoolIntegrityV1::Healthy,
            append_intent: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SpoolIntegrityV1 {
    Healthy,
    Corrupted { at_offset: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AppendIntentV1 {
    pub(super) sequence: u64,
    pub(super) file_offset: u64,
    pub(super) framed_len: u32,
    pub(super) frame: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AcknowledgedSequenceV1 {
    pub(super) sequence: u64,
    pub(super) receipt_id: [u8; 16],
    pub(super) disposition: HookSpoolAckDispositionV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LeaseFileV1 {
    pub(super) version: u16,
    pub(super) token: [u8; 16],
    pub(super) expires_at: UtcMicros,
}

#[derive(Debug)]
pub(super) struct ScanResult {
    pub(super) records: Vec<HookSpoolRecordV1>,
    pub(super) valid_end: u64,
    pub(super) physical_len: u64,
    pub(super) partial_tail: Option<Vec<u8>>,
    pub(super) corruption: Option<u64>,
}
