//! Authenticated protocol boundary for remote offline capture.
//!
//! The node-local daemon accepts a capture request only from the enrolled
//! credential, re-reads durable enrollment state, checks current policy
//! evidence, and admits the frame through [`RemoteCaptureServiceV1`], which
//! rejects capture whenever the owning authority is reachable.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CurrentRemoteAuthorityStateV1, DurableObservationV1, EnrollmentCredentialRecordV1,
    ManifestDigest, RemoteAuthorityUnavailableReasonV1, RemoteCapabilityV1,
    RemoteRepositoryScopeV1, UtcMicros, canonical_sha256,
};
use tracedecay_tool_catalog::{EffectClass, UseCaseId};

use crate::{
    ApplicationContractError, ApplicationEnvelope, Deadline, EffectId, EffectReceipt, EffectResult,
    EffectTermination, IdempotencyKey, OperationBudgetUsage, OperationReceipt, ReconciliationState,
};

use super::auth::{
    OpaqueRemoteCredential, RemoteAuthenticationError, RemoteEnrollmentAuthorityErrorV1,
    RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentCredentialLookupPortV1, authenticate_caller,
};
use super::capture::{
    RemoteCaptureApplicationErrorV1, RemoteCapturePersistenceErrorV1, RemoteCapturePortV1,
    RemoteCaptureReceiptV1, RemoteCaptureSequenceV1, RemoteCaptureServiceV1,
    RemoteOfflineCaptureCommandV1, RemoteWriterAuthorityV1,
};
use super::protocol::{
    REMOTE_CAPTURE_USE_CASE_ID_V1, REMOTE_PROTOCOL_VERSION_V1, RemoteProtocolBodyV1,
    RemoteProtocolFailureV1, RemoteProtocolPortV1, RemoteProtocolRequestV1,
    RemoteProtocolResponseV1, remote_capture_result_contract_v1, remote_protocol_problem,
};
use super::replay::{RemoteReplayApplicationErrorV1, RemoteReplayPolicyEvidenceV1};

/// Offline capture body. The credential is carried by the authenticated
/// transport boundary; the body binds writer identity, sequence linkage, and
/// the sanitized canonical observation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteCaptureRequestV1 {
    pub writer: RemoteWriterAuthorityV1,
    pub policy_revision: u64,
    pub sequence: RemoteCaptureSequenceV1,
    pub observation: DurableObservationV1,
}

impl RemoteCaptureRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.writer.validate().is_err() || self.sequence.validate().is_err() {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "remote capture writer or sequence",
            });
        }
        if self.policy_revision == 0 {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "remote capture policy revision",
            });
        }
        Ok(())
    }
}

impl RemoteProtocolBodyV1 for RemoteCaptureRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        _sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        self.validate()
    }
}

/// Current durable capture policy for one exact repository scope, reusing the
/// single replay policy record so capture and replay cannot diverge.
pub trait RemoteCapturePolicyEvidencePortV1: Send + Sync {
    fn capture_policy_evidence(
        &self,
        scope: &RemoteRepositoryScopeV1,
    ) -> Result<RemoteReplayPolicyEvidenceV1, RemoteReplayApplicationErrorV1>;
}

pub struct RemoteOfflineCaptureServiceOutcomeV1 {
    pub receipt: RemoteCaptureReceiptV1,
    pub caller: EnrollmentCredentialRecordV1,
    pub caller_admission: RemoteEnrollmentCommitReceiptV1,
    pub policy: RemoteReplayPolicyEvidenceV1,
    pub input_digest: ManifestDigest,
    pub captured_at: UtcMicros,
    pub completed_at: UtcMicros,
}

pub struct RemoteOfflineCaptureProtocolServiceV1<P> {
    credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
    policy: Arc<dyn RemoteCapturePolicyEvidencePortV1>,
    capture: RemoteCaptureServiceV1<P>,
    clock: fn() -> UtcMicros,
}

