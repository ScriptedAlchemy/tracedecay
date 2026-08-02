//! Authenticated, scope-exact orchestration for replaying admitted captures.
//!
//! Store runtime bindings and SQL receipts stay behind the adapter ports. The
//! application carries only canonical capture identity and a bounded replay
//! receipt suitable for validation and status reporting.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CurrentRemoteAuthorityStateV1, EnrollmentCredentialRecordV1, ManifestDigest,
    RemoteAuthorityUnavailableReasonV1, RemoteCapabilityV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, UtcMicros, canonical_json_bytes, canonical_sha256,
};
use tracedecay_store::{RemoteObservationReplayPartsV1, RemoteObservationReplayWriteV1};

use super::auth::{
    OpaqueRemoteCredential, RemoteAuthenticationError, RemoteAuthorityAuthenticationPort,
    RemoteEnrollmentAuthorityErrorV1, RemoteEnrollmentCommitReceiptV1,
    RemoteEnrollmentCredentialLookupPortV1, authenticate_remote_request,
};
use super::capture::{RemoteCapturePersistenceErrorV1, RemoteWriterAuthorityV1};
use super::protocol::{
    REMOTE_PROTOCOL_VERSION_V1, REMOTE_REPLAY_USE_CASE_ID_V1, RemoteClockPortV1,
    RemoteProtocolExecutionErrorV1, RemoteProtocolFailureV1, RemoteProtocolPortV1,
    RemoteProtocolRequestV1, RemoteProtocolResponseV1, remote_protocol_problem,
    remote_replay_result_contract_v1,
};
use crate::{
    ApplicationEnvelope, Deadline, EffectId, EffectReceipt, EffectResult, EffectTermination,
    IdempotencyKey, OperationBudgetUsage, OperationReceipt, PolicyDecisionRef, ReconciliationState,
    ResolvedScope,
};
use tracedecay_tool_catalog::{EffectClass, UseCaseId};

pub use super::replay_contract::{
    RemoteReplayCommitReceiptV1, RemoteReplayFindingV1, RemoteReplayFrameV1,
    RemoteReplayOperationReceiptV1, RemoteReplayRequestV1, RemoteReplaySpoolStateV1,
    RemoteReplayStateV1, RemoteReplayTransitionReceiptV1, RemoteReplayTransitionV1,
    canonical_remote_event_id_v1,
};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteReplayPolicyDecisionV1 {
    Admit,
    Reject,
    Quarantine,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteReplayPolicyEvidenceV1 {
    pub scope: ResolvedScope,
    pub repository_scope: RemoteRepositoryScopeV1,
    pub policy_revision: u64,
    pub decision: RemoteReplayPolicyDecisionV1,
    pub policy: PolicyDecisionRef,
    pub configuration_digest: ManifestDigest,
    pub catalog_digest: ManifestDigest,
    pub privacy_digest: ManifestDigest,
    pub revalidated_at: UtcMicros,
}

impl RemoteReplayPolicyEvidenceV1 {
    pub fn validate(&self) -> Result<(), RemoteReplayApplicationErrorV1> {
        self.repository_scope
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        if self.scope.project_id != self.repository_scope.project_id
            || self.scope.repository_id != self.repository_scope.repository_id
            || self.scope.worktree_id != self.repository_scope.worktree_id
            || self.scope.reference != self.repository_scope.reference
            || self.policy_revision == 0
            || self.policy_revision != self.policy.revision
        {
            return Err(RemoteReplayApplicationErrorV1::PolicyMismatch);
        }
        self.scope
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        self.policy
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        self.configuration_digest
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        self.catalog_digest
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)?;
        self.privacy_digest
            .validate()
            .map_err(|_| RemoteReplayApplicationErrorV1::PolicyMismatch)
    }

    pub fn validate_for(
        &self,
        frame: &RemoteReplayFrameV1,
    ) -> Result<(), RemoteReplayApplicationErrorV1> {
        self.validate()?;
        if self.repository_scope != frame.capture.writer.scope
            || self.policy_revision < frame.capture.policy_revision
        {
            return Err(RemoteReplayApplicationErrorV1::PolicyMismatch);
        }
        Ok(())
    }
}

