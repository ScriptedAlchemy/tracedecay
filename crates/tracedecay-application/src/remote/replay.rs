//! Authenticated, scope-exact orchestration for replaying admitted captures.
//!
//! Store runtime bindings and SQL receipts stay behind the adapter ports. The
//! application carries only canonical capture identity and a bounded replay
//! receipt suitable for validation and status reporting.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CurrentRemoteAuthorityStateV1, EnrollmentCredentialRecordV1, ManifestDigest,
    RemoteAuthorityUnavailableReasonV1, RemoteCapabilityV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, UtcMicros, canonical_sha256,
};

use super::auth::{
    OpaqueRemoteCredential, RemoteAuthenticationError, RemoteAuthorityAuthenticationPort,
    RemoteEnrollmentAuthorityErrorV1, RemoteEnrollmentCommitReceiptV1,
    RemoteEnrollmentCredentialLookupPortV1, authenticate_remote_request,
};
use super::capture::{
    AdmittedRemoteCaptureV1, RemoteCapturePersistenceErrorV1, RemoteWriterAuthorityV1,
};
use super::protocol::RemoteProtocolBodyV1;
use super::protocol::{
    REMOTE_PROTOCOL_VERSION_V1, REMOTE_REPLAY_USE_CASE_ID_V1, RemoteProtocolFailureV1,
    RemoteProtocolPortV1, RemoteProtocolRequestV1, RemoteProtocolResponseV1,
    remote_protocol_problem, remote_replay_result_contract_v1,
};
use crate::{
    ApplicationContractError, ApplicationEnvelope, Deadline, EffectId, EffectReceipt, EffectResult,
    EffectTermination, IdempotencyKey, OperationBudgetUsage, OperationReceipt, PolicyDecisionRef,
    ReconciliationState, ResolvedScope,
};
use tracedecay_tool_catalog::{EffectClass, UseCaseId};

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

#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub committed_at: UtcMicros,
    pub budget: OperationBudgetUsage,
    pub transaction: Option<RemoteReplayCommitReceiptV1>,
}