impl<P> RemoteOfflineCaptureProtocolServiceV1<P>
where
    P: RemoteCapturePortV1,
{
    pub fn new(
        credentials: Arc<dyn RemoteEnrollmentCredentialLookupPortV1>,
        policy: Arc<dyn RemoteCapturePolicyEvidencePortV1>,
        port: P,
        clock: fn() -> UtcMicros,
    ) -> Self {
        Self {
            credentials,
            policy,
            capture: RemoteCaptureServiceV1::new(port),
            clock,
        }
    }

    pub fn capture(
        &self,
        request: &RemoteProtocolRequestV1<RemoteCaptureRequestV1>,
        presented_credential: &OpaqueRemoteCredential,
    ) -> Result<RemoteOfflineCaptureServiceOutcomeV1, RemoteCaptureProtocolErrorV1> {
        if request.protocol_version != REMOTE_PROTOCOL_VERSION_V1 {
            return Err(RemoteCaptureProtocolErrorV1::UnsupportedVersion);
        }
        request
            .validate_metadata()
            .and_then(|()| request.body.validate())
            .map_err(|_| RemoteCaptureProtocolErrorV1::InvalidRequest)?;
        let input_digest =
            canonical_sha256(request).map_err(|_| RemoteCaptureProtocolErrorV1::InvalidRequest)?;
        let captured_at = (self.clock)();
        let caller = self
            .credentials
            .authority_enrollment(
                &request.brain_id,
                &request.caller_node_id,
                request.enrollment_revision,
            )
            .map_err(RemoteCaptureProtocolErrorV1::Credential)?;
        authenticate_caller(
            &caller,
            presented_credential,
            &request.brain_id,
            RemoteCapabilityV1::CaptureOffline,
            &request.body.writer.scope,
            captured_at,
        )
        .map_err(RemoteCaptureProtocolErrorV1::Authentication)?;
        let caller_admission = self
            .credentials
            .enrollment_commit_receipt(&caller.enrollment_id)
            .map_err(RemoteCaptureProtocolErrorV1::Credential)?;
        caller_admission
            .validate()
            .map_err(|_| RemoteCaptureProtocolErrorV1::ReceiptMismatch)?;
        if caller_admission.enrollment != caller {
            return Err(RemoteCaptureProtocolErrorV1::ReceiptMismatch);
        }
        let policy = self
            .policy
            .capture_policy_evidence(&request.body.writer.scope)
            .map_err(RemoteCaptureProtocolErrorV1::Policy)?;
        policy
            .validate()
            .map_err(RemoteCaptureProtocolErrorV1::Policy)?;
        if policy.repository_scope != request.body.writer.scope
            || policy.policy_revision < request.body.policy_revision
        {
            return Err(RemoteCaptureProtocolErrorV1::Policy(
                RemoteReplayApplicationErrorV1::PolicyMismatch,
            ));
        }
        let receipt = self
            .capture
            .capture(RemoteOfflineCaptureCommandV1 {
                enrollment: caller.clone(),
                writer: request.body.writer.clone(),
                policy_revision: request.body.policy_revision,
                sequence: request.body.sequence.clone(),
                observation: request.body.observation.clone(),
                captured_at,
            })
            .map_err(RemoteCaptureProtocolErrorV1::Capture)?;
        let completed_at = (self.clock)();
        Ok(RemoteOfflineCaptureServiceOutcomeV1 {
            receipt,
            caller,
            caller_admission,
            policy,
            input_digest,
            captured_at,
            completed_at,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RemoteCaptureProtocolErrorV1 {
    #[error("remote capture protocol version is unsupported")]
    UnsupportedVersion,
    #[error("remote capture request is invalid")]
    InvalidRequest,
    #[error("remote capture request does not match the durable caller enrollment")]
    ReceiptMismatch,
    #[error("remote capture credential authority failed")]
    Credential(RemoteEnrollmentAuthorityErrorV1),
    #[error("remote capture caller authentication failed")]
    Authentication(RemoteAuthenticationError),
    #[error("remote capture policy authority failed")]
    Policy(RemoteReplayApplicationErrorV1),
    #[error(transparent)]
    Capture(RemoteCaptureApplicationErrorV1),
}

pub struct RemoteOfflineCaptureProtocolAdapterV1<P> {
    service: RemoteOfflineCaptureProtocolServiceV1<P>,
}

impl<P> RemoteOfflineCaptureProtocolAdapterV1<P>
where
    P: RemoteCapturePortV1,
{
    pub fn new(service: RemoteOfflineCaptureProtocolServiceV1<P>) -> Self {
        Self { service }
    }
}

impl<P> RemoteProtocolPortV1<RemoteCaptureRequestV1> for RemoteOfflineCaptureProtocolAdapterV1<P>
where
    P: RemoteCapturePortV1,
{
    type Output = RemoteCaptureReceiptV1;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteCaptureRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> Result<RemoteProtocolResponseV1<Self::Output>, ApplicationContractError> {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        match self.service.capture(&request, &credential) {
            Ok(outcome) => {
                // A frame was admitted, so the owning authority was observed
                // unreachable at admission time; report exactly that state.
                let authority = CurrentRemoteAuthorityStateV1::Unavailable {
                    reason: RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
                    observed_at: outcome.captured_at,
                };
                let result = match capture_effect_envelope(request, outcome) {
                    Ok(envelope) => Ok(envelope),
                    Err(failure) => Err(remote_protocol_problem(
                        remote_capture_result_contract_v1(),
                        request_id.clone(),
                        failure,
                    )?),
                };
                RemoteProtocolResponseV1::new_or_unavailable(
                    request_id,
                    authority,
                    result,
                    remote_capture_result_contract_v1(),
                    observed_at,
                )
            }
            Err(error) => {
                let failure = capture_protocol_failure(error);
                let authority = CurrentRemoteAuthorityStateV1::Unavailable {
                    reason: RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
                    observed_at,
                };
                RemoteProtocolResponseV1::new_or_unavailable(
                    request_id.clone(),
                    authority,
                    Err(remote_protocol_problem(
                        remote_capture_result_contract_v1(),
                        request_id,
                        failure,
                    )?),
                    remote_capture_result_contract_v1(),
                    observed_at,
                )
            }
        }
    }
}

fn capture_effect_envelope(
    request: RemoteProtocolRequestV1<RemoteCaptureRequestV1>,
    outcome: RemoteOfflineCaptureServiceOutcomeV1,
) -> Result<ApplicationEnvelope<RemoteCaptureReceiptV1>, RemoteProtocolFailureV1> {
    let expected_state = canonical_sha256(&(
        "tracedecay.remote-capture-pre.v1",
        &request.body.sequence,
        outcome.caller.revision,
    ))
    .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let committed_state = canonical_sha256(&(
        "tracedecay.remote-capture-committed.v1",
        &outcome.receipt,
        outcome.caller.revision,
    ))
    .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let deadline = Deadline::new(outcome.caller.expires_at)
        .map_err(|_| RemoteProtocolFailureV1::EnrollmentExpired)?;
    let observation_bytes = serde_json::to_vec(&request.body.observation)
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?
        .len() as u64;
    let elapsed_micros = outcome
        .completed_at
        .0
        .checked_sub(outcome.captured_at.0)
        .and_then(|elapsed| u64::try_from(elapsed).ok())
        .ok_or(RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let execution = OperationReceipt::completed(
        request.sent_at,
        outcome.completed_at,
        deadline,
        OperationBudgetUsage {
            units_consumed: 1,
            bytes_consumed: observation_bytes.max(1),
            elapsed_micros,
        },
    )
    .map_err(|_| RemoteProtocolFailureV1::EnrollmentExpired)?;
    let event_digest = canonical_sha256(&(
        "tracedecay.remote-capture-effect.v1",
        &outcome.receipt.event_id,
        outcome.caller.revision,
    ))
    .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let event_digest_id = event_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let operation = UseCaseId::new(REMOTE_CAPTURE_USE_CASE_ID_V1)
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let effect_id = EffectId::new(format!("effect.remote.capture.{event_digest_id}"))
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let idempotency_key =
        IdempotencyKey::new(format!("idempotency.remote.capture.{event_digest_id}"))
            .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let mut authority = outcome.caller_admission.admission.authority().clone();
    authority.policy = outcome.policy.policy.clone();
    authority
        .validate_for(&outcome.policy.scope)
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    let receipt = EffectReceipt {
        operation,
        request_id: request.request_id.clone(),
        actor: outcome.caller_admission.admission.actor().clone(),
        scope: outcome.policy.scope.clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: idempotency_key.clone(),
        input_digest: outcome.input_digest,
        expected_state: expected_state.clone(),
        policy_digest: outcome.policy.policy.digest.clone(),
        configuration_digest: outcome.policy.configuration_digest.clone(),
        catalog_digest: outcome.policy.catalog_digest.clone(),
        privacy_digest: outcome.policy.privacy_digest.clone(),
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
        Some(outcome.receipt),
    )
    .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
    Ok(ApplicationEnvelope::effect(
        remote_capture_result_contract_v1(),
        request.request_id,
        outcome.policy.scope,
        effect,
    ))
}

fn capture_protocol_failure(error: RemoteCaptureProtocolErrorV1) -> RemoteProtocolFailureV1 {
    match error {
        RemoteCaptureProtocolErrorV1::UnsupportedVersion => {
            RemoteProtocolFailureV1::UnsupportedVersion
        }
        RemoteCaptureProtocolErrorV1::InvalidRequest
        | RemoteCaptureProtocolErrorV1::ReceiptMismatch => RemoteProtocolFailureV1::ScopeMismatch,
        RemoteCaptureProtocolErrorV1::Credential(
            RemoteEnrollmentAuthorityErrorV1::Unavailable
            | RemoteEnrollmentAuthorityErrorV1::IdentityConflict,
        ) => RemoteProtocolFailureV1::AuthorityUnavailable,
        RemoteCaptureProtocolErrorV1::Credential(
            RemoteEnrollmentAuthorityErrorV1::GrantConsumed,
        ) => RemoteProtocolFailureV1::StaleCredentialRevision,
        RemoteCaptureProtocolErrorV1::Credential(
            RemoteEnrollmentAuthorityErrorV1::GrantNotFound,
        ) => RemoteProtocolFailureV1::CallerAuthenticationFailed,
        RemoteCaptureProtocolErrorV1::Authentication(error) => match error {
            RemoteAuthenticationError::Expired => RemoteProtocolFailureV1::EnrollmentExpired,
            RemoteAuthenticationError::Revoked => RemoteProtocolFailureV1::EnrollmentRevoked,
            RemoteAuthenticationError::InsufficientCapability => {
                RemoteProtocolFailureV1::InsufficientCapability
            }
            RemoteAuthenticationError::ScopeMismatch => RemoteProtocolFailureV1::ScopeMismatch,
            RemoteAuthenticationError::StaleRevision => {
                RemoteProtocolFailureV1::StaleCredentialRevision
            }
            _ => RemoteProtocolFailureV1::CallerAuthenticationFailed,
        },
        RemoteCaptureProtocolErrorV1::Policy(_) => {
            RemoteProtocolFailureV1::CallerAuthenticationFailed
        }
        RemoteCaptureProtocolErrorV1::Capture(error) => match error {
            RemoteCaptureApplicationErrorV1::EnrollmentExpired => {
                RemoteProtocolFailureV1::EnrollmentExpired
            }
            RemoteCaptureApplicationErrorV1::EnrollmentRevoked => {
                RemoteProtocolFailureV1::EnrollmentRevoked
            }
            RemoteCaptureApplicationErrorV1::CaptureNotAuthorized => {
                RemoteProtocolFailureV1::InsufficientCapability
            }
            RemoteCaptureApplicationErrorV1::AuthorityReachable => {
                RemoteProtocolFailureV1::AuthorityReachable
            }
            RemoteCaptureApplicationErrorV1::Persistence(
                RemoteCapturePersistenceErrorV1::Overflow,
            ) => RemoteProtocolFailureV1::SpoolSaturated,
            RemoteCaptureApplicationErrorV1::InvalidEnrollment
            | RemoteCaptureApplicationErrorV1::InvalidSequence
            | RemoteCaptureApplicationErrorV1::WriterFenceMismatch => {
                RemoteProtocolFailureV1::ScopeMismatch
            }
            RemoteCaptureApplicationErrorV1::AuthorityReachabilityUnknown
            | RemoteCaptureApplicationErrorV1::InvalidPortResult
            | RemoteCaptureApplicationErrorV1::Persistence(_) => {
                RemoteProtocolFailureV1::AuthorityUnavailable
            }
        },
    }
}