pub trait RemoteReplayPolicyPortV1: Send + Sync {
    fn authorize_current_policy(
        &self,
        frame: &RemoteReplayFrameV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteReplayPolicyDecisionV1, RemoteReplayApplicationErrorV1>;
}

pub trait RemoteReplayPolicyEvidencePortV1: RemoteReplayPolicyPortV1 {
    fn current_policy_evidence(
        &self,
        frame: &RemoteReplayFrameV1,
    ) -> Result<RemoteReplayPolicyEvidenceV1, RemoteReplayApplicationErrorV1>;
}

pub trait RemoteReplaySpoolPortV1: Send + Sync {
    fn state(
        &self,
        event_id: &str,
    ) -> Result<RemoteReplaySpoolStateV1, RemoteCapturePersistenceErrorV1>;

    fn transition(
        &self,
        transition: RemoteReplayTransitionV1,
    ) -> Result<RemoteReplayTransitionReceiptV1, RemoteCapturePersistenceErrorV1>;

    fn begin_replay_attempt(
        &self,
        event_id: &str,
        observed_at: UtcMicros,
    ) -> Result<u64, RemoteCapturePersistenceErrorV1>;

    fn abandon_replay_attempt(
        &self,
        event_id: &str,
        replay_attempt: u64,
    ) -> Result<(), RemoteCapturePersistenceErrorV1>;
}

pub trait RemoteReplayFrameLookupPortV1: Send + Sync {
    fn load_replay_frame(
        &self,
        event_id: &str,
    ) -> Result<RemoteReplayFrameV1, RemoteCapturePersistenceErrorV1>;
}

pub trait RemoteReplayCurrentWriterPortV1: Send + Sync {
    fn current_authority(
        &self,
        expected: &RemoteWriterFenceV1,
    ) -> Result<CurrentRemoteAuthorityStateV1, RemoteCapturePersistenceErrorV1>;

