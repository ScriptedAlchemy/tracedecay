//! Authenticated, scope-exact orchestration for replaying admitted captures.
//!
//! Store runtime bindings and SQL receipts stay behind the adapter ports. The
//! application carries only canonical capture identity and a bounded replay
//! receipt suitable for validation and status reporting.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    EnrollmentCredentialRecordV1, RemoteCapabilityV1, RemoteWriterFenceV1, UtcMicros,
};

use super::auth::{
    OpaqueRemoteCredential, RemoteAuthenticationError, RemoteAuthorityAuthenticationPort,
    authenticate_remote_request,
};
use super::capture::{
    AdmittedRemoteCaptureV1, RemoteCapturePersistenceErrorV1, RemoteWriterAuthorityV1,
};
use super::protocol::RemoteProtocolBodyV1;
use crate::ApplicationContractError;

/// Secret-free replay selector. The authority loads the canonical admitted
/// capture from its encrypted spool; callers cannot resubmit or alter payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayRequestV1 {
    pub event_id: String,
}

impl RemoteReplayRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.event_id.len() < 16
            || self.event_id.len() > 160
            || self.event_id.trim() != self.event_id
            || self.event_id.chars().any(char::is_control)
        {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "remote replay event id",
            });
        }
        Ok(())
    }
}

impl RemoteProtocolBodyV1 for RemoteReplayRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        _sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        self.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteReplayFrameV1 {
    pub event_id: String,
    pub capture: AdmittedRemoteCaptureV1,
}

