//! Store-facing contracts for PR16 remote offline capture and replay.
//!
//! These contracts accept only the domain's receipt-bound durable observation.
//! Raw host frames, overlays, analyzer state, credentials, and unsanitized bytes
//! have no representation here.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{
    BrainNodeId, ComponentVersion, DurableObservationV1, EntityId, ManifestDigest, PayloadDigestV1,
    ProjectId, RemoteRepositoryScopeV1, RemoteWriterFenceV1, UtcMicros, canonical_sha256,
};

use crate::{StoreCommitReceiptV1, StoreRuntimeBindingV1};

pub const REMOTE_CAPTURE_SCHEMA_V1: u16 = 1;

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct RemoteCaptureEventIdV1(String);

impl RemoteCaptureEventIdV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, RemoteCaptureContractErrorV1> {
        let value = value.into();
        if value.len() < 16
            || value.len() > 160
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(RemoteCaptureContractErrorV1::InvalidEventId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RemoteCaptureEventIdV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteRepositoryIdentityV1 {
    pub project_id: ProjectId,
    pub scope: RemoteRepositoryScopeV1,
}

impl RemoteRepositoryIdentityV1 {
    pub fn validate(&self) -> Result<(), RemoteCaptureContractErrorV1> {
        self.project_id
            .validate()
            .map_err(|_| RemoteCaptureContractErrorV1::InvalidRepositoryIdentity)?;
        self.scope
            .validate()
            .map_err(|_| RemoteCaptureContractErrorV1::InvalidRepositoryIdentity)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteEnrollmentIdentityV1 {
    pub enrollment_id: EntityId,
    pub enrollment_revision: u64,
    pub node_id: BrainNodeId,
    pub repository: RemoteRepositoryIdentityV1,
}

impl RemoteEnrollmentIdentityV1 {
    pub fn validate(&self) -> Result<(), RemoteCaptureContractErrorV1> {
        if self.enrollment_revision == 0 {
            return Err(RemoteCaptureContractErrorV1::Zero {
                field: "enrollment revision",
            });
        }
        self.enrollment_id
            .validate()
            .map_err(|_| RemoteCaptureContractErrorV1::InvalidEnrollmentIdentity)?;
        self.node_id
            .validate()
            .map_err(|_| RemoteCaptureContractErrorV1::InvalidNodeIdentity)?;
        self.repository.validate()
    }
}

/// Current single-writer fence at capture or replay time.
///
/// The domain fence is the exact PR16 Brain/shard/generation/placement/epoch
/// key. `runtime` is the already-admitted production storage binding.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteWriterBindingV1 {
    pub fence: RemoteWriterFenceV1,
    pub runtime: StoreRuntimeBindingV1,
}

impl RemoteWriterBindingV1 {
    pub fn validate(&self) -> Result<(), RemoteCaptureContractErrorV1> {
        self.fence
            .validate()
            .map_err(|_| RemoteCaptureContractErrorV1::InvalidWriterFence)?;
        if !self.runtime.shard_id.is_mutable() {
            return Err(RemoteCaptureContractErrorV1::ImmutableShard);
        }
        if self.fence.brain_id != self.runtime.shard_id.brain_id
            || self.fence.authority_epoch.0 != self.runtime.authority_epoch.get()
        {
            return Err(RemoteCaptureContractErrorV1::InvalidWriterFence);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteCaptureSequenceV1 {
    pub sequence: u64,
    pub previous_event_id: Option<RemoteCaptureEventIdV1>,
}

impl RemoteCaptureSequenceV1 {
    pub fn new(
        sequence: u64,
        previous_event_id: Option<RemoteCaptureEventIdV1>,
    ) -> Result<Self, RemoteCaptureContractErrorV1> {
        if sequence == 0 {
            return Err(RemoteCaptureContractErrorV1::Zero {
                field: "capture sequence",
            });
        }
        if (sequence == 1) != previous_event_id.is_none() {
            return Err(RemoteCaptureContractErrorV1::InvalidCausalEvidence);
        }
        Ok(Self {
            sequence,
            previous_event_id,
        })
    }

    pub fn validate(&self) -> Result<(), RemoteCaptureContractErrorV1> {
        Self::new(self.sequence, self.previous_event_id.clone()).map(|_| ())
    }
}

/// One canonical, already-sanitized offline observation.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteCaptureFrameV1 {
    pub schema_version: u16,
    pub event_id: RemoteCaptureEventIdV1,
    pub enrollment: RemoteEnrollmentIdentityV1,
    pub sequence: RemoteCaptureSequenceV1,
    pub sanitizer_revision: ComponentVersion,
    pub payload_length: u64,
    pub payload_digest: PayloadDigestV1,
    pub captured_at: UtcMicros,
    pub policy_revision: u64,
    pub captured_writer: RemoteWriterBindingV1,
    pub observation: DurableObservationV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteCaptureFrameWireV1 {
    schema_version: u16,
    event_id: RemoteCaptureEventIdV1,
    enrollment: RemoteEnrollmentIdentityV1,
    sequence: RemoteCaptureSequenceV1,
    sanitizer_revision: ComponentVersion,
    payload_length: u64,
    payload_digest: PayloadDigestV1,
    captured_at: UtcMicros,
    policy_revision: u64,
    captured_writer: RemoteWriterBindingV1,
    observation: DurableObservationV1,
}

impl RemoteCaptureFrameV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        enrollment: RemoteEnrollmentIdentityV1,
        sequence: RemoteCaptureSequenceV1,
        captured_at: UtcMicros,
        policy_revision: u64,
        captured_writer: RemoteWriterBindingV1,
        observation: DurableObservationV1,
    ) -> Result<Self, RemoteCaptureContractErrorV1> {
        enrollment.validate()?;
        sequence.validate()?;
        captured_writer.validate()?;
        if policy_revision == 0 {
            return Err(RemoteCaptureContractErrorV1::Zero {
                field: "policy revision",
            });
        }
        let sanitizer_revision = observation.receipt().receipt().sanitizer_version().clone();
        let payload = observation
            .canonical_payload_bytes()
            .map_err(|_| RemoteCaptureContractErrorV1::InvalidCanonicalObservation)?;
        let payload_length = u64::try_from(payload.len())
            .map_err(|_| RemoteCaptureContractErrorV1::PayloadTooLarge)?;
        if payload_length == 0 {
            return Err(RemoteCaptureContractErrorV1::Zero {
                field: "canonical payload length",
            });
        }
        let payload_digest = observation.payload_reference().digest().clone();
        let event_id = derive_event_id(
            &enrollment,
            &sequence,
            &sanitizer_revision,
            payload_length,
            &payload_digest,
            &captured_writer,
        )?;
        let frame = Self {
            schema_version: REMOTE_CAPTURE_SCHEMA_V1,
            event_id,
            enrollment,
            sequence,
            sanitizer_revision,
            payload_length,
            payload_digest,
            captured_at,
            policy_revision,
            captured_writer,
            observation,
        };
        frame.validate()?;
        Ok(frame)
    }

    pub fn validate(&self) -> Result<(), RemoteCaptureContractErrorV1> {
        if self.schema_version != REMOTE_CAPTURE_SCHEMA_V1 {
            return Err(RemoteCaptureContractErrorV1::UnsupportedSchema);
        }
        self.enrollment.validate()?;
        self.sequence.validate()?;
        self.captured_writer.validate()?;
        if self.policy_revision == 0 || self.payload_length == 0 {
            return Err(RemoteCaptureContractErrorV1::Zero {
                field: "frame revision or payload length",
            });
        }
        let expected_revision = self.observation.receipt().receipt().sanitizer_version();
        let payload = self
            .observation
            .canonical_payload_bytes()
            .map_err(|_| RemoteCaptureContractErrorV1::InvalidCanonicalObservation)?;
        let expected_length = u64::try_from(payload.len())
            .map_err(|_| RemoteCaptureContractErrorV1::PayloadTooLarge)?;
        if expected_revision != &self.sanitizer_revision
            || expected_length != self.payload_length
            || self.observation.payload_reference().digest() != &self.payload_digest
        {
            return Err(RemoteCaptureContractErrorV1::PayloadBindingMismatch);
        }
        let expected_event_id = derive_event_id(
            &self.enrollment,
            &self.sequence,
            &self.sanitizer_revision,
            self.payload_length,
            &self.payload_digest,
            &self.captured_writer,
        )?;
        if expected_event_id != self.event_id {
            return Err(RemoteCaptureContractErrorV1::EventIdentityMismatch);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RemoteCaptureFrameV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RemoteCaptureFrameWireV1::deserialize(deserializer)?;
        let frame = Self {
            schema_version: wire.schema_version,
            event_id: wire.event_id,
            enrollment: wire.enrollment,
            sequence: wire.sequence,
            sanitizer_revision: wire.sanitizer_revision,
            payload_length: wire.payload_length,
            payload_digest: wire.payload_digest,
            captured_at: wire.captured_at,
            policy_revision: wire.policy_revision,
            captured_writer: wire.captured_writer,
            observation: wire.observation,
        };
        frame.validate().map_err(serde::de::Error::custom)?;
        Ok(frame)
    }
}

fn derive_event_id(
    enrollment: &RemoteEnrollmentIdentityV1,
    sequence: &RemoteCaptureSequenceV1,
    sanitizer_revision: &ComponentVersion,
    payload_length: u64,
    payload_digest: &PayloadDigestV1,
    writer: &RemoteWriterBindingV1,
) -> Result<RemoteCaptureEventIdV1, RemoteCaptureContractErrorV1> {
    let digest: ManifestDigest = canonical_sha256(&(
        "tracedecay.remote-capture.event.v1",
        enrollment,
        sequence,
        sanitizer_revision,
        payload_length,
        payload_digest,
        writer,
    ))
    .map_err(|_| RemoteCaptureContractErrorV1::EventIdentityEncoding)?;
    RemoteCaptureEventIdV1::new(format!("remote.event.{}", digest.as_str()))
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCaptureStateV1 {
    Captured,
    Pending,
    Admitted,
    Duplicate,
    Rejected,
    Quarantined,
    Acknowledged,
    GarbageCollectionEligible,
}

impl RemoteCaptureStateV1 {
    pub fn permits_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (
                    Self::Captured,
                    Self::Pending | Self::Rejected | Self::Quarantined
                ) | (
                    Self::Pending,
                    Self::Admitted | Self::Duplicate | Self::Rejected | Self::Quarantined
                ) | (Self::Rejected | Self::Quarantined, Self::Pending)
                    | (Self::Admitted | Self::Duplicate, Self::Acknowledged)
                    | (Self::Acknowledged, Self::GarbageCollectionEligible)
            )
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCaptureFindingV1 {
    Overflow,
    Corruption,
    SequenceGap,
    LostAcknowledgement,
    EnrollmentRevoked,
    PolicyChanged,
    ReplayRejected,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteCaptureTransitionV1 {
    pub event_id: RemoteCaptureEventIdV1,
    pub from: RemoteCaptureStateV1,
    pub to: RemoteCaptureStateV1,
    pub replay_attempt: u64,
    pub observed_at: UtcMicros,
    pub finding: Option<RemoteCaptureFindingV1>,
    pub receipt: Option<StoreCommitReceiptV1>,
}

impl RemoteCaptureTransitionV1 {
    pub fn validate(&self) -> Result<(), RemoteCaptureContractErrorV1> {
        if !self.from.permits_transition_to(self.to) {
            return Err(RemoteCaptureContractErrorV1::InvalidStateTransition);
        }
        let requires_receipt = matches!(
            self.to,
            RemoteCaptureStateV1::Admitted
                | RemoteCaptureStateV1::Duplicate
                | RemoteCaptureStateV1::Acknowledged
                | RemoteCaptureStateV1::GarbageCollectionEligible
        );
        if requires_receipt != self.receipt.is_some() {
            return Err(RemoteCaptureContractErrorV1::ReceiptStateMismatch);
        }
        if matches!(
            self.to,
            RemoteCaptureStateV1::Admitted
                | RemoteCaptureStateV1::Duplicate
                | RemoteCaptureStateV1::Rejected
                | RemoteCaptureStateV1::Quarantined
        ) && self.replay_attempt == 0
        {
            return Err(RemoteCaptureContractErrorV1::Zero {
                field: "replay attempt",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteCaptureContractErrorV1 {
    Zero { field: &'static str },
    InvalidEventId,
    InvalidEnrollmentIdentity,
    InvalidNodeIdentity,
    InvalidRepositoryIdentity,
    InvalidCausalEvidence,
    ImmutableShard,
    InvalidWriterFence,
    UnsupportedSchema,
    InvalidCanonicalObservation,
    PayloadTooLarge,
    PayloadBindingMismatch,
    EventIdentityEncoding,
    EventIdentityMismatch,
    InvalidStateTransition,
    ReceiptStateMismatch,
}

impl fmt::Display for RemoteCaptureContractErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero { field } => write!(formatter, "{field} must be non-zero"),
            Self::InvalidEventId => formatter.write_str("remote event identity is not canonical"),
            Self::InvalidEnrollmentIdentity => {
                formatter.write_str("remote enrollment identity is invalid")
            }
            Self::InvalidNodeIdentity => formatter.write_str("remote node identity is invalid"),
            Self::InvalidRepositoryIdentity => {
                formatter.write_str("remote repository identity is invalid")
            }
            Self::InvalidCausalEvidence => {
                formatter.write_str("capture sequence has invalid causal evidence")
            }
            Self::ImmutableShard => formatter.write_str("remote writer shard is immutable"),
            Self::InvalidWriterFence => {
                formatter.write_str("remote writer fence does not match storage binding")
            }
            Self::UnsupportedSchema => formatter.write_str("remote capture schema is unsupported"),
            Self::InvalidCanonicalObservation => {
                formatter.write_str("canonical observation is invalid")
            }
            Self::PayloadTooLarge => formatter.write_str("canonical payload length overflowed"),
            Self::PayloadBindingMismatch => {
                formatter.write_str("canonical payload evidence does not match the observation")
            }
            Self::EventIdentityEncoding => {
                formatter.write_str("remote event identity could not be encoded")
            }
            Self::EventIdentityMismatch => {
                formatter.write_str("remote event identity does not match frame contents")
            }
            Self::InvalidStateTransition => {
                formatter.write_str("remote capture state transition is invalid")
            }
            Self::ReceiptStateMismatch => {
                formatter.write_str("remote capture state has invalid receipt evidence")
            }
        }
    }
}

impl std::error::Error for RemoteCaptureContractErrorV1 {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_requires_exact_predecessor_shape() {
        let previous = RemoteCaptureEventIdV1::new("remote.event.previous").unwrap();
        assert!(RemoteCaptureSequenceV1::new(1, None).is_ok());
        assert!(RemoteCaptureSequenceV1::new(2, Some(previous)).is_ok());
        assert_eq!(
            RemoteCaptureSequenceV1::new(2, None),
            Err(RemoteCaptureContractErrorV1::InvalidCausalEvidence)
        );
    }

    #[test]
    fn garbage_collection_requires_durable_acknowledgement_first() {
        assert!(
            !RemoteCaptureStateV1::Pending
                .permits_transition_to(RemoteCaptureStateV1::GarbageCollectionEligible)
        );
        assert!(
            RemoteCaptureStateV1::Acknowledged
                .permits_transition_to(RemoteCaptureStateV1::GarbageCollectionEligible)
        );
    }
}