    fn current_writer(
        &self,
        frame: &RemoteReplayFrameV1,
    ) -> Result<RemoteReplayCurrentWriterV1, RemoteCapturePersistenceErrorV1>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteReplayCurrentWriterV1 {
    pub writer: Option<RemoteWriterAuthorityV1>,
    pub state: CurrentRemoteAuthorityStateV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteReplayServiceOutcomeV1 {
    pub outcome: RemoteReplayOutcomeV1,
    pub authority: CurrentRemoteAuthorityStateV1,
    pub frame: RemoteReplayFrameV1,
    pub caller_admission: RemoteEnrollmentCommitReceiptV1,
    pub caller: EnrollmentCredentialRecordV1,
    pub policy: RemoteReplayPolicyEvidenceV1,
    pub input_digest: ManifestDigest,
}

pub struct RemoteReplayServiceV1 {
    authentication: Arc<dyn RemoteAuthorityAuthenticationPort + Send + Sync>,
    credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
    current_writer: Arc<dyn RemoteReplayCurrentWriterPortV1>,
    policy: Arc<dyn RemoteReplayPolicyPortV1>,
    policy_evidence: Arc<dyn RemoteReplayPolicyEvidencePortV1>,
    transaction: Arc<dyn RemoteReplayTransactionPortV1>,
    clock: Arc<dyn RemoteClockPortV1>,
}

impl RemoteReplayServiceV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_clock(
        authentication: Arc<dyn RemoteAuthorityAuthenticationPort + Send + Sync>,
        credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
        current_writer: Arc<dyn RemoteReplayCurrentWriterPortV1>,
        policy: Arc<dyn RemoteReplayPolicyPortV1>,
        policy_evidence: Arc<dyn RemoteReplayPolicyEvidencePortV1>,
        transaction: Arc<dyn RemoteReplayTransactionPortV1>,
        clock: Arc<dyn RemoteClockPortV1>,
    ) -> Self {
        Self {
            authentication,
            credentials,
            current_writer,
            policy,
            policy_evidence,
            transaction,
            clock,
        }
    }

    pub fn replay(
        &self,
        request: &RemoteProtocolRequestV1<RemoteReplayRequestV1>,
        presented_credential: &OpaqueRemoteCredential,
    ) -> Result<RemoteReplayServiceOutcomeV1, RemoteReplayServiceErrorV1> {
        if request.protocol_version != REMOTE_PROTOCOL_VERSION_V1 {
            return Err(RemoteReplayServiceErrorV1::UnsupportedVersion);
        }
        request
            .validate_metadata()
            .and_then(|()| request.body.validate())
            .map_err(|_| RemoteReplayServiceErrorV1::InvalidRequest)?;
        let input_digest =
            canonical_sha256(request).map_err(|_| RemoteReplayServiceErrorV1::InvalidRequest)?;
        let frame = request.body.frame.clone();
        let caller = self
            .credentials
            .enrollment_by_id(&frame.capture.enrollment_id)
            .map_err(RemoteReplayServiceErrorV1::Credential)?;
        if request.brain_id != caller.brain_id
            || request.caller_node_id != caller.node_id
            || request.enrollment_revision != caller.revision
            || caller.enrollment_id != frame.capture.enrollment_id
            || caller.node_id != frame.capture.node_id
            || caller.revision != frame.capture.enrollment_revision
        {
            return Err(RemoteReplayServiceErrorV1::RequestBindingMismatch);
        }
        let caller_admission = self
            .credentials
            .enrollment_commit_receipt(&frame.capture.enrollment_id)
            .map_err(RemoteReplayServiceErrorV1::Credential)?;
        caller_admission
            .validate()
            .map_err(|_| RemoteReplayServiceErrorV1::RequestBindingMismatch)?;
        if caller_admission.enrollment != caller
            || caller_admission.admission.scope().project_id != caller.scope.project_id
            || caller_admission.admission.scope().repository_id != caller.scope.repository_id
            || caller_admission.admission.scope().worktree_id != caller.scope.worktree_id
            || caller_admission.admission.scope().reference != caller.scope.reference
        {
            return Err(RemoteReplayServiceErrorV1::RequestBindingMismatch);
        }
        let current = self
            .current_writer
            .current_writer(&frame)
            .map_err(RemoteReplayServiceErrorV1::Persistence)?;
        let writer = current.writer.as_ref().ok_or_else(|| {
            RemoteReplayServiceErrorV1::AuthorityUnavailable(Box::new(current.state.clone()))
        })?;
        if request.expected_authority != writer.authority.fence {
            return Err(RemoteReplayServiceErrorV1::ExpectedAuthorityMismatch(
                Box::new(current.state.clone()),
            ));
        }
        let authority_credential = self
            .credentials
            .authority_enrollment(
                &writer.authority.fence.brain_id,
                &writer.authority.fence.authority_node_id,
                writer.authority.credential_revision,
            )
            .map_err(RemoteReplayServiceErrorV1::Credential)?;
        let policy = self.policy_evidence.current_policy_evidence(&frame)?;
        let outcome = admit_remote_replay_at_authority(
            self.authentication.as_ref(),
            self.policy.as_ref(),
            self.transaction.as_ref(),
            &authority_credential,
            &caller,
            presented_credential,
            &frame,
            writer,
            request.body.replay_attempt,
            self.clock.as_ref(),
        )?;
        Ok(RemoteReplayServiceOutcomeV1 {
            outcome,
            authority: current.state,
            frame,
            caller_admission,
            caller,
            policy,
            input_digest,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RemoteReplayServiceErrorV1 {
    #[error("remote replay protocol version is unsupported")]
    UnsupportedVersion,
    #[error("remote replay request is invalid")]
    InvalidRequest,
    #[error("remote replay request does not match the durable caller enrollment")]
    RequestBindingMismatch,
    #[error("remote replay expected authority does not match the current writer")]
    ExpectedAuthorityMismatch(Box<CurrentRemoteAuthorityStateV1>),
    #[error("remote replay credential authority failed")]
    Credential(RemoteEnrollmentAuthorityErrorV1),
    #[error("remote replay authority is unavailable")]
    AuthorityUnavailable(Box<CurrentRemoteAuthorityStateV1>),
    #[error(transparent)]
    Persistence(RemoteCapturePersistenceErrorV1),
    #[error(transparent)]
    Replay(#[from] RemoteReplayApplicationErrorV1),
}

pub struct RemoteReplayProtocolAdapterV1 {
    service: RemoteReplayServiceV1,
}

impl RemoteReplayProtocolAdapterV1 {
    pub fn new(service: RemoteReplayServiceV1) -> Self {
        Self { service }
    }
}

impl RemoteProtocolPortV1<RemoteReplayRequestV1> for RemoteReplayProtocolAdapterV1 {
    type Output = RemoteReplayOutcomeV1;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteReplayRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> Result<RemoteProtocolResponseV1<Self::Output>, RemoteProtocolExecutionErrorV1> {
        let request_id = request.request_id.clone();
        let observed_at = self.service.clock.now()?;
        let server_authority = self
            .service
            .current_writer
            .current_authority(&request.expected_authority)
            .map_err(|_| RemoteProtocolExecutionErrorV1::AuthorityUnavailable)?;
        match self.service.replay(&request, &credential) {
            Ok(outcome) => {
                let authority = outcome.authority.clone();
                let result = replay_effect_envelope(request, outcome).map_err(|failure| {
                    remote_protocol_problem(
                        remote_replay_result_contract_v1(),
                        request_id.clone(),
                        failure,
                    )
                });
                RemoteProtocolResponseV1::new(request_id, authority, result)
                    .map_err(|_| RemoteProtocolExecutionErrorV1::AuthorityUnavailable)
            }
            Err(error) => {
                let authority = if matches!(
                    &error,
                    RemoteReplayServiceErrorV1::Credential(
                        RemoteEnrollmentAuthorityErrorV1::GrantNotFound
                    )
                ) {
                    CurrentRemoteAuthorityStateV1::Unavailable {
                        reason: RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
                        observed_at,
                    }
                } else {
                    match &error {
                        RemoteReplayServiceErrorV1::AuthorityUnavailable(state)
                        | RemoteReplayServiceErrorV1::ExpectedAuthorityMismatch(state) => {
                            state.as_ref().clone()
                        }
                        _ => server_authority,
                    }
                };
                let failure = replay_protocol_failure(error);
                RemoteProtocolResponseV1::new(
                    request_id.clone(),
                    authority,
                    Err(remote_protocol_problem(
                        remote_replay_result_contract_v1(),
                        request_id,
                        failure,
                    )),
                )
                .map_err(|_| RemoteProtocolExecutionErrorV1::AuthorityUnavailable)
            }
        }
    }
}

fn replay_effect_envelope(
    request: RemoteProtocolRequestV1<RemoteReplayRequestV1>,
    outcome: RemoteReplayServiceOutcomeV1,
) -> Result<ApplicationEnvelope<RemoteReplayOutcomeV1>, RemoteProtocolFailureV1> {
    let operation_receipt = match &outcome.outcome {
        RemoteReplayOutcomeV1::Acknowledged {
            receipt,
            operation_receipt,
            ..
        } if operation_receipt.transaction.as_ref() == Some(receipt) => operation_receipt,
        RemoteReplayOutcomeV1::Rejected { operation_receipt }
        | RemoteReplayOutcomeV1::Quarantined { operation_receipt }
            if operation_receipt.transaction.is_none() =>
        {
            operation_receipt
        }
        _ => return Err(RemoteProtocolFailureV1::AuthorityUnavailable),
    };
    operation_receipt
        .validate()
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let expected_state = operation_receipt.pre_state_digest.clone();
    let committed_state = operation_receipt.committed_effect_digest.clone();
    let deadline = Deadline::new(outcome.caller.expires_at)
        .map_err(|_| RemoteProtocolFailureV1::EnrollmentExpired)?;
    let execution = OperationReceipt::completed(
        operation_receipt.started_at,
        operation_receipt.committed_at,
        deadline,
        operation_receipt.budget,
    )
    .map_err(|_| RemoteProtocolFailureV1::EnrollmentExpired)?;
    let event_digest = canonical_sha256(&(
        "tracedecay.remote-replay-effect.v1",
        &outcome.frame.event_id,
        outcome.caller.revision,
    ))
    .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let event_digest_id = event_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let operation = UseCaseId::new(REMOTE_REPLAY_USE_CASE_ID_V1)
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let effect_id = EffectId::new(format!("effect.remote.replay.{event_digest_id}"))
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let idempotency_key =
        IdempotencyKey::new(format!("idempotency.remote.replay.{event_digest_id}"))
            .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let mut authority = outcome.caller_admission.admission.authority().clone();
    authority.policy = outcome.policy.policy.clone();
    authority
        .validate_for(&outcome.policy.scope)
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let receipt = EffectReceipt {
        operation: operation.clone(),
        request_id: request.request_id.clone(),
        actor: outcome.caller_admission.admission.actor().clone(),
        scope: outcome.policy.scope.clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: idempotency_key.clone(),
        input_digest: outcome.input_digest,
        expected_state: expected_state.clone(),
        policy_digest: outcome.policy.policy.digest.clone(),
        configuration_digest: outcome.policy.configuration_digest,
        catalog_digest: outcome.policy.catalog_digest,
        privacy_digest: outcome.policy.privacy_digest,
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    let effect = EffectResult::new(
        effect_id,
        EffectClass::Administrative,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(outcome.outcome),
    )
    .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    Ok(ApplicationEnvelope::effect(
        remote_replay_result_contract_v1(),
        request.request_id,
        outcome.policy.scope,
        effect,
    ))
}

fn replay_protocol_failure(error: RemoteReplayServiceErrorV1) -> RemoteProtocolFailureV1 {
    match error {
        RemoteReplayServiceErrorV1::UnsupportedVersion => {
            RemoteProtocolFailureV1::UnsupportedVersion
        }
        RemoteReplayServiceErrorV1::InvalidRequest
        | RemoteReplayServiceErrorV1::RequestBindingMismatch => {
            RemoteProtocolFailureV1::ScopeMismatch
        }
        RemoteReplayServiceErrorV1::ExpectedAuthorityMismatch(_) => {
            RemoteProtocolFailureV1::StaleAuthorityFence
        }
        RemoteReplayServiceErrorV1::AuthorityUnavailable(_)
        | RemoteReplayServiceErrorV1::Persistence(_)
        | RemoteReplayServiceErrorV1::Credential(
            RemoteEnrollmentAuthorityErrorV1::Unavailable
            | RemoteEnrollmentAuthorityErrorV1::IdentityConflict,
        ) => RemoteProtocolFailureV1::AuthorityUnavailable,
        RemoteReplayServiceErrorV1::Credential(RemoteEnrollmentAuthorityErrorV1::GrantConsumed) => {
            RemoteProtocolFailureV1::StaleCredentialRevision
        }
        RemoteReplayServiceErrorV1::Credential(RemoteEnrollmentAuthorityErrorV1::GrantNotFound) => {
            RemoteProtocolFailureV1::CallerAuthenticationFailed
        }
        RemoteReplayServiceErrorV1::Replay(replay) => match replay {
            RemoteReplayApplicationErrorV1::Authentication(authentication) => {
                match authentication {
                    RemoteAuthenticationError::Expired => {
                        RemoteProtocolFailureV1::EnrollmentExpired
                    }
                    RemoteAuthenticationError::Revoked => {
                        RemoteProtocolFailureV1::EnrollmentRevoked
                    }
                    RemoteAuthenticationError::InsufficientCapability => {
                        RemoteProtocolFailureV1::InsufficientCapability
                    }
                    RemoteAuthenticationError::StaleRevision
                    | RemoteAuthenticationError::RevisionOverflow => {
                        RemoteProtocolFailureV1::StaleCredentialRevision
                    }
                    RemoteAuthenticationError::AuthorityAuthenticationFailed
                    | RemoteAuthenticationError::InvalidAuthorityCredential => {
                        RemoteProtocolFailureV1::AuthorityAuthenticationFailed
                    }
                    RemoteAuthenticationError::InvalidCredential => {
                        RemoteProtocolFailureV1::CallerAuthenticationFailed
                    }
                    RemoteAuthenticationError::IdentityMismatch
                    | RemoteAuthenticationError::ScopeMismatch
                    | RemoteAuthenticationError::InvalidEnrollment
                    | RemoteAuthenticationError::InvalidValidity => {
                        RemoteProtocolFailureV1::ScopeMismatch
                    }
                }
            }
            RemoteReplayApplicationErrorV1::FenceMismatch
            | RemoteReplayApplicationErrorV1::ReceiptMismatch
            | RemoteReplayApplicationErrorV1::Transaction(
                RemoteReplayTransactionErrorV1::FenceMismatch,
            ) => RemoteProtocolFailureV1::StaleAuthorityFence,
            RemoteReplayApplicationErrorV1::PolicyMismatch => {
                RemoteProtocolFailureV1::StaleCredentialRevision
            }
            RemoteReplayApplicationErrorV1::InvalidFrame
            | RemoteReplayApplicationErrorV1::Transaction(
                RemoteReplayTransactionErrorV1::IdempotencyConflict,
            ) => RemoteProtocolFailureV1::ScopeMismatch,
            RemoteReplayApplicationErrorV1::InvalidReplayAttempt
            | RemoteReplayApplicationErrorV1::InvalidSpoolState
            | RemoteReplayApplicationErrorV1::ReceiptMissing
            | RemoteReplayApplicationErrorV1::PolicyUnavailable
            | RemoteReplayApplicationErrorV1::ClockUnavailable
            | RemoteReplayApplicationErrorV1::Persistence(_)
            | RemoteReplayApplicationErrorV1::Transaction(
                RemoteReplayTransactionErrorV1::SequenceGap
                | RemoteReplayTransactionErrorV1::CanonicalEffect
                | RemoteReplayTransactionErrorV1::Unavailable,
            ) => RemoteProtocolFailureV1::AuthorityUnavailable,
        },
    }
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
        committed_at: UtcMicros,
    ) -> Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1>;
}

pub fn canonical_remote_observation_write_v1(
    frame: &RemoteReplayFrameV1,
    current_writer: &RemoteWriterAuthorityV1,
) -> Result<RemoteObservationReplayWriteV1, RemoteReplayApplicationErrorV1> {
    frame.validate()?;
    current_writer
        .validate()
        .map_err(|_| RemoteReplayApplicationErrorV1::FenceMismatch)?;
    let frame_digest = canonical_sha256(&frame.capture)
        .map_err(|_| RemoteReplayApplicationErrorV1::InvalidFrame)?;
    RemoteObservationReplayWriteV1::new(
        RemoteObservationReplayPartsV1 {
            event_id: frame.event_id.clone(),
            frame_digest,
            enrollment_id: frame.capture.enrollment_id.clone(),
            enrollment_revision: frame.capture.enrollment_revision,
            node_id: frame.capture.node_id.clone(),
            policy_revision: frame.capture.policy_revision,
            capture_sequence: frame.capture.sequence.sequence,
            previous_event_id: frame.capture.sequence.previous_event_id.clone(),
            writer_project_id: current_writer.project_id.clone(),
            writer_scope: current_writer.scope.clone(),
            current_writer: current_writer.authority.clone(),
            captured_at: frame.capture.captured_at,
        },
        frame.capture.anchored_write.clone(),
    )
    .map_err(|_| RemoteReplayApplicationErrorV1::InvalidFrame)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum RemoteReplayOutcomeV1 {
    Acknowledged {
        disposition: RemoteReplayStateV1,
        receipt: RemoteReplayCommitReceiptV1,
        operation_receipt: RemoteReplayOperationReceiptV1,
    },
    Rejected {
        operation_receipt: RemoteReplayOperationReceiptV1,
    },
    Quarantined {
        operation_receipt: RemoteReplayOperationReceiptV1,
    },
}

/// Admit one node-submitted frame at the current authority.
///
/// Node-local spool state is deliberately absent. The caller owns replay
/// attempts and acknowledgement transitions; this authority validates and
/// atomically commits only the submitted sanitized frame.
#[allow(clippy::too_many_arguments)]
pub fn admit_remote_replay_at_authority(
    authentication: &dyn RemoteAuthorityAuthenticationPort,
    policy: &dyn RemoteReplayPolicyPortV1,
    transaction: &dyn RemoteReplayTransactionPortV1,
    authority_credential: &EnrollmentCredentialRecordV1,
    caller_credential: &EnrollmentCredentialRecordV1,
    presented_caller_credential: &OpaqueRemoteCredential,
    frame: &RemoteReplayFrameV1,
    current_writer: &RemoteWriterAuthorityV1,
    replay_attempt: u64,
    clock: &dyn RemoteClockPortV1,
) -> Result<RemoteReplayOutcomeV1, RemoteReplayApplicationErrorV1> {
    if replay_attempt == 0 {
        return Err(RemoteReplayApplicationErrorV1::InvalidReplayAttempt);
    }
    let started_at = remote_clock_now(clock)?;
    validate_scope_and_fence(frame, current_writer, caller_credential)?;
    authenticate_remote_request(
        authentication,
        &current_writer.authority,
        authority_credential,
        caller_credential,
        presented_caller_credential,
        RemoteCapabilityV1::Replay,
        &frame.capture.writer.scope,
        started_at,
    )
    .map_err(RemoteReplayApplicationErrorV1::Authentication)?;

    match policy.authorize_current_policy(frame, started_at)? {
        RemoteReplayPolicyDecisionV1::Reject => {
            let committed_at = remote_clock_now(clock)?;
            return Ok(RemoteReplayOutcomeV1::Rejected {
                operation_receipt: authority_replay_operation_receipt(
                    frame,
                    replay_attempt,
                    RemoteReplayStateV1::Rejected,
                    started_at,
                    committed_at,
                    None,
                )?,
            });
        }
        RemoteReplayPolicyDecisionV1::Quarantine => {
            let committed_at = remote_clock_now(clock)?;
            return Ok(RemoteReplayOutcomeV1::Quarantined {
                operation_receipt: authority_replay_operation_receipt(
                    frame,
                    replay_attempt,
                    RemoteReplayStateV1::Quarantined,
                    started_at,
                    committed_at,
                    None,
                )?,
            });
        }
        RemoteReplayPolicyDecisionV1::Admit => {}
    }

    let committed_at = remote_clock_now(clock)?;
    let (disposition, receipt) = match transaction
        .commit(frame, current_writer, committed_at)
        .map_err(RemoteReplayApplicationErrorV1::Transaction)?
    {
        RemoteReplayTransactionOutcomeV1::Admitted(receipt) => {
            (RemoteReplayStateV1::Admitted, receipt)
        }
        RemoteReplayTransactionOutcomeV1::Duplicate(receipt) => {
            (RemoteReplayStateV1::Duplicate, receipt)
        }
    };
    receipt.validate_for(frame, current_writer)?;
    let acknowledged_at = remote_clock_now(clock)?;
    if receipt.committed_at > acknowledged_at {
        return Err(RemoteReplayApplicationErrorV1::ReceiptMismatch);
    }
    let operation_receipt = authority_replay_operation_receipt(
        frame,
        replay_attempt,
        disposition,
        started_at,
        acknowledged_at,
        Some(receipt.clone()),
    )?;
    Ok(RemoteReplayOutcomeV1::Acknowledged {
        disposition,
        receipt,
        operation_receipt,
    })
}

fn authority_replay_operation_receipt(
    frame: &RemoteReplayFrameV1,
    replay_attempt: u64,
    disposition: RemoteReplayStateV1,
    started_at: UtcMicros,
    committed_at: UtcMicros,
    transaction: Option<RemoteReplayCommitReceiptV1>,
) -> Result<RemoteReplayOperationReceiptV1, RemoteReplayApplicationErrorV1> {
    if replay_attempt == 0
        || committed_at < started_at
        || matches!(
            disposition,
            RemoteReplayStateV1::Pending
                | RemoteReplayStateV1::Acknowledged
                | RemoteReplayStateV1::GarbageCollectionEligible
        )
        || matches!(
            disposition,
            RemoteReplayStateV1::Admitted | RemoteReplayStateV1::Duplicate
        ) != transaction.is_some()
    {
        return Err(RemoteReplayApplicationErrorV1::ReceiptMismatch);
    }
    let pre_state_digest = canonical_sha256(&(
        "tracedecay.remote-replay.authority-input.v1",
        replay_attempt,
        frame,
    ))
    .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)?;
    let terminal_state_digest = canonical_sha256(&(
        "tracedecay.remote-replay.authority-output.v1",
        replay_attempt,
        disposition,
        transaction.as_ref(),
    ))
    .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)?;
    let committed_effect_digest = transaction
        .as_ref()
        .map(canonical_sha256)
        .transpose()
        .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)?
        .unwrap_or_else(|| terminal_state_digest.clone());
    let bytes_consumed = canonical_json_bytes(frame)
        .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)?
        .len()
        .try_into()
        .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)?;
    let elapsed_micros = committed_at
        .0
        .checked_sub(started_at.0)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(RemoteReplayApplicationErrorV1::ReceiptMismatch)?;
    let receipt = RemoteReplayOperationReceiptV1 {
        event_id: frame.event_id.clone(),
        replay_attempt,
        pre_state_digest,
        terminal_state_digest,
        committed_effect_digest,
        started_at,
        committed_at,
        budget: OperationBudgetUsage {
            units_consumed: 1,
            bytes_consumed,
            elapsed_micros,
        },
        transaction,
    };
    receipt.validate()?;
    Ok(receipt)
}

fn remote_clock_now(
    clock: &dyn RemoteClockPortV1,
) -> Result<UtcMicros, RemoteReplayApplicationErrorV1> {
    clock
        .now()
        .map_err(|_| RemoteReplayApplicationErrorV1::ClockUnavailable)
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
    let captured_fence = &captured.authority.fence;
    let current_fence = &current_writer.authority.fence;
    if !(current_fence == captured_fence || current_fence.fences(captured_fence))
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

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteReplayTransactionErrorV1 {
    #[error("remote replay writer fence is stale")]
    FenceMismatch,
    #[error("remote replay idempotency identity conflicts")]
    IdempotencyConflict,
    #[error("remote replay capture sequence has a gap")]
    SequenceGap,
    #[error("remote replay canonical effect failed")]
    CanonicalEffect,
    #[error("remote replay storage is unavailable")]
    Unavailable,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteReplayApplicationErrorV1 {
    #[error("remote replay authoritative clock is unavailable")]
    ClockUnavailable,
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
    #[error("remote replay policy evidence is unavailable")]
    PolicyUnavailable,
    #[error("remote replay policy evidence does not match the canonical frame")]
    PolicyMismatch,
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
    fn replay_protocol_failures_preserve_concealment_and_staleness() {
        assert_eq!(
            replay_protocol_failure(RemoteReplayServiceErrorV1::Replay(
                RemoteReplayApplicationErrorV1::Authentication(RemoteAuthenticationError::Revoked,),
            )),
            RemoteProtocolFailureV1::EnrollmentRevoked
        );
        assert_eq!(
            replay_protocol_failure(RemoteReplayServiceErrorV1::Replay(
                RemoteReplayApplicationErrorV1::Authentication(RemoteAuthenticationError::Expired,),
            )),
            RemoteProtocolFailureV1::EnrollmentExpired
        );
    }

    #[test]
    fn replay_operation_and_result_contract_are_operation_specific() {
        assert_eq!(REMOTE_REPLAY_USE_CASE_ID_V1, "use-case.remote.replay");
        assert_ne!(
            remote_replay_result_contract_v1(),
            super::super::protocol::remote_enrollment_result_contract_v1()
        );
    }
}