impl RemoteReplayFrameV1 {
    pub fn validate(&self) -> Result<(), RemoteReplayApplicationErrorV1> {
        if self.event_id.len() < 16
            || self.event_id.len() > 160
            || self.event_id.trim() != self.event_id
            || self.event_id.chars().any(char::is_control)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteReplayPolicyDecisionV1 {
    Admit,
    Reject,
    Quarantine,
}

pub trait RemoteReplayPolicyPortV1: Send + Sync {
    fn authorize_current_policy(
        &self,
        frame: &RemoteReplayFrameV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteReplayPolicyDecisionV1, RemoteReplayApplicationErrorV1>;
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
        {
            return Err(RemoteReplayApplicationErrorV1::ReceiptMismatch);
        }
        self.writer_fence
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteReplaySpoolStateV1 {
    pub state: RemoteReplayStateV1,
    pub receipt: Option<RemoteReplayCommitReceiptV1>,
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

pub trait RemoteReplaySpoolPortV1: Send + Sync {
    fn state(
        &self,
        event_id: &str,
    ) -> Result<RemoteReplaySpoolStateV1, RemoteCapturePersistenceErrorV1>;

    fn transition(
        &self,
        transition: RemoteReplayTransitionV1,
    ) -> Result<(), RemoteCapturePersistenceErrorV1>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteReplayTransactionOutcomeV1 {
    Admitted(RemoteReplayCommitReceiptV1),
    Duplicate(RemoteReplayCommitReceiptV1),
}

pub trait RemoteReplayTransactionPortV1: Send + Sync {
    fn commit(
        &self,
        frame: &RemoteReplayFrameV1,
        current_writer: &RemoteWriterAuthorityV1,
    ) -> Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1>;
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum RemoteReplayOutcomeV1 {
    Acknowledged {
        disposition: RemoteReplayStateV1,
        receipt: RemoteReplayCommitReceiptV1,
    },
    Rejected,
    Quarantined,
}

#[allow(clippy::too_many_arguments)]
pub fn replay_remote_capture(
    authentication: &dyn RemoteAuthorityAuthenticationPort,
    policy: &dyn RemoteReplayPolicyPortV1,
    transaction: &dyn RemoteReplayTransactionPortV1,
    spool: &dyn RemoteReplaySpoolPortV1,
    authority_credential: &EnrollmentCredentialRecordV1,
    caller_credential: &EnrollmentCredentialRecordV1,
    presented_caller_credential: &OpaqueRemoteCredential,
    frame: &RemoteReplayFrameV1,
    current_writer: &RemoteWriterAuthorityV1,
    replay_attempt: u64,
    observed_at: UtcMicros,
) -> Result<RemoteReplayOutcomeV1, RemoteReplayApplicationErrorV1> {
    if replay_attempt == 0 {
        return Err(RemoteReplayApplicationErrorV1::InvalidReplayAttempt);
    }
    validate_scope_and_fence(frame, current_writer, caller_credential)?;
    if let Err(error) = authenticate_remote_request(
        authentication,
        &current_writer.authority,
        authority_credential,
        caller_credential,
        presented_caller_credential,
        RemoteCapabilityV1::Replay,
        &frame.capture.writer.scope,
        observed_at,
    ) {
        if error == RemoteAuthenticationError::Revoked
            && spool
                .state(&frame.event_id)
                .map_err(RemoteReplayApplicationErrorV1::Persistence)?
                .state
                == RemoteReplayStateV1::Pending
        {
            transition(
                spool,
                frame,
                RemoteReplayStateV1::Pending,
                RemoteReplayStateV1::Rejected,
                replay_attempt,
                observed_at,
                Some(RemoteReplayFindingV1::EnrollmentRevoked),
                None,
            )?;
        }
        return Err(RemoteReplayApplicationErrorV1::Authentication(error));
    }

    let spool_state = spool
        .state(&frame.event_id)
        .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
    if matches!(
        spool_state.state,
        RemoteReplayStateV1::Admitted | RemoteReplayStateV1::Duplicate
    ) {
        let receipt = spool_state
            .receipt
            .ok_or(RemoteReplayApplicationErrorV1::ReceiptMissing)?;
        receipt.validate_for(frame, current_writer)?;
        acknowledge(
            spool,
            frame,
            spool_state.state,
            replay_attempt,
            observed_at,
            receipt.clone(),
        )?;
        return Ok(RemoteReplayOutcomeV1::Acknowledged {
            disposition: spool_state.state,
            receipt,
        });
    }
    if spool_state.state != RemoteReplayStateV1::Pending {
        return Err(RemoteReplayApplicationErrorV1::InvalidSpoolState);
    }

    match policy.authorize_current_policy(frame, observed_at)? {
        RemoteReplayPolicyDecisionV1::Reject => {
            transition(
                spool,
                frame,
                RemoteReplayStateV1::Pending,
                RemoteReplayStateV1::Rejected,
                replay_attempt,
                observed_at,
                Some(RemoteReplayFindingV1::PolicyChanged),
                None,
            )?;
            return Ok(RemoteReplayOutcomeV1::Rejected);
        }
        RemoteReplayPolicyDecisionV1::Quarantine => {
            transition(
                spool,
                frame,
                RemoteReplayStateV1::Pending,
                RemoteReplayStateV1::Quarantined,
                replay_attempt,
                observed_at,
                Some(RemoteReplayFindingV1::PolicyChanged),
                None,
            )?;
            return Ok(RemoteReplayOutcomeV1::Quarantined);
        }
        RemoteReplayPolicyDecisionV1::Admit => {}
    }

    let (disposition, receipt, finding) = match transaction
        .commit(frame, current_writer)
        .map_err(RemoteReplayApplicationErrorV1::Transaction)?
    {
        RemoteReplayTransactionOutcomeV1::Admitted(receipt) => {
            (RemoteReplayStateV1::Admitted, receipt, None)
        }
        RemoteReplayTransactionOutcomeV1::Duplicate(receipt) => (
            RemoteReplayStateV1::Duplicate,
            receipt,
            Some(RemoteReplayFindingV1::LostAcknowledgement),
        ),
    };
    receipt.validate_for(frame, current_writer)?;
    transition(
        spool,
        frame,
        RemoteReplayStateV1::Pending,
        disposition,
        replay_attempt,
        observed_at,
        finding,
        Some(receipt.clone()),
    )?;
    acknowledge(
        spool,
        frame,
        disposition,
        replay_attempt,
        observed_at,
        receipt.clone(),
    )?;
    Ok(RemoteReplayOutcomeV1::Acknowledged {
        disposition,
        receipt,
    })
}

pub fn mark_remote_capture_gc_eligible(
    spool: &dyn RemoteReplaySpoolPortV1,
    frame: &RemoteReplayFrameV1,
    receipt: RemoteReplayCommitReceiptV1,
    replay_attempt: u64,
    observed_at: UtcMicros,
) -> Result<(), RemoteReplayApplicationErrorV1> {
    let state = spool
        .state(&frame.event_id)
        .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
    if state.state != RemoteReplayStateV1::Acknowledged || state.receipt.as_ref() != Some(&receipt)
    {
        return Err(RemoteReplayApplicationErrorV1::InvalidSpoolState);
    }
    transition(
        spool,
        frame,
        RemoteReplayStateV1::Acknowledged,
        RemoteReplayStateV1::GarbageCollectionEligible,
        replay_attempt,
        observed_at,
        None,
        Some(receipt),
    )
}

fn validate_scope_and_fence(
    frame: &RemoteReplayFrameV1,
    current_writer: &RemoteWriterAuthorityV1,
    caller: &EnrollmentCredentialRecordV1,
) -> Result<(), RemoteReplayApplicationErrorV1> {
    frame.validate()?;
    current_writer
        .validate()
        .map_err(|_| RemoteReplayApplicationErrorV1::FenceMismatch)?;
    let captured = &frame.capture.writer;
    if !captured
        .authority
        .fence
        .same_mutable_shard(&current_writer.authority.fence)
        || captured.project_id != current_writer.project_id
        || captured.scope != current_writer.scope
        || caller.enrollment_id != frame.capture.enrollment_id
        || caller.revision != frame.capture.enrollment_revision
        || caller.node_id != frame.capture.node_id
        || caller.scope != frame.capture.writer.scope
        || caller.brain_id != current_writer.authority.fence.brain_id
    {
        return Err(RemoteReplayApplicationErrorV1::FenceMismatch);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn transition(
    spool: &dyn RemoteReplaySpoolPortV1,
    frame: &RemoteReplayFrameV1,
    from: RemoteReplayStateV1,
    to: RemoteReplayStateV1,
    replay_attempt: u64,
    observed_at: UtcMicros,
    finding: Option<RemoteReplayFindingV1>,
    receipt: Option<RemoteReplayCommitReceiptV1>,
) -> Result<(), RemoteReplayApplicationErrorV1> {
    let transition = RemoteReplayTransitionV1 {
        event_id: frame.event_id.clone(),
        from,
        to,
        replay_attempt,
        observed_at,
        finding,
        receipt,
    };
    transition.validate()?;
    spool
        .transition(transition)
        .map_err(RemoteReplayApplicationErrorV1::Persistence)
}

fn acknowledge(
    spool: &dyn RemoteReplaySpoolPortV1,
    frame: &RemoteReplayFrameV1,
    from: RemoteReplayStateV1,
    replay_attempt: u64,
    observed_at: UtcMicros,
    receipt: RemoteReplayCommitReceiptV1,
) -> Result<(), RemoteReplayApplicationErrorV1> {
    transition(
        spool,
        frame,
        from,
        RemoteReplayStateV1::Acknowledged,
        replay_attempt,
        observed_at,
        None,
        Some(receipt),
    )
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RemoteReplayTransactionErrorV1 {
    #[error("remote replay writer fence is stale")]
    FenceMismatch,
    #[error("remote replay idempotency identity conflicts")]
    IdempotencyConflict,
    #[error("remote replay canonical effect failed")]
    CanonicalEffect,
    #[error("remote replay storage is unavailable")]
    Unavailable,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RemoteReplayApplicationErrorV1 {
    #[error("remote replay frame is invalid")]
    InvalidFrame,
    #[error("remote replay attempt must be non-zero")]
    InvalidReplayAttempt,
    #[error("remote replay writer fence is mismatched")]
    FenceMismatch,
    #[error("remote replay spool state is invalid")]
    InvalidSpoolState,
    #[error("remote replay durable receipt is missing")]
    ReceiptMissing,
    #[error("remote replay durable receipt is mismatched")]
    ReceiptMismatch,
    #[error(transparent)]
    Authentication(RemoteAuthenticationError),
    #[error(transparent)]
    Persistence(RemoteCapturePersistenceErrorV1),
    #[error(transparent)]
    Transaction(RemoteReplayTransactionErrorV1),
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn replay_selector_rejects_noncanonical_event_identity() {
        assert!(
            RemoteReplayRequestV1 {
                event_id: "short".into()
            }
            .validate()
            .is_err()
        );
        assert!(
            RemoteReplayRequestV1 {
                event_id: "remote.event.sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()
            }
            .validate()
            .is_ok()
        );
    }
}
