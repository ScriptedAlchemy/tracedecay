//! Canonical Remote Brain replay frame and receipt contracts.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{ManifestDigest, RemoteWriterFenceV1, UtcMicros, canonical_sha256};

use super::capture::{AdmittedRemoteCaptureV1, RemoteWriterAuthorityV1};
use super::protocol::RemoteProtocolBodyV1;
use super::replay::RemoteReplayApplicationErrorV1;
use crate::{ApplicationContractError, OperationBudgetUsage};

/// One node-owned spool frame submitted to the authenticated authority.
///
/// The authority never opens a node's local spool. The node decrypts the
/// sanitized frame, binds its current replay attempt, and sends that canonical
/// value over mutual TLS.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayRequestV1 {
    pub frame: RemoteReplayFrameV1,
    pub replay_attempt: u64,
}

impl RemoteReplayRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.frame
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "remote replay frame",
            })?;
        if self.replay_attempt == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "remote replay attempt",
            });
        }
        Ok(())
    }
}

impl RemoteProtocolBodyV1 for RemoteReplayRequestV1 {
    fn validate_remote_protocol_body(&self) -> Result<(), ApplicationContractError> {
        self.validate()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayFrameV1 {
    pub event_id: String,
    pub capture: AdmittedRemoteCaptureV1,
}

impl RemoteReplayFrameV1 {
    pub fn validate(&self) -> Result<(), RemoteReplayApplicationErrorV1> {
        let event_identity_matches = canonical_remote_event_id_v1(&self.capture)
            .is_ok_and(|event_id| event_id == self.event_id);
        if !valid_remote_event_id(&self.event_id)
            || !event_identity_matches
            || self.capture.enrollment_revision == 0
            || self.capture.policy_revision == 0
        {
            return Err(RemoteReplayApplicationErrorV1::InvalidFrame);
        }
        self.capture
            .writer
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::InvalidFrame)?;
        self.capture
            .sequence
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::InvalidFrame)
    }
}

pub fn canonical_remote_event_id_v1(
    capture: &AdmittedRemoteCaptureV1,
) -> Result<String, ApplicationContractError> {
    let digest = canonical_sha256(capture)?;
    Ok(format!(
        "remote.event.{}",
        digest
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or(digest.as_str())
    ))
}