impl RemoteReplayOperationReceiptV1 {
    pub fn validate(&self) -> Result<(), RemoteReplayApplicationErrorV1> {
        if self.replay_attempt == 0
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
    frames: Arc<dyn RemoteReplayFrameLookupPortV1>,
    current_writer: Arc<dyn RemoteReplayCurrentWriterPortV1>,
    policy: Arc<dyn RemoteReplayPolicyPortV1>,
    policy_evidence: Arc<dyn RemoteReplayPolicyEvidencePortV1>,
    transaction: Arc<dyn RemoteReplayTransactionPortV1>,
    spool: Arc<dyn RemoteReplaySpoolPortV1>,
    clock: Arc<dyn RemoteReplayClockPortV1>,
}

pub trait RemoteReplayClockPortV1: Send + Sync {
    fn now(&self) -> Result<UtcMicros, RemoteReplayApplicationErrorV1>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRemoteReplayClockV1;

impl RemoteReplayClockPortV1 for SystemRemoteReplayClockV1 {
    fn now(&self) -> Result<UtcMicros, RemoteReplayApplicationErrorV1> {
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RemoteReplayApplicationErrorV1::ClockUnavailable)?
            .as_micros();
        i64::try_from(micros)
            .map(UtcMicros)
            .map_err(|_| RemoteReplayApplicationErrorV1::ClockUnavailable)
    }
}

impl RemoteReplayServiceV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        authentication: Arc<dyn RemoteAuthorityAuthenticationPort + Send + Sync>,
        credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
        frames: Arc<dyn RemoteReplayFrameLookupPortV1>,
        current_writer: Arc<dyn RemoteReplayCurrentWriterPortV1>,
        policy: Arc<dyn RemoteReplayPolicyPortV1>,
        policy_evidence: Arc<dyn RemoteReplayPolicyEvidencePortV1>,
        transaction: Arc<dyn RemoteReplayTransactionPortV1>,
        spool: Arc<dyn RemoteReplaySpoolPortV1>,
    ) -> Self {
        Self::new_with_clock(
            authentication,
            credentials,
            frames,
            current_writer,
            policy,
            policy_evidence,
            transaction,
            spool,
            Arc::new(SystemRemoteReplayClockV1),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_clock(
        authentication: Arc<dyn RemoteAuthorityAuthenticationPort + Send + Sync>,
        credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
        frames: Arc<dyn RemoteReplayFrameLookupPortV1>,
        current_writer: Arc<dyn RemoteReplayCurrentWriterPortV1>,
        policy: Arc<dyn RemoteReplayPolicyPortV1>,
        policy_evidence: Arc<dyn RemoteReplayPolicyEvidencePortV1>,
        transaction: Arc<dyn RemoteReplayTransactionPortV1>,
        spool: Arc<dyn RemoteReplaySpoolPortV1>,
        clock: Arc<dyn RemoteReplayClockPortV1>,
    ) -> Self {
        Self {
            authentication,
            credentials,
            frames,
            current_writer,
            policy,
            policy_evidence,
            transaction,
            spool,
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
        let frame = self
            .frames
            .load_replay_frame(&request.body.event_id)
            .map_err(|error| match error {
                RemoteCapturePersistenceErrorV1::Corruption
                | RemoteCapturePersistenceErrorV1::SequenceGap => {
                    RemoteReplayServiceErrorV1::FrameSelectionRejected
                }
                error => RemoteReplayServiceErrorV1::Persistence(error),
            })?;
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
        if request.expected_authority.as_ref() != Some(&writer.authority.fence) {
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
        let outcome = replay_remote_capture(
            self.authentication.as_ref(),
            self.policy.as_ref(),
            self.transaction.as_ref(),
            self.spool.as_ref(),
            &authority_credential,
            &caller,
            presented_credential,
            &frame,
            writer,
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
    #[error("remote replay frame selection is not authorized")]
    FrameSelectionRejected,
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
    ) -> RemoteProtocolResponseV1<Self::Output> {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        let fallback_authority = request.expected_authority.clone().map_or_else(
            || CurrentRemoteAuthorityStateV1::Unavailable {
                reason: RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
                observed_at,
            },
            |known_fence| CurrentRemoteAuthorityStateV1::Partial {
                known_fence: Some(known_fence),
                missing: BTreeSet::from([RemoteAuthorityUnavailableReasonV1::FenceUnverified]),
                observed_at,
            },
        );
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
                    .expect("replay adapter preserves validated response identities")
            }
            Err(error) => {
                let authority = match &error {
                    RemoteReplayServiceErrorV1::AuthorityUnavailable(state)
                    | RemoteReplayServiceErrorV1::ExpectedAuthorityMismatch(state) => {
                        state.as_ref().clone()
                    }
                    _ => fallback_authority,
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
                .expect("replay adapter preserves validated problem identities")
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
    if operation_receipt.committed_at < request.sent_at {
        return Err(RemoteProtocolFailureV1::AuthorityUnavailable);
    }
    let expected_state = operation_receipt.pre_state_digest.clone();
    let committed_state = operation_receipt.committed_effect_digest.clone();
    let deadline = Deadline::new(outcome.caller.expires_at)
        .map_err(|_| RemoteProtocolFailureV1::EnrollmentExpired)?;
    let execution = OperationReceipt::completed(
        request.sent_at,
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
        RemoteReplayServiceErrorV1::FrameSelectionRejected => {
            RemoteProtocolFailureV1::CallerAuthenticationFailed
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
                RemoteReplayTransactionErrorV1::CanonicalEffect
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
    ) -> Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1>;
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
    clock: &dyn RemoteReplayClockPortV1,
) -> Result<RemoteReplayOutcomeV1, RemoteReplayApplicationErrorV1> {
    let observed_at = clock.now()?;
    with_replay_attempt(spool, &frame.event_id, observed_at, |replay_attempt| {
        replay_remote_capture_attempt(
            authentication,
            policy,
            transaction,
            spool,
            authority_credential,
            caller_credential,
            presented_caller_credential,
            frame,
            current_writer,
            replay_attempt,
            observed_at,
            clock,
        )
    })
}

fn with_replay_attempt<T>(
    spool: &dyn RemoteReplaySpoolPortV1,
    event_id: &str,
    observed_at: UtcMicros,
    operation: impl FnOnce(u64) -> Result<T, RemoteReplayApplicationErrorV1>,
) -> Result<T, RemoteReplayApplicationErrorV1> {
    let replay_attempt = spool
        .begin_replay_attempt(event_id, observed_at)
        .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
    let result = operation(replay_attempt);
    if result.is_err() {
        spool
            .abandon_replay_attempt(event_id, replay_attempt)
            .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn replay_remote_capture_attempt(
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
    clock: &dyn RemoteReplayClockPortV1,
) -> Result<RemoteReplayOutcomeV1, RemoteReplayApplicationErrorV1> {
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
                clock.now()?,
                Some(RemoteReplayFindingV1::EnrollmentRevoked),
                None,
            )?;
        }
        return Err(RemoteReplayApplicationErrorV1::Authentication(error));
    }

    let spool_state = spool
        .state(&frame.event_id)
        .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
    if let Some(previous_event_id) = &frame.capture.sequence.previous_event_id {
        let predecessor = spool
            .state(previous_event_id)
            .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
        if !matches!(
            predecessor.state,
            RemoteReplayStateV1::Acknowledged | RemoteReplayStateV1::GarbageCollectionEligible
        ) {
            return Err(RemoteReplayApplicationErrorV1::InvalidSpoolState);
        }
    }
    if matches!(
        spool_state.state,
        RemoteReplayStateV1::Admitted | RemoteReplayStateV1::Duplicate
    ) {
        let receipt = spool_state
            .receipt
            .ok_or(RemoteReplayApplicationErrorV1::ReceiptMissing)?;
        receipt.validate_for(frame, current_writer)?;
        let acknowledged_at = clock.now()?;
        if receipt.committed_at > acknowledged_at {
            return Err(RemoteReplayApplicationErrorV1::ReceiptMismatch);
        }
        let terminal = acknowledge(
            spool,
            frame,
            spool_state.state,
            replay_attempt,
            acknowledged_at,
            receipt.clone(),
        )?;
        let operation_receipt =
            replay_operation_receipt(&terminal, &terminal, Some(receipt.clone()))?;
        return Ok(RemoteReplayOutcomeV1::Acknowledged {
            disposition: spool_state.state,
            receipt,
            operation_receipt,
        });
    }
    if spool_state.state != RemoteReplayStateV1::Pending {
        return Err(RemoteReplayApplicationErrorV1::InvalidSpoolState);
    }

    match policy.authorize_current_policy(frame, observed_at)? {
        RemoteReplayPolicyDecisionV1::Reject => {
            let terminal = transition(
                spool,
                frame,
                RemoteReplayStateV1::Pending,
                RemoteReplayStateV1::Rejected,
                replay_attempt,
                clock.now()?,
                Some(RemoteReplayFindingV1::PolicyChanged),
                None,
            )?;
            return Ok(RemoteReplayOutcomeV1::Rejected {
                operation_receipt: replay_operation_receipt(&terminal, &terminal, None)?,
            });
        }
        RemoteReplayPolicyDecisionV1::Quarantine => {
            let terminal = transition(
                spool,
                frame,
                RemoteReplayStateV1::Pending,
                RemoteReplayStateV1::Quarantined,
                replay_attempt,
                clock.now()?,
                Some(RemoteReplayFindingV1::PolicyChanged),
                None,
            )?;
            return Ok(RemoteReplayOutcomeV1::Quarantined {
                operation_receipt: replay_operation_receipt(&terminal, &terminal, None)?,
            });
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
    let admitted_at = clock.now()?;
    if receipt.committed_at > admitted_at {
        return Err(RemoteReplayApplicationErrorV1::ReceiptMismatch);
    }
    let admitted = transition(
        spool,
        frame,
        RemoteReplayStateV1::Pending,
        disposition,
        replay_attempt,
        admitted_at,
        finding,
        Some(receipt.clone()),
    )?;
    let terminal = acknowledge(
        spool,
        frame,
        disposition,
        replay_attempt,
        clock.now()?,
        receipt.clone(),
    )?;
    let operation_receipt = replay_operation_receipt(&admitted, &terminal, Some(receipt.clone()))?;
    Ok(RemoteReplayOutcomeV1::Acknowledged {
        disposition,
        receipt,
        operation_receipt,
    })
}

pub fn mark_remote_capture_gc_eligible(
    spool: &dyn RemoteReplaySpoolPortV1,
    frame: &RemoteReplayFrameV1,
    receipt: RemoteReplayCommitReceiptV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteReplayApplicationErrorV1> {
    let state = spool
        .state(&frame.event_id)
        .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
    if state.state != RemoteReplayStateV1::Acknowledged || state.receipt.as_ref() != Some(&receipt)
    {
        return Err(RemoteReplayApplicationErrorV1::InvalidSpoolState);
    }
    let replay_attempt = spool
        .begin_replay_attempt(&frame.event_id, observed_at)
        .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
    let result = transition(
        spool,
        frame,
        RemoteReplayStateV1::Acknowledged,
        RemoteReplayStateV1::GarbageCollectionEligible,
        replay_attempt,
        observed_at,
        None,
        Some(receipt),
    );
    if result.is_err() {
        spool
            .abandon_replay_attempt(&frame.event_id, replay_attempt)
            .map_err(RemoteReplayApplicationErrorV1::Persistence)?;
    }
    result.map(|_| ())
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
) -> Result<RemoteReplayTransitionReceiptV1, RemoteReplayApplicationErrorV1> {
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
) -> Result<RemoteReplayTransitionReceiptV1, RemoteReplayApplicationErrorV1> {
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

fn replay_operation_receipt(
    first: &RemoteReplayTransitionReceiptV1,
    terminal: &RemoteReplayTransitionReceiptV1,
    transaction: Option<RemoteReplayCommitReceiptV1>,
) -> Result<RemoteReplayOperationReceiptV1, RemoteReplayApplicationErrorV1> {
    if first.event_id != terminal.event_id
        || first.replay_attempt != terminal.replay_attempt
        || first.committed_at > terminal.committed_at
        || (first != terminal && first.to != terminal.from)
        || !matches!(
            terminal.to,
            RemoteReplayStateV1::Acknowledged
                | RemoteReplayStateV1::Rejected
                | RemoteReplayStateV1::Quarantined
        )
        || transaction
            .as_ref()
            .is_some_and(|receipt| receipt.committed_at > terminal.committed_at)
    {
        return Err(RemoteReplayApplicationErrorV1::ReceiptMismatch);
    }
    let budget = if first == terminal {
        first.budget
    } else {
        OperationBudgetUsage {
            units_consumed: first
                .budget
                .units_consumed
                .checked_add(terminal.budget.units_consumed)
                .ok_or(RemoteReplayApplicationErrorV1::ReceiptMismatch)?,
            bytes_consumed: first
                .budget
                .bytes_consumed
                .checked_add(terminal.budget.bytes_consumed)
                .ok_or(RemoteReplayApplicationErrorV1::ReceiptMismatch)?,
            elapsed_micros: first
                .budget
                .elapsed_micros
                .checked_add(terminal.budget.elapsed_micros)
                .ok_or(RemoteReplayApplicationErrorV1::ReceiptMismatch)?,
        }
    };
    let committed_effect_digest = if let Some(transaction) = &transaction {
        canonical_sha256(transaction)
            .map_err(|_| RemoteReplayApplicationErrorV1::ReceiptMismatch)?
    } else {
        terminal.terminal_state_digest.clone()
    };
    Ok(RemoteReplayOperationReceiptV1 {
        event_id: first.event_id.clone(),
        replay_attempt: first.replay_attempt,
        pre_state_digest: first.pre_state_digest.clone(),
        terminal_state_digest: terminal.terminal_state_digest.clone(),
        committed_effect_digest,
        committed_at: terminal.committed_at,
        budget,
        transaction,
    })
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
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

    #[test]
    fn replay_protocol_failures_preserve_concealment_and_staleness() {
        assert_eq!(
            replay_protocol_failure(RemoteReplayServiceErrorV1::FrameSelectionRejected),
            RemoteProtocolFailureV1::CallerAuthenticationFailed
        );
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

    #[test]
    fn replay_operation_receipt_rejects_timestamp_inversion() {
        let digest = ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let receipt = |committed_at| RemoteReplayTransitionReceiptV1 {
            event_id: "remote.event.test".into(),
            replay_attempt: 1,
            from: RemoteReplayStateV1::Pending,
            to: RemoteReplayStateV1::Rejected,
            pre_state_digest: digest.clone(),
            terminal_state_digest: digest.clone(),
            committed_at,
            budget: OperationBudgetUsage {
                units_consumed: 1,
                bytes_consumed: 1,
                elapsed_micros: 1,
            },
        };
        assert_eq!(
            replay_operation_receipt(&receipt(UtcMicros(2)), &receipt(UtcMicros(1)), None),
            Err(RemoteReplayApplicationErrorV1::ReceiptMismatch)
        );
    }
}
