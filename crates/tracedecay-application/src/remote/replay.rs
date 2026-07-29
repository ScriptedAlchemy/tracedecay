//! Authenticated, scope-exact application orchestration for remote replay.

use thiserror::Error;
use tracedecay_domain::{EnrollmentCredentialRecordV1, RemoteCapabilityV1, UtcMicros};
use tracedecay_store::{
    RemoteCaptureEventIdV1, RemoteCaptureFindingV1, RemoteCaptureFrameV1, RemoteCaptureStateV1,
    RemoteCaptureTransitionV1, RemoteWriterBindingV1, RuntimeSubmitRequestV1, StoreCommitReceiptV1,
    StoreShardScopeV1,
};

use super::auth::{
    OpaqueRemoteCredential, RemoteAuthenticationError, RemoteAuthorityAuthenticationPort,
    authenticate_remote_request,
};
use super::capture::RemoteCapturePersistenceErrorV1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteReplayPolicyDecisionV1 {
    Admit,
    Reject,
    Quarantine,
}

/// Current deletion, tombstone, quarantine, retention, and policy authority.
pub trait RemoteReplayPolicyPortV1: Send + Sync {
    fn authorize_current_policy(
        &self,
        frame: &RemoteCaptureFrameV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteReplayPolicyDecisionV1, RemoteReplayApplicationErrorV1>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteReplaySpoolStateV1 {
    pub state: RemoteCaptureStateV1,
    pub receipt: Option<StoreCommitReceiptV1>,
}

pub trait RemoteReplaySpoolPortV1: Send + Sync {
    fn state(
        &self,
        event_id: &RemoteCaptureEventIdV1,
    ) -> Result<RemoteReplaySpoolStateV1, RemoteCapturePersistenceErrorV1>;

    fn transition(
        &self,
        transition: RemoteCaptureTransitionV1,
    ) -> Result<(), RemoteCapturePersistenceErrorV1>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteReplayTransactionOutcomeV1 {
    Admitted(StoreCommitReceiptV1),
    Duplicate(StoreCommitReceiptV1),
}

/// Production adapter boundary. Implementations must use the native writer's
/// single SQLite transaction so canonical effect, idempotency ledger, and
/// durable receipt commit atomically.
pub trait RemoteReplayTransactionPortV1: Send + Sync {
    fn commit(
        &self,
        frame: &RemoteCaptureFrameV1,
        current_writer: &RemoteWriterBindingV1,
        request: &RuntimeSubmitRequestV1,
    ) -> Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteReplayOutcomeV1 {
    Acknowledged {
        disposition: RemoteCaptureStateV1,
        receipt: StoreCommitReceiptV1,
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
    frame: &RemoteCaptureFrameV1,
    current_writer: &RemoteWriterBindingV1,
    request: &RuntimeSubmitRequestV1,
    replay_attempt: u64,
    observed_at: UtcMicros,
) -> Result<RemoteReplayOutcomeV1, RemoteReplayApplicationErrorV1> {
    if replay_attempt == 0 {
        return Err(RemoteReplayApplicationErrorV1::InvalidReplayAttempt);
    }
    validate_scope_and_fence(frame, current_writer, caller_credential, request)?;
    if let Err(error) = authenticate_remote_request(
        authentication,
        &current_writer.fence,
        authority_credential,
        caller_credential,
        presented_caller_credential,
        RemoteCapabilityV1::Replay,
        &frame.enrollment.repository.scope,
        observed_at,
    ) {
        if error == RemoteAuthenticationError::Revoked
            && spool
                .state(&frame.event_id)
                .map_err(RemoteReplayApplicationErrorV1::Persistence)?
                .state
                == RemoteCaptureStateV1::Pending
        {
            spool
                .transition(RemoteCaptureTransitionV1 {
                    event_id: frame.event_id.clone(),
                    from: RemoteCaptureStateV1::Pending,
                    to: RemoteCaptureStateV1::Rejected,
                    replay_attempt,
                    observed_at,
                    finding: Some(RemoteCaptureFindingV1::EnrollmentRevoked),
                    receipt: None,
                })
                .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
        }
        return Err(RemoteReplayApplicationErrorV1::Authentication(error));
    }

    let spool_state = spool
        .state(&frame.event_id)
        .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
    if matches!(
        spool_state.state,
        RemoteCaptureStateV1::Admitted | RemoteCaptureStateV1::Duplicate
    ) {
        let receipt = spool_state
            .receipt
            .ok_or(RemoteReplayApplicationErrorV1::ReceiptMissing)?;
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
    if spool_state.state != RemoteCaptureStateV1::Pending {
        return Err(RemoteReplayApplicationErrorV1::InvalidSpoolState);
    }

    match policy.authorize_current_policy(frame, observed_at)? {
        RemoteReplayPolicyDecisionV1::Reject => {
            transition_policy_disposition(
                spool,
                frame,
                RemoteCaptureStateV1::Rejected,
                replay_attempt,
                observed_at,
            )?;
            return Ok(RemoteReplayOutcomeV1::Rejected);
        }
        RemoteReplayPolicyDecisionV1::Quarantine => {
            transition_policy_disposition(
                spool,
                frame,
                RemoteCaptureStateV1::Quarantined,
                replay_attempt,
                observed_at,
            )?;
            return Ok(RemoteReplayOutcomeV1::Quarantined);
        }
        RemoteReplayPolicyDecisionV1::Admit => {}
    }

    let (disposition, receipt, finding) = match transaction
        .commit(frame, current_writer, request)
        .map_err(RemoteReplayApplicationErrorV1::Transaction)?
    {
        RemoteReplayTransactionOutcomeV1::Admitted(receipt) => {
            (RemoteCaptureStateV1::Admitted, receipt, None)
        }
        RemoteReplayTransactionOutcomeV1::Duplicate(receipt) => (
            RemoteCaptureStateV1::Duplicate,
            receipt,
            Some(RemoteCaptureFindingV1::LostAcknowledgement),
        ),
    };
    spool
        .transition(RemoteCaptureTransitionV1 {
            event_id: frame.event_id.clone(),
            from: RemoteCaptureStateV1::Pending,
            to: disposition,
            replay_attempt,
            observed_at,
            finding,
            receipt: Some(receipt.clone()),
        })
        .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
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
    frame: &RemoteCaptureFrameV1,
    receipt: StoreCommitReceiptV1,
    replay_attempt: u64,
    observed_at: UtcMicros,
) -> Result<(), RemoteReplayApplicationErrorV1> {
    let state = spool
        .state(&frame.event_id)
        .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
    if state.state != RemoteCaptureStateV1::Acknowledged || state.receipt.as_ref() != Some(&receipt)
    {
        return Err(RemoteReplayApplicationErrorV1::InvalidSpoolState);
    }
    spool
        .transition(RemoteCaptureTransitionV1 {
            event_id: frame.event_id.clone(),
            from: RemoteCaptureStateV1::Acknowledged,
            to: RemoteCaptureStateV1::GarbageCollectionEligible,
            replay_attempt,
            observed_at,
            finding: None,
            receipt: Some(receipt),
        })
        .map_err(RemoteReplayApplicationErrorV1::Persistence)
}

fn validate_scope_and_fence(
    frame: &RemoteCaptureFrameV1,
    current_writer: &RemoteWriterBindingV1,
    caller: &EnrollmentCredentialRecordV1,
    request: &RuntimeSubmitRequestV1,
) -> Result<(), RemoteReplayApplicationErrorV1> {
    frame
        .validate()
        .map_err(|_| RemoteReplayApplicationErrorV1::InvalidFrame)?;
    current_writer
        .validate()
        .map_err(|_| RemoteReplayApplicationErrorV1::FenceMismatch)?;
    let captured = &frame.captured_writer.fence;
    let current = &current_writer.fence;
    if captured.brain_id != current.brain_id
        || captured.shard_id != current.shard_id
        || captured.generation_id != current.generation_id
        || request.binding() != &current_writer.runtime
        || caller.enrollment_id != frame.enrollment.enrollment_id
        || caller.revision != frame.enrollment.enrollment_revision
        || caller.node_id != frame.enrollment.node_id
        || caller.scope != frame.enrollment.repository.scope
        || caller.brain_id != current.brain_id
    {
        return Err(RemoteReplayApplicationErrorV1::FenceMismatch);
    }
    let Some(project_id) = current_writer.runtime.shard_id.scope.project_id() else {
        return Err(RemoteReplayApplicationErrorV1::ScopeMismatch);
    };
    if project_id != &frame.enrollment.repository.project_id {
        return Err(RemoteReplayApplicationErrorV1::ScopeMismatch);
    }
    if let StoreShardScopeV1::Code {
        repository_id,
        scope,
        ..
    } = &current_writer.runtime.shard_id.scope
    {
        if repository_id != &frame.enrollment.repository.scope.repository_id {
            return Err(RemoteReplayApplicationErrorV1::ScopeMismatch);
        }
        let worktree_matches = match scope {
            tracedecay_store::CodeShardScopeV1::Worktree { worktree_id }
            | tracedecay_store::CodeShardScopeV1::Branch { worktree_id, .. } => {
                worktree_id == &frame.enrollment.repository.scope.worktree_id
            }
            tracedecay_store::CodeShardScopeV1::Snapshot {
                worktree_id: Some(worktree_id),
                ..
            } => worktree_id == &frame.enrollment.repository.scope.worktree_id,
            tracedecay_store::CodeShardScopeV1::Snapshot {
                worktree_id: None, ..
            } => false,
        };
        if !worktree_matches {
            return Err(RemoteReplayApplicationErrorV1::ScopeMismatch);
        }
    }
    Ok(())
}

fn transition_policy_disposition(
    spool: &dyn RemoteReplaySpoolPortV1,
    frame: &RemoteCaptureFrameV1,
    to: RemoteCaptureStateV1,
    replay_attempt: u64,
    observed_at: UtcMicros,
) -> Result<(), RemoteReplayApplicationErrorV1> {
    spool
        .transition(RemoteCaptureTransitionV1 {
            event_id: frame.event_id.clone(),
            from: RemoteCaptureStateV1::Pending,
            to,
            replay_attempt,
            observed_at,
            finding: Some(RemoteCaptureFindingV1::PolicyChanged),
            receipt: None,
        })
        .map_err(RemoteReplayApplicationErrorV1::Persistence)
}

fn acknowledge(
    spool: &dyn RemoteReplaySpoolPortV1,
    frame: &RemoteCaptureFrameV1,
    from: RemoteCaptureStateV1,
    replay_attempt: u64,
    observed_at: UtcMicros,
    receipt: StoreCommitReceiptV1,
) -> Result<(), RemoteReplayApplicationErrorV1> {
    spool
        .transition(RemoteCaptureTransitionV1 {
            event_id: frame.event_id.clone(),
            from,
            to: RemoteCaptureStateV1::Acknowledged,
            replay_attempt,
            observed_at,
            finding: None,
            receipt: Some(receipt),
        })
        .map_err(RemoteReplayApplicationErrorV1::Persistence)
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
    #[error("remote replay repository scope is mismatched")]
    ScopeMismatch,
    #[error("remote replay spool state is invalid")]
    InvalidSpoolState,
    #[error("remote replay durable receipt is missing")]
    ReceiptMissing,
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
    fn only_acknowledged_state_can_become_gc_eligible() {
        assert!(
            RemoteCaptureStateV1::Acknowledged
                .permits_transition_to(RemoteCaptureStateV1::GarbageCollectionEligible)
        );
        assert!(
            !RemoteCaptureStateV1::Duplicate
                .permits_transition_to(RemoteCaptureStateV1::GarbageCollectionEligible)
        );
    }
}
