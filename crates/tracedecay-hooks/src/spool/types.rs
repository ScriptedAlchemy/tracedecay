use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::UtcMicros;

use crate::{
    HookContractError, HookEventEnvelopeV2, HookHostV1, MAX_SPOOL_BYTES_PER_HOST,
    MAX_SPOOL_BYTES_PER_SESSION, MAX_SPOOL_RECORDS_PER_HOST, MAX_SPOOL_RECORDS_PER_SESSION,
};

use super::{FRAME_CHECKSUM_BYTES, FRAME_HEADER_BYTES, FRAME_LENGTH_BYTES};

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
    /// Frames whose framing, SHA-256, and envelope JSON were decoded during
    /// this open. A validated checkpoint contributes zero; only its appended
    /// suffix contributes to this count.
    pub scanned_records: u32,
    /// Fixed-width checkpoint index entries adopted without decoding their
    /// envelope frames during this open.
    pub checkpoint_records: u32,
    /// Bytes read from a checkpoint whose framing, checksum, and index body
    /// were validated during this open.
    pub checkpoint_bytes: u64,
    /// Whether this open durably refreshed the bounded checkpoint anchor.
    pub checkpoint_rewritten: bool,
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
    /// Raw frame bytes, base64-encoded in the JSON meta. A serde byte array
    /// renders as one JSON integer per byte (~4x the frame size), and the
    /// intent is rewritten and fsynced twice per appended event, so the
    /// encoding directly bounds append write amplification. The released
    /// metadata wrote the byte array under this same `SPOOL_META_VERSION`, so
    /// decoding still accepts it and rewrites it as base64 on the next append.
    #[serde(with = "frame_base64")]
    pub(super) frame: Vec<u8>,
}

mod frame_base64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::de::{Error as DeError, SeqAccess, Visitor};
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    /// Accepts the base64 string this build writes and the byte array the
    /// released build wrote. Both yield the exact same frame bytes, which the
    /// caller still validates against the magic, length, sequence, checksum,
    /// and full envelope decode before any recovery decision.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        deserializer.deserialize_any(FrameVisitor)
    }

    struct FrameVisitor;

    impl<'de> Visitor<'de> for FrameVisitor {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a base64 frame string or a released frame byte array")
        }

        fn visit_str<E: DeError>(self, encoded: &str) -> Result<Self::Value, E> {
            STANDARD.decode(encoded.as_bytes()).map_err(E::custom)
        }

        fn visit_bytes<E: DeError>(self, bytes: &[u8]) -> Result<Self::Value, E> {
            Ok(bytes.to_vec())
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut bytes = Vec::new();
            while let Some(byte) = seq.next_element::<u8>()? {
                bytes.push(byte);
            }
            Ok(bytes)
        }
    }
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
    pub(super) records: Vec<PendingRecordV1>,
    pub(super) valid_end: u64,
    pub(super) physical_len: u64,
    pub(super) scanned_records: u32,
    pub(super) partial_tail: Option<Vec<u8>>,
    pub(super) corruption: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingRecordV1 {
    pub(super) sequence: u64,
    pub(super) protected_session_id: [u8; 32],
    pub(super) queued_at: UtcMicros,
    pub(super) framed_len: u32,
    pub(super) file_offset: u64,
    pub(super) event_id: [u8; 16],
    pub(super) checksum: [u8; 32],
    pub(super) envelope: Option<HookEventEnvelopeV2>,
}

impl PendingRecordV1 {
    pub(super) fn from_record(record: &HookSpoolRecordV1, file_offset: u64) -> Self {
        Self {
            sequence: record.sequence,
            protected_session_id: record.protected_session_id,
            queued_at: record.queued_at,
            framed_len: record.framed_len,
            file_offset,
            event_id: record.envelope.event_id,
            checksum: record.checksum,
            envelope: Some(record.envelope.clone()),
        }
    }

    pub(super) fn to_record(&self) -> Option<HookSpoolRecordV1> {
        let frame_overhead =
            u32::try_from(FRAME_LENGTH_BYTES + FRAME_HEADER_BYTES + FRAME_CHECKSUM_BYTES).ok()?;
        Some(HookSpoolRecordV1 {
            sequence: self.sequence,
            protected_session_id: self.protected_session_id,
            queued_at: self.queued_at,
            envelope: self.envelope.clone()?,
            encoded_len: self.framed_len.checked_sub(frame_overhead)?,
            checksum: self.checksum,
            framed_len: self.framed_len,
        })
    }

    pub(super) fn matches_record(&self, record: &HookSpoolRecordV1) -> bool {
        self.sequence == record.sequence
            && self.protected_session_id == record.protected_session_id
            && self.queued_at == record.queued_at
            && self.framed_len == record.framed_len
            && self.event_id == record.envelope.event_id
            && self.checksum == record.checksum
    }
}