fn valid_remote_event_id(event_id: &str) -> bool {
    (16..=160).contains(&event_id.len())
        && event_id.trim() == event_id
        && !event_id.chars().any(char::is_control)
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteReplayStateV1 {
    Pending,
    Admitted,
    Duplicate,
    Acknowledged,
    Rejected,
    Quarantined,
    GarbageCollectionEligible,
}

impl RemoteReplayStateV1 {
    pub const fn permits_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Pending,
                Self::Admitted | Self::Duplicate | Self::Rejected | Self::Quarantined
            ) | (Self::Admitted | Self::Duplicate, Self::Acknowledged)
                | (Self::Acknowledged, Self::GarbageCollectionEligible)
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteReplayFindingV1 {
    EnrollmentRevoked,
    PolicyChanged,
    LostAcknowledgement,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayCommitReceiptV1 {
    pub event_id: String,
    pub writer_fence: RemoteWriterFenceV1,
    pub commit_sequence: u64,
    pub committed_at: UtcMicros,
    pub budget: OperationBudgetUsage,
}

impl RemoteReplayCommitReceiptV1 {
    pub fn validate_for(
        &self,
        frame: &RemoteReplayFrameV1,
        current_writer: &RemoteWriterAuthorityV1,
    ) -> Result<(), RemoteReplayApplicationErrorV1> {
        if self.event_id != frame.event_id
            || self.writer_fence != current_writer.authority.fence
            || self.commit_sequence == 0
            || self.committed_at < frame.capture.captured_at
            || self.budget.units_consumed == 0
            || self.budget.bytes_consumed == 0
        {
            return Err(RemoteReplayApplicationErrorV1::ReceiptMismatch);
        }
        self.writer_fence
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplaySpoolStateV1 {
    pub state: RemoteReplayStateV1,
    pub receipt: Option<RemoteReplayCommitReceiptV1>,
    pub last_attempt: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayTransitionV1 {
    pub event_id: String,
    pub from: RemoteReplayStateV1,
    pub to: RemoteReplayStateV1,
    pub replay_attempt: u64,
    pub observed_at: UtcMicros,
    pub finding: Option<RemoteReplayFindingV1>,
    pub receipt: Option<RemoteReplayCommitReceiptV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayTransitionReceiptV1 {
    pub event_id: String,
    pub replay_attempt: u64,
    pub from: RemoteReplayStateV1,
    pub to: RemoteReplayStateV1,
    pub pre_state_digest: ManifestDigest,
    pub terminal_state_digest: ManifestDigest,
    pub committed_at: UtcMicros,
    pub budget: OperationBudgetUsage,
}

impl RemoteReplayTransitionReceiptV1 {
    pub fn validate_for(
        &self,
        transition: &RemoteReplayTransitionV1,
    ) -> Result<(), RemoteReplayApplicationErrorV1> {
        if self.event_id != transition.event_id
            || self.replay_attempt != transition.replay_attempt
            || self.from != transition.from
            || self.to != transition.to
            || self.committed_at < transition.observed_at
            || self.budget.units_consumed == 0
            || self.budget.bytes_consumed == 0
        {
            return Err(RemoteReplayApplicationErrorV1::ReceiptMismatch);
        }
        self.pre_state_digest
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)?;
        self.terminal_state_digest
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayOperationReceiptV1 {
    pub event_id: String,
    pub replay_attempt: u64,
    pub pre_state_digest: ManifestDigest,
    pub terminal_state_digest: ManifestDigest,
    pub committed_effect_digest: ManifestDigest,
    pub started_at: UtcMicros,
    pub committed_at: UtcMicros,
    pub budget: OperationBudgetUsage,
    pub transaction: Option<RemoteReplayCommitReceiptV1>,
}

impl RemoteReplayOperationReceiptV1 {
    pub fn validate(&self) -> Result<(), RemoteReplayApplicationErrorV1> {
        if self.replay_attempt == 0
            || self.started_at > self.committed_at
            || self.budget.units_consumed == 0
            || self.budget.bytes_consumed == 0
        {
            return Err(RemoteReplayApplicationErrorV1::ReceiptMismatch);
        }
        self.pre_state_digest
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)?;
        self.terminal_state_digest
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)?;
        let expected_effect = if let Some(transaction) = &self.transaction {
            canonical_sha256(transaction)
                .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)?
        } else {
            self.terminal_state_digest.clone()
        };
        if self.committed_effect_digest != expected_effect {
            return Err(RemoteReplayApplicationErrorV1::ReceiptMismatch);
        }
        Ok(())
    }
}

impl RemoteReplayTransitionV1 {
    pub fn validate(&self) -> Result<(), RemoteReplayApplicationErrorV1> {
        if self.replay_attempt == 0 || !self.from.permits_transition_to(self.to) {
            return Err(RemoteReplayApplicationErrorV1::InvalidSpoolState);
        }
        let receipt_required = matches!(
            self.to,
            RemoteReplayStateV1::Admitted
                | RemoteReplayStateV1::Duplicate
                | RemoteReplayStateV1::Acknowledged
                | RemoteReplayStateV1::GarbageCollectionEligible
        );
        if receipt_required != self.receipt.is_some() {
            return Err(RemoteReplayApplicationErrorV1::ReceiptMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_frame_rejects_noncanonical_event_identity() {
        assert!(!valid_remote_event_id("short"));
        assert!(valid_remote_event_id(
            "remote.event.sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
    }

    #[test]
    fn replay_state_machine_preserves_acknowledgement_boundary() {
        assert!(RemoteReplayStateV1::Pending.permits_transition_to(RemoteReplayStateV1::Admitted));
        assert!(
            RemoteReplayStateV1::Duplicate.permits_transition_to(RemoteReplayStateV1::Acknowledged)
        );
        assert!(
            RemoteReplayStateV1::Acknowledged
                .permits_transition_to(RemoteReplayStateV1::GarbageCollectionEligible)
        );
        assert!(
            !RemoteReplayStateV1::Pending
                .permits_transition_to(RemoteReplayStateV1::GarbageCollectionEligible)
        );
    }
}
