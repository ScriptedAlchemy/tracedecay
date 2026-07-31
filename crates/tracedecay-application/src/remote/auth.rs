//! Secret-safe remote enrollment and mutual-authentication application logic.

use std::collections::BTreeSet;
use std::fmt;
use std::hint::black_box;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ActorId, BrainId, BrainNodeId, CredentialRevocationReceiptV1, CredentialRotationReceiptV1,
    CurrentRemoteAuthorityStateV1, CurrentRemoteAuthorityV1, EnrollmentCredentialRecordV1,
    EnrollmentCredentialStateV1, EnrollmentGrantV1, EntityId, ManifestDigest,
    RemoteAuthorityUnavailableReasonV1, RemoteCapabilityV1, RemoteCredentialFingerprintV1,
    RemoteRepositoryScopeV1, UtcMicros, canonical_sha256, validate_remote_secret_length,
};
use tracedecay_tool_catalog::{EffectClass, UseCaseId};

use crate::{
    ApplicationEnvelope, AuthorityReceipt, Deadline, EffectId, EffectReceipt, EffectResult,
    EffectTermination, IdempotencyKey, OperationBudgetUsage, OperationReceipt, ReconciliationState,
    ResolvedScope, ResultContractRef,
};

use super::protocol::{
    EnrollmentRequestV1, REMOTE_ENROLLMENT_USE_CASE_ID_V1, RemoteEnrollmentProtocolPortV1,
    RemoteProtocolFailureV1, RemoteProtocolRequestV1, RemoteProtocolResponseV1,
    remote_enrollment_result_contract_v1, remote_protocol_problem,
};

/// Opaque credential accepted only at an application boundary.
///
/// It is intentionally neither `Clone` nor `Serialize`; debug output is always
/// redacted, and owned bytes are overwritten before release.
pub struct OpaqueRemoteCredential {
    bytes: Box<[u8]>,
}

impl OpaqueRemoteCredential {
    pub fn new(bytes: impl Into<Box<[u8]>>) -> Result<Self, RemoteAuthenticationError> {
        let mut bytes = bytes.into();
        if validate_remote_secret_length(&bytes).is_err() {
            bytes.fill(0);
            black_box(&bytes);
            return Err(RemoteAuthenticationError::InvalidCredential);
        }
        Ok(Self { bytes })
    }

    pub(crate) fn expose_for_authentication(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for OpaqueRemoteCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueRemoteCredential([REDACTED])")
    }
}

impl Drop for OpaqueRemoteCredential {
    fn drop(&mut self) {
        self.bytes.fill(0);
        black_box(&self.bytes);
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteAuthenticationError {
    #[error("remote credential is invalid")]
    InvalidCredential,
    #[error("remote enrollment record is invalid")]
    InvalidEnrollment,
    #[error("remote enrollment is expired")]
    Expired,
    #[error("remote enrollment is revoked")]
    Revoked,
    #[error("remote enrollment identity does not match the request")]
    IdentityMismatch,
    #[error("remote enrollment does not authorize the requested capability")]
    InsufficientCapability,
    #[error("remote enrollment does not authorize the requested repository scope")]
    ScopeMismatch,
    #[error("remote authority authentication failed")]
    AuthorityAuthenticationFailed,
    #[error("remote authority credential is stale or not authorized to serve")]
    InvalidAuthorityCredential,
    #[error("remote credential revision overflowed")]
    RevisionOverflow,
    #[error("remote credential revision is stale")]
    StaleRevision,
    #[error("remote credential validity interval is invalid")]
    InvalidValidity,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RemoteEnrollmentAuthorityErrorV1 {
    #[error("remote enrollment authority is unavailable")]
    Unavailable,
    #[error("remote enrollment grant was not found")]
    GrantNotFound,
    #[error("remote enrollment grant was already consumed")]
    GrantConsumed,
    #[error("remote enrollment identity conflicts with durable state")]
    IdentityConflict,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RemoteEnrollmentEvidenceErrorV1 {
    #[error("remote enrollment evidence contains an invalid field")]
    InvalidField,
    #[error("remote enrollment evidence does not match its authority")]
    AuthorityMismatch,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteEnrollmentAdmissionEvidenceV1 {
    result_contract: ResultContractRef,
    scope: ResolvedScope,
    authority: AuthorityReceipt,
    actor: ActorId,
    operation: UseCaseId,
    effect_id: EffectId,
    effect_class: EffectClass,
    idempotency_key: IdempotencyKey,
    configuration_digest: ManifestDigest,
    catalog_digest: ManifestDigest,
    privacy_digest: ManifestDigest,
    effective_deadline: Deadline,
}

impl RemoteEnrollmentAdmissionEvidenceV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        grant: &EnrollmentGrantV1,
        scope: ResolvedScope,
        authority: AuthorityReceipt,
        actor: ActorId,
        configuration_digest: ManifestDigest,
        catalog_digest: ManifestDigest,
        privacy_digest: ManifestDigest,
        effective_deadline: Deadline,
    ) -> Result<Self, RemoteEnrollmentEvidenceErrorV1> {
        let identity = format!("{}.{}", grant.grant_id.as_str(), grant.revision);
        let evidence = Self {
            result_contract: remote_enrollment_result_contract_v1(),
            scope,
            authority,
            actor,
            operation: UseCaseId::new(REMOTE_ENROLLMENT_USE_CASE_ID_V1)
                .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?,
            effect_id: EffectId::new(format!("effect.remote.enrollment.{identity}"))
                .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?,
            effect_class: EffectClass::Administrative,
            idempotency_key: IdempotencyKey::new(format!("remote.enrollment.{identity}"))
                .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?,
            configuration_digest,
            catalog_digest,
            privacy_digest,
            effective_deadline,
        };
        evidence.validate_for(grant)?;
        Ok(evidence)
    }

    pub fn validate_for(
        &self,
        grant: &EnrollmentGrantV1,
    ) -> Result<(), RemoteEnrollmentEvidenceErrorV1> {
        self.scope
            .validate()
            .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?;
        self.authority
            .validate_for(&self.scope)
            .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?;
        self.configuration_digest
            .validate()
            .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?;
        self.catalog_digest
            .validate()
            .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?;
        self.privacy_digest
            .validate()
            .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?;
        let grant_digest =
            canonical_sha256(grant).map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?;
        if self.authority.grant_id.as_str() != grant.grant_id.as_str()
            || self.authority.grant_revision != grant.revision
            || self.authority.grant_digest != grant_digest
            || self.scope.project_id != grant.scope.project_id
            || self.scope.repository_id != grant.scope.repository_id
            || self.scope.worktree_id != grant.scope.worktree_id
            || self.scope.reference != grant.scope.reference
            || self.result_contract != remote_enrollment_result_contract_v1()
            || self.operation.as_str() != REMOTE_ENROLLMENT_USE_CASE_ID_V1
            || self.effect_class != EffectClass::Administrative
            || self.effect_id.as_str()
                != format!(
                    "effect.remote.enrollment.{}.{}",
                    grant.grant_id.as_str(),
                    grant.revision
                )
            || self.idempotency_key.as_str()
                != format!(
                    "remote.enrollment.{}.{}",
                    grant.grant_id.as_str(),
                    grant.revision
                )
            || self
                .effective_deadline
                .is_elapsed_at(self.authority.revalidated_at)
        {
            return Err(RemoteEnrollmentEvidenceErrorV1::AuthorityMismatch);
        }
        Ok(())
    }

    pub fn result_contract(&self) -> &ResultContractRef {
        &self.result_contract
    }

    pub fn scope(&self) -> &ResolvedScope {
        &self.scope
    }

    pub fn authority(&self) -> &AuthorityReceipt {
        &self.authority
    }

    pub fn actor(&self) -> &ActorId {
        &self.actor
    }

    pub fn operation(&self) -> &UseCaseId {
        &self.operation
    }

    pub fn effect_id(&self) -> &EffectId {
        &self.effect_id
    }

    pub fn effect_class(&self) -> &EffectClass {
        &self.effect_class
    }

    pub fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    pub fn configuration_digest(&self) -> &ManifestDigest {
        &self.configuration_digest
    }

    pub fn catalog_digest(&self) -> &ManifestDigest {
        &self.catalog_digest
    }

    pub fn privacy_digest(&self) -> &ManifestDigest {
        &self.privacy_digest
    }

    pub fn effective_deadline(&self) -> &Deadline {
        &self.effective_deadline
    }
}

impl<'de> Deserialize<'de> for RemoteEnrollmentAdmissionEvidenceV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            result_contract: ResultContractRef,
            scope: ResolvedScope,
            authority: AuthorityReceipt,
            actor: ActorId,
            operation: UseCaseId,
            effect_id: EffectId,
            effect_class: EffectClass,
            idempotency_key: IdempotencyKey,
            configuration_digest: ManifestDigest,
            catalog_digest: ManifestDigest,
            privacy_digest: ManifestDigest,
            effective_deadline: Deadline,
        }

        let wire = Wire::deserialize(deserializer)?;
        let identity = format!(
            "{}.{}",
            wire.authority.grant_id.as_str(),
            wire.authority.grant_revision
        );
        if wire.result_contract != remote_enrollment_result_contract_v1()
            || wire.operation.as_str() != REMOTE_ENROLLMENT_USE_CASE_ID_V1
            || wire.effect_class != EffectClass::Administrative
            || wire.effect_id.as_str() != format!("effect.remote.enrollment.{identity}")
            || wire.idempotency_key.as_str() != format!("remote.enrollment.{identity}")
        {
            return Err(serde::de::Error::custom(
                "non-canonical remote enrollment admission identity",
            ));
        }
        Ok(Self {
            result_contract: wire.result_contract,
            scope: wire.scope,
            authority: wire.authority,
            actor: wire.actor,
            operation: wire.operation,
            effect_id: wire.effect_id,
            effect_class: wire.effect_class,
            idempotency_key: wire.idempotency_key,
            configuration_digest: wire.configuration_digest,
            catalog_digest: wire.catalog_digest,
            privacy_digest: wire.privacy_digest,
            effective_deadline: wire.effective_deadline,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteEnrollmentCommitReceiptV1 {
    pub admission: RemoteEnrollmentAdmissionEvidenceV1,
    pub prior_grant_digest: ManifestDigest,
    pub input_digest: ManifestDigest,
    pub committed_state_digest: ManifestDigest,
    pub consumed_at: UtcMicros,
    pub budget: OperationBudgetUsage,
    pub enrollment: EnrollmentCredentialRecordV1,
}

impl RemoteEnrollmentCommitReceiptV1 {
    pub fn validate(&self) -> Result<(), RemoteEnrollmentEvidenceErrorV1> {
        self.enrollment
            .validate()
            .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?;
        self.prior_grant_digest
            .validate()
            .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?;
        self.input_digest
            .validate()
            .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?;
        self.committed_state_digest
            .validate()
            .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?;
        if self.prior_grant_digest != self.admission.authority.grant_digest
            || self.committed_state_digest
                != canonical_sha256(&self.enrollment)
                    .map_err(|_| RemoteEnrollmentEvidenceErrorV1::InvalidField)?
            || self.consumed_at != self.enrollment.issued_at
            || self.admission.authority.revalidated_at > self.consumed_at
            || self.budget.units_consumed == 0
            || self.budget.bytes_consumed == 0
        {
            return Err(RemoteEnrollmentEvidenceErrorV1::AuthorityMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEnrollmentEffectOutcomeV1 {
    pub result_contract: ResultContractRef,
    pub scope: ResolvedScope,
    pub effect: EffectResult<EnrollmentCredentialRecordV1>,
}

/// Durable grant and enrollment authority. Implementations must atomically
/// consume the exact loaded grant while persisting the issued fingerprint-only
/// enrollment record.
pub trait RemoteEnrollmentAuthorityPortV1: Send + Sync {
    fn load_grant(
        &self,
        grant_id: &EntityId,
    ) -> Result<EnrollmentGrantV1, RemoteEnrollmentAuthorityErrorV1>;

    fn load_admission_evidence(
        &self,
        grant_id: &EntityId,
    ) -> Result<RemoteEnrollmentAdmissionEvidenceV1, RemoteEnrollmentAuthorityErrorV1>;

    fn commit_enrollment(
        &self,
        grant: &EnrollmentGrantV1,
        enrollment: &EnrollmentCredentialRecordV1,
        input_digest: &ManifestDigest,
        consumed_at: UtcMicros,
    ) -> Result<RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentAuthorityErrorV1>;
}

pub trait RemoteEnrollmentCredentialLookupPortV1: Send + Sync {
    fn enrollment_by_id(
        &self,
        enrollment_id: &EntityId,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1>;

    fn authority_enrollment(
        &self,
        brain_id: &BrainId,
        node_id: &BrainNodeId,
        revision: u64,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1>;

    fn enrollment_commit_receipt(
        &self,
        enrollment_id: &EntityId,
    ) -> Result<RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentAuthorityErrorV1>;
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteEnrollmentServiceErrorV1 {
    #[error("remote enrollment request is invalid")]
    InvalidRequest,
    #[error(transparent)]
    Authentication(#[from] RemoteAuthenticationError),
    #[error(transparent)]
    Authority(#[from] RemoteEnrollmentAuthorityErrorV1),
}

pub struct RemoteEnrollmentServiceV1<A> {
    authority: A,
}

impl<A> RemoteEnrollmentServiceV1<A>
where
    A: RemoteEnrollmentAuthorityPortV1,
{
    pub const fn new(authority: A) -> Self {
        Self { authority }
    }

    pub fn enroll(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        grant_credential: &OpaqueRemoteCredential,
        enrollment_credential: &OpaqueRemoteCredential,
    ) -> Result<RemoteEnrollmentEffectOutcomeV1, RemoteEnrollmentServiceErrorV1> {
        request
            .validate_initial_enrollment_metadata()
            .map_err(|_| RemoteEnrollmentServiceErrorV1::InvalidRequest)?;
        request
            .body
            .validate(request.sent_at)
            .map_err(|_| RemoteEnrollmentServiceErrorV1::InvalidRequest)?;
        if request.brain_id != request.body.brain_id
            || request.caller_node_id != request.body.node_id
        {
            return Err(RemoteEnrollmentServiceErrorV1::InvalidRequest);
        }
        let input_digest = canonical_sha256(&request)
            .map_err(|_| RemoteEnrollmentServiceErrorV1::InvalidRequest)?;
        let grant = self.authority.load_grant(&request.body.grant_id)?;
        let admission = self
            .authority
            .load_admission_evidence(&request.body.grant_id)?;
        admission
            .validate_for(&grant)
            .map_err(|_| RemoteEnrollmentServiceErrorV1::InvalidRequest)?;
        if admission
            .effective_deadline()
            .is_elapsed_at(request.sent_at)
        {
            return Err(RemoteEnrollmentServiceErrorV1::InvalidRequest);
        }
        let issue = EnrollmentIssueRequestV1 {
            grant_id: request.body.grant_id,
            grant_revision: request.body.grant_revision,
            enrollment_id: request.body.enrollment_id,
            brain_id: request.body.brain_id,
            node_id: request.body.node_id,
            issued_at: request.sent_at,
            expires_at: request.body.expires_at,
            capabilities: request.body.capabilities,
            scope: request.body.scope,
        };
        let enrollment = issue_enrollment(&grant, grant_credential, issue, enrollment_credential)?;
        let receipt = self.authority.commit_enrollment(
            &grant,
            &enrollment,
            &input_digest,
            request.sent_at,
        )?;
        receipt.validate().map_err(|_| {
            RemoteEnrollmentServiceErrorV1::Authority(
                RemoteEnrollmentAuthorityErrorV1::IdentityConflict,
            )
        })?;
        if receipt.admission != admission
            || receipt.input_digest != input_digest
            || receipt.enrollment != enrollment
            || receipt
                .admission
                .effective_deadline()
                .is_elapsed_at(receipt.consumed_at)
        {
            return Err(RemoteEnrollmentServiceErrorV1::Authority(
                RemoteEnrollmentAuthorityErrorV1::IdentityConflict,
            ));
        }
        let execution = OperationReceipt::completed(
            request.sent_at,
            receipt.consumed_at,
            admission.effective_deadline().clone(),
            receipt.budget,
        )
        .map_err(|_| RemoteEnrollmentServiceErrorV1::InvalidRequest)?;
        let effect_receipt = EffectReceipt {
            operation: admission.operation().clone(),
            request_id: request.request_id,
            actor: admission.actor().clone(),
            scope: admission.scope().clone(),
            effect_class: *admission.effect_class(),
            idempotency_key: admission.idempotency_key().clone(),
            input_digest,
            expected_state: receipt.prior_grant_digest.clone(),
            policy_digest: admission.authority().policy.digest.clone(),
            configuration_digest: admission.configuration_digest().clone(),
            catalog_digest: admission.catalog_digest().clone(),
            privacy_digest: admission.privacy_digest().clone(),
            outcome: EffectTermination::Completed,
            committed_state: Some(receipt.committed_state_digest.clone()),
            external_proof: None,
        };
        let effect = EffectResult::new(
            admission.effect_id().clone(),
            *admission.effect_class(),
            admission.idempotency_key().clone(),
            admission.authority().clone(),
            receipt.prior_grant_digest,
            execution,
            ReconciliationState::Reconciled,
            effect_receipt,
            Some(enrollment),
        )
        .map_err(|_| RemoteEnrollmentServiceErrorV1::InvalidRequest)?;
        Ok(RemoteEnrollmentEffectOutcomeV1 {
            result_contract: admission.result_contract().clone(),
            scope: admission.scope().clone(),
            effect,
        })
    }
}

pub struct RemoteEnrollmentProtocolAdapterV1<A> {
    service: RemoteEnrollmentServiceV1<A>,
}

impl<A> RemoteEnrollmentProtocolAdapterV1<A>
where
    A: RemoteEnrollmentAuthorityPortV1,
{
    pub fn new(authority: A) -> Self {
        Self {
            service: RemoteEnrollmentServiceV1::new(authority),
        }
    }
}

impl<A> RemoteEnrollmentProtocolPortV1 for RemoteEnrollmentProtocolAdapterV1<A>
where
    A: RemoteEnrollmentAuthorityPortV1,
{
    fn execute_enrollment(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        grant_credential: OpaqueRemoteCredential,
        enrollment_credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<EnrollmentCredentialRecordV1> {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        let result = match self
            .service
            .enroll(request, &grant_credential, &enrollment_credential)
        {
            Ok(outcome) if outcome.result_contract == remote_enrollment_result_contract_v1() => {
                Ok(ApplicationEnvelope::effect(
                    outcome.result_contract,
                    request_id.clone(),
                    outcome.scope,
                    outcome.effect,
                ))
            }
            Ok(_) => Err(remote_protocol_problem(
                remote_enrollment_result_contract_v1(),
                request_id.clone(),
                RemoteProtocolFailureV1::AuthorityUnavailable,
            )),
            Err(error) => Err(remote_protocol_problem(
                remote_enrollment_result_contract_v1(),
                request_id.clone(),
                enrollment_protocol_failure(error),
            )),
        };
        RemoteProtocolResponseV1::new(
            request_id,
            CurrentRemoteAuthorityStateV1::Unavailable {
                reason: RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
                observed_at,
            },
            result,
        )
        .expect("validated enrollment response identities are preserved")
    }
}

fn enrollment_protocol_failure(error: RemoteEnrollmentServiceErrorV1) -> RemoteProtocolFailureV1 {
    match error {
        RemoteEnrollmentServiceErrorV1::InvalidRequest => RemoteProtocolFailureV1::ScopeMismatch,
        RemoteEnrollmentServiceErrorV1::Authentication(authentication) => match authentication {
            RemoteAuthenticationError::Expired => RemoteProtocolFailureV1::EnrollmentExpired,
            RemoteAuthenticationError::Revoked => RemoteProtocolFailureV1::EnrollmentRevoked,
            RemoteAuthenticationError::InsufficientCapability => {
                RemoteProtocolFailureV1::InsufficientCapability
            }
            RemoteAuthenticationError::StaleRevision
            | RemoteAuthenticationError::RevisionOverflow => {
                RemoteProtocolFailureV1::StaleCredentialRevision
            }
            RemoteAuthenticationError::AuthorityAuthenticationFailed => {
                RemoteProtocolFailureV1::AuthorityAuthenticationFailed
            }
            RemoteAuthenticationError::InvalidAuthorityCredential => {
                RemoteProtocolFailureV1::AuthorityAuthenticationFailed
            }
            RemoteAuthenticationError::IdentityMismatch
            | RemoteAuthenticationError::ScopeMismatch
            | RemoteAuthenticationError::InvalidEnrollment
            | RemoteAuthenticationError::InvalidValidity => RemoteProtocolFailureV1::ScopeMismatch,
            RemoteAuthenticationError::InvalidCredential => {
                RemoteProtocolFailureV1::CallerAuthenticationFailed
            }
        },
        RemoteEnrollmentServiceErrorV1::Authority(authority) => match authority {
            RemoteEnrollmentAuthorityErrorV1::Unavailable
            | RemoteEnrollmentAuthorityErrorV1::IdentityConflict => {
                RemoteProtocolFailureV1::AuthorityUnavailable
            }
            RemoteEnrollmentAuthorityErrorV1::GrantNotFound => {
                RemoteProtocolFailureV1::CallerAuthenticationFailed
            }
            RemoteEnrollmentAuthorityErrorV1::GrantConsumed => {
                RemoteProtocolFailureV1::StaleCredentialRevision
            }
        },
    }
}

/// Metadata used to issue an enrollment. The secret remains a separate opaque
/// argument and never becomes part of this serializable record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrollmentIssueRequestV1 {
    pub grant_id: EntityId,
    pub grant_revision: u64,
    pub enrollment_id: EntityId,
    pub brain_id: BrainId,
    pub node_id: BrainNodeId,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub capabilities: BTreeSet<RemoteCapabilityV1>,
    pub scope: RemoteRepositoryScopeV1,
}

pub fn issue_enrollment(
    grant: &EnrollmentGrantV1,
    presented_grant: &OpaqueRemoteCredential,
    request: EnrollmentIssueRequestV1,
    credential: &OpaqueRemoteCredential,
) -> Result<EnrollmentCredentialRecordV1, RemoteAuthenticationError> {
    grant
        .validate()
        .map_err(|_| RemoteAuthenticationError::InvalidEnrollment)?;
    match grant.state_at(request.issued_at) {
        EnrollmentCredentialStateV1::Active => {}
        EnrollmentCredentialStateV1::NotYetValid => {
            return Err(RemoteAuthenticationError::InvalidEnrollment);
        }
        EnrollmentCredentialStateV1::Expired => return Err(RemoteAuthenticationError::Expired),
        EnrollmentCredentialStateV1::Revoked => return Err(RemoteAuthenticationError::Revoked),
    }
    if !fingerprints_equal(&grant.fingerprint, &fingerprint(presented_grant)?) {
        return Err(RemoteAuthenticationError::InvalidCredential);
    }
    if request.grant_id != grant.grant_id || request.grant_revision != grant.revision {
        return Err(RemoteAuthenticationError::StaleRevision);
    }
    if request.brain_id != grant.brain_id
        || request.node_id != grant.node_id
        || request.scope != grant.scope
        || request.expires_at > grant.expires_at
    {
        return Err(RemoteAuthenticationError::IdentityMismatch);
    }
    if !request.capabilities.is_subset(&grant.capabilities) {
        return Err(RemoteAuthenticationError::InsufficientCapability);
    }
    let record = EnrollmentCredentialRecordV1 {
        enrollment_id: request.enrollment_id,
        brain_id: request.brain_id,
        node_id: request.node_id,
        fingerprint: fingerprint(credential)?,
        revision: 1,
        issued_at: request.issued_at,
        expires_at: request.expires_at,
        revoked_at: None,
        capabilities: request.capabilities,
        scope: request.scope,
    };
    record
        .validate()
        .map_err(|_| RemoteAuthenticationError::InvalidEnrollment)?;
    Ok(record)
}

/// Authority authentication is delegated to the concrete network boundary.
/// An HTTP/rustls adapter must verify the connected authority peer; the
/// application never accepts a caller-supplied boolean or trust-root claim.
pub trait RemoteAuthorityAuthenticationPort {
    fn authenticate_connected_authority(
        &self,
        expected_authority: &CurrentRemoteAuthorityV1,
        expected_credential: &EnrollmentCredentialRecordV1,
        observed_at: UtcMicros,
    ) -> Result<(), RemoteAuthenticationError>;
}

/// Authenticate both sides of a remote request and reauthorize exact scope.
#[allow(clippy::too_many_arguments)]
pub fn authenticate_remote_request(
    authority_port: &dyn RemoteAuthorityAuthenticationPort,
    expected_authority: &CurrentRemoteAuthorityV1,
    authority_credential: &EnrollmentCredentialRecordV1,
    caller_credential: &EnrollmentCredentialRecordV1,
    presented_caller_credential: &OpaqueRemoteCredential,
    requested_capability: RemoteCapabilityV1,
    requested_scope: &RemoteRepositoryScopeV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteAuthenticationError> {
    expected_authority
        .validate()
        .map_err(|_| RemoteAuthenticationError::AuthorityAuthenticationFailed)?;
    validate_authority_credential(expected_authority, authority_credential, observed_at)?;
    authority_port.authenticate_connected_authority(
        expected_authority,
        authority_credential,
        observed_at,
    )?;
    authenticate_caller(
        caller_credential,
        presented_caller_credential,
        &expected_authority.fence.brain_id,
        requested_capability,
        requested_scope,
        observed_at,
    )
}

pub fn authenticate_caller(
    record: &EnrollmentCredentialRecordV1,
    presented: &OpaqueRemoteCredential,
    expected_brain: &BrainId,
    requested_capability: RemoteCapabilityV1,
    requested_scope: &RemoteRepositoryScopeV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteAuthenticationError> {
    record
        .validate()
        .map_err(|_| RemoteAuthenticationError::InvalidEnrollment)?;
    match record.state_at(observed_at) {
        EnrollmentCredentialStateV1::Active => {}
        EnrollmentCredentialStateV1::NotYetValid => {
            return Err(RemoteAuthenticationError::InvalidEnrollment);
        }
        EnrollmentCredentialStateV1::Expired => return Err(RemoteAuthenticationError::Expired),
        EnrollmentCredentialStateV1::Revoked => return Err(RemoteAuthenticationError::Revoked),
    }
    if &record.brain_id != expected_brain {
        return Err(RemoteAuthenticationError::IdentityMismatch);
    }
    if !fingerprints_equal(&record.fingerprint, &fingerprint(presented)?) {
        return Err(RemoteAuthenticationError::InvalidCredential);
    }
    if &record.scope != requested_scope {
        return Err(RemoteAuthenticationError::ScopeMismatch);
    }
    if !record.capabilities.contains(&requested_capability) {
        return Err(RemoteAuthenticationError::InsufficientCapability);
    }
    Ok(())
}

fn validate_authority_credential(
    authority: &CurrentRemoteAuthorityV1,
    record: &EnrollmentCredentialRecordV1,
    observed_at: UtcMicros,
) -> Result<(), RemoteAuthenticationError> {
    record
        .validate()
        .map_err(|_| RemoteAuthenticationError::InvalidAuthorityCredential)?;
    if record.brain_id != authority.fence.brain_id
        || record.node_id != authority.fence.authority_node_id
        || record.revision != authority.credential_revision
        || !record
            .capabilities
            .contains(&RemoteCapabilityV1::ServeAuthority)
        || record.state_at(observed_at) != EnrollmentCredentialStateV1::Active
    {
        return Err(RemoteAuthenticationError::InvalidAuthorityCredential);
    }
    Ok(())
}

pub fn rotate_credential(
    current: &EnrollmentCredentialRecordV1,
    expected_revision: u64,
    presented_current: &OpaqueRemoteCredential,
    replacement: &OpaqueRemoteCredential,
    rotated_at: UtcMicros,
    expires_at: UtcMicros,
) -> Result<(EnrollmentCredentialRecordV1, CredentialRotationReceiptV1), RemoteAuthenticationError>
{
    if current.revision != expected_revision {
        return Err(RemoteAuthenticationError::StaleRevision);
    }
    authenticate_caller(
        current,
        presented_current,
        &current.brain_id,
        RemoteCapabilityV1::RotateCredential,
        &current.scope,
        rotated_at,
    )?;
    if expires_at <= rotated_at {
        return Err(RemoteAuthenticationError::InvalidValidity);
    }
    let next_revision = current
        .revision
        .checked_add(1)
        .ok_or(RemoteAuthenticationError::RevisionOverflow)?;
    let next = EnrollmentCredentialRecordV1 {
        fingerprint: fingerprint(replacement)?,
        revision: next_revision,
        issued_at: rotated_at,
        expires_at,
        revoked_at: None,
        ..current.clone()
    };
    next.validate()
        .map_err(|_| RemoteAuthenticationError::InvalidEnrollment)?;
    let receipt = CredentialRotationReceiptV1 {
        enrollment_id: next.enrollment_id.clone(),
        node_id: next.node_id.clone(),
        prior_revision: current.revision,
        current_revision: next.revision,
        rotated_at,
        expires_at,
    };
    Ok((next, receipt))
}

/// Revoke a credential after the surrounding authority command has been
/// authenticated and authorized. Revocation is monotone and idempotent at an
/// already-revoked timestamp.
pub fn revoke_credential(
    current: &EnrollmentCredentialRecordV1,
    expected_revision: u64,
    revoked_at: UtcMicros,
) -> Result<(EnrollmentCredentialRecordV1, CredentialRevocationReceiptV1), RemoteAuthenticationError>
{
    current
        .validate()
        .map_err(|_| RemoteAuthenticationError::InvalidEnrollment)?;
    if current.revision != expected_revision {
        return Err(RemoteAuthenticationError::StaleRevision);
    }
    if revoked_at < current.issued_at {
        return Err(RemoteAuthenticationError::InvalidValidity);
    }
    if current.revoked_at == Some(revoked_at) {
        return Ok((
            current.clone(),
            CredentialRevocationReceiptV1 {
                enrollment_id: current.enrollment_id.clone(),
                node_id: current.node_id.clone(),
                prior_revision: current.revision.saturating_sub(1),
                current_revision: current.revision,
                revoked_at,
            },
        ));
    }
    if current.revoked_at.is_some() {
        return Err(RemoteAuthenticationError::Revoked);
    }
    let next_revision = current
        .revision
        .checked_add(1)
        .ok_or(RemoteAuthenticationError::RevisionOverflow)?;
    let next = EnrollmentCredentialRecordV1 {
        revision: next_revision,
        revoked_at: Some(revoked_at),
        ..current.clone()
    };
    let receipt = CredentialRevocationReceiptV1 {
        enrollment_id: next.enrollment_id.clone(),
        node_id: next.node_id.clone(),
        prior_revision: current.revision,
        current_revision: next.revision,
        revoked_at,
    };
    Ok((next, receipt))
}

fn fingerprint(
    credential: &OpaqueRemoteCredential,
) -> Result<RemoteCredentialFingerprintV1, RemoteAuthenticationError> {
    RemoteCredentialFingerprintV1::from_secret(credential.expose_for_authentication())
        .map_err(|_| RemoteAuthenticationError::InvalidCredential)
}

fn fingerprints_equal(
    left: &RemoteCredentialFingerprintV1,
    right: &RemoteCredentialFingerprintV1,
) -> bool {
    let left = left.digest().as_str().as_bytes();
    let right = right.digest().as_str().as_bytes();
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::{CapabilityGrantId, DisclosureClass, PolicyDecisionRef, RequestId};

    use super::*;
    use tracedecay_domain::{
        AuthorityEpoch, ComponentVersion, ProjectId, ProjectionGenerationId, RefId,
        RemotePlacementRevisionV1, RemoteWriterFenceV1, RepositoryId, RepositoryStateSnapshotId,
        ShardId, WorktreeId,
    };

    fn credential(value: u8) -> OpaqueRemoteCredential {
        OpaqueRemoteCredential::new(vec![value; 32].into_boxed_slice()).unwrap()
    }

    fn scope() -> RemoteRepositoryScopeV1 {
        RemoteRepositoryScopeV1 {
            project_id: ProjectId::new("project.remote").unwrap(),
            repository_id: RepositoryId::new("repository.remote").unwrap(),
            worktree_id: WorktreeId::new("worktree.remote").unwrap(),
            reference: Some(RefId::new("refs/heads/main").unwrap()),
            snapshot_id: RepositoryStateSnapshotId::new("repository.state.remote").unwrap(),
        }
    }

    fn request(capabilities: BTreeSet<RemoteCapabilityV1>) -> EnrollmentIssueRequestV1 {
        EnrollmentIssueRequestV1 {
            grant_id: EntityId::new("grant.remote").unwrap(),
            grant_revision: 1,
            enrollment_id: EntityId::new("enrollment.remote").unwrap(),
            brain_id: BrainId::new("brain.remote").unwrap(),
            node_id: BrainNodeId::new("node.remote").unwrap(),
            issued_at: UtcMicros(10),
            expires_at: UtcMicros(100),
            capabilities,
            scope: scope(),
        }
    }

    fn enroll(
        enrollment_credential: &OpaqueRemoteCredential,
        capabilities: BTreeSet<RemoteCapabilityV1>,
    ) -> EnrollmentCredentialRecordV1 {
        let grant_credential = credential(b'g');
        let grant = EnrollmentGrantV1 {
            grant_id: EntityId::new("grant.remote").unwrap(),
            brain_id: BrainId::new("brain.remote").unwrap(),
            node_id: BrainNodeId::new("node.remote").unwrap(),
            fingerprint: fingerprint(&grant_credential).unwrap(),
            revision: 1,
            issued_at: UtcMicros(1),
            expires_at: UtcMicros(100),
            revoked_at: None,
            capabilities: capabilities.clone(),
            scope: scope(),
        };
        issue_enrollment(
            &grant,
            &grant_credential,
            request(capabilities),
            enrollment_credential,
        )
        .unwrap()
    }

    struct TestEnrollmentAuthority {
        grant: EnrollmentGrantV1,
        admission: RemoteEnrollmentAdmissionEvidenceV1,
        committed: Mutex<Option<EnrollmentCredentialRecordV1>>,
    }

    impl RemoteEnrollmentAuthorityPortV1 for TestEnrollmentAuthority {
        fn load_grant(
            &self,
            grant_id: &EntityId,
        ) -> Result<EnrollmentGrantV1, RemoteEnrollmentAuthorityErrorV1> {
            if &self.grant.grant_id != grant_id {
                return Err(RemoteEnrollmentAuthorityErrorV1::GrantNotFound);
            }
            if self.committed.lock().unwrap().is_some() {
                return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
            }
            Ok(self.grant.clone())
        }

        fn load_admission_evidence(
            &self,
            grant_id: &EntityId,
        ) -> Result<RemoteEnrollmentAdmissionEvidenceV1, RemoteEnrollmentAuthorityErrorV1> {
            if &self.grant.grant_id != grant_id {
                return Err(RemoteEnrollmentAuthorityErrorV1::GrantNotFound);
            }
            if self.committed.lock().unwrap().is_some() {
                return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
            }
            Ok(self.admission.clone())
        }

        fn commit_enrollment(
            &self,
            grant: &EnrollmentGrantV1,
            enrollment: &EnrollmentCredentialRecordV1,
            input_digest: &ManifestDigest,
            consumed_at: UtcMicros,
        ) -> Result<RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentAuthorityErrorV1> {
            if grant != &self.grant {
                return Err(RemoteEnrollmentAuthorityErrorV1::IdentityConflict);
            }
            let mut committed = self.committed.lock().unwrap();
            if committed.is_some() {
                return Err(RemoteEnrollmentAuthorityErrorV1::GrantConsumed);
            }
            *committed = Some(enrollment.clone());
            Ok(RemoteEnrollmentCommitReceiptV1 {
                admission: self.admission.clone(),
                prior_grant_digest: canonical_sha256(grant).unwrap(),
                input_digest: input_digest.clone(),
                committed_state_digest: canonical_sha256(enrollment).unwrap(),
                consumed_at,
                budget: OperationBudgetUsage {
                    units_consumed: 2,
                    bytes_consumed: 256,
                    elapsed_micros: 1,
                },
                enrollment: enrollment.clone(),
            })
        }
    }

    fn enrollment_service(
        grant_credential: &OpaqueRemoteCredential,
    ) -> RemoteEnrollmentServiceV1<TestEnrollmentAuthority> {
        let grant = EnrollmentGrantV1 {
            grant_id: EntityId::new("grant.remote").unwrap(),
            brain_id: BrainId::new("brain.remote").unwrap(),
            node_id: BrainNodeId::new("node.remote").unwrap(),
            fingerprint: fingerprint(grant_credential).unwrap(),
            revision: 1,
            issued_at: UtcMicros(1),
            expires_at: UtcMicros(100),
            revoked_at: None,
            capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
            scope: scope(),
        };
        let resolved_scope = ResolvedScope::new(
            grant.scope.project_id.clone(),
            grant.scope.repository_id.clone(),
            grant.scope.worktree_id.clone(),
            grant.scope.reference.clone(),
        )
        .unwrap();
        let grant_digest = canonical_sha256(&grant).unwrap();
        let policy = PolicyDecisionRef::new(
            "policy.remote.enrollment",
            1,
            grant_digest.clone(),
            ComponentVersion::new("policy.remote.enrollment.v1").unwrap(),
        )
        .unwrap();
        let admission = RemoteEnrollmentAdmissionEvidenceV1::new(
            &grant,
            resolved_scope.clone(),
            AuthorityReceipt {
                grant_id: CapabilityGrantId::new(grant.grant_id.as_str()).unwrap(),
                grant_revision: grant.revision,
                grant_digest,
                authorized_scope_digest: resolved_scope.scope_digest,
                disclosure: DisclosureClass::Evidence,
                policy,
                revalidated_at: UtcMicros(10),
            },
            ActorId::new("actor.remote.node").unwrap(),
            ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
            ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
            ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
            Deadline::new(UtcMicros(100)).unwrap(),
        )
        .unwrap();
        RemoteEnrollmentServiceV1::new(TestEnrollmentAuthority {
            admission,
            grant,
            committed: Mutex::new(None),
        })
    }

    fn refresh_test_admission(service: &mut RemoteEnrollmentServiceV1<TestEnrollmentAuthority>) {
        let digest = canonical_sha256(&service.authority.grant).unwrap();
        service.authority.admission.authority.grant_digest = digest.clone();
        service.authority.admission.authority.policy.digest = digest;
    }

    fn protocol_enrollment_request(node_id: &str) -> RemoteProtocolRequestV1<EnrollmentRequestV1> {
        let brain_id = BrainId::new("brain.remote").unwrap();
        RemoteProtocolRequestV1::new_initial_enrollment(
            RequestId::new("request.remote.enrollment").unwrap(),
            brain_id.clone(),
            BrainNodeId::new(node_id).unwrap(),
            UtcMicros(10),
            EnrollmentRequestV1 {
                grant_id: EntityId::new("grant.remote").unwrap(),
                grant_revision: 1,
                enrollment_id: EntityId::new("enrollment.remote").unwrap(),
                brain_id,
                node_id: BrainNodeId::new(node_id).unwrap(),
                expires_at: UtcMicros(90),
                capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
                scope: scope(),
            },
        )
        .unwrap()
    }

    #[test]
    fn enrollment_service_consumes_grant_once_and_persists_only_fingerprint() {
        let grant_credential = credential(b'g');
        let enrollment_credential = credential(b'e');
        let service = enrollment_service(&grant_credential);

        let outcome = service
            .enroll(
                protocol_enrollment_request("node.remote"),
                &grant_credential,
                &enrollment_credential,
            )
            .unwrap();
        assert_eq!(
            outcome.effect.payload.as_ref().unwrap().fingerprint,
            fingerprint(&enrollment_credential).unwrap()
        );
        assert_eq!(
            service.enroll(
                protocol_enrollment_request("node.remote"),
                &grant_credential,
                &enrollment_credential,
            ),
            Err(RemoteEnrollmentServiceErrorV1::Authority(
                RemoteEnrollmentAuthorityErrorV1::GrantConsumed
            ))
        );
        let persisted = service.authority.committed.lock().unwrap();
        assert_eq!(persisted.as_ref(), outcome.effect.payload.as_ref());
        assert!(!format!("{persisted:?}").contains(&"e".repeat(32)));
    }

    #[test]
    fn enrollment_service_rejects_wrong_secret_and_node_identity() {
        let grant_credential = credential(b'g');
        let enrollment_credential = credential(b'e');
        let service = enrollment_service(&grant_credential);
        assert_eq!(
            service.enroll(
                protocol_enrollment_request("node.remote"),
                &credential(b'x'),
                &enrollment_credential,
            ),
            Err(RemoteEnrollmentServiceErrorV1::Authentication(
                RemoteAuthenticationError::InvalidCredential
            ))
        );
        assert_eq!(
            service.enroll(
                protocol_enrollment_request("node.other"),
                &grant_credential,
                &enrollment_credential,
            ),
            Err(RemoteEnrollmentServiceErrorV1::Authentication(
                RemoteAuthenticationError::IdentityMismatch
            ))
        );
        let mut wrong_scope = protocol_enrollment_request("node.remote");
        wrong_scope.body.scope.repository_id = RepositoryId::new("repository.other").unwrap();
        assert_eq!(
            service.enroll(wrong_scope, &grant_credential, &enrollment_credential,),
            Err(RemoteEnrollmentServiceErrorV1::Authentication(
                RemoteAuthenticationError::IdentityMismatch
            ))
        );
    }

    #[test]
    fn enrollment_service_rejects_expired_and_revoked_grants() {
        let grant_credential = credential(b'g');
        let enrollment_credential = credential(b'e');
        let mut expired = enrollment_service(&grant_credential);
        expired.authority.grant.expires_at = UtcMicros(10);
        refresh_test_admission(&mut expired);
        assert_eq!(
            expired.enroll(
                protocol_enrollment_request("node.remote"),
                &grant_credential,
                &enrollment_credential,
            ),
            Err(RemoteEnrollmentServiceErrorV1::Authentication(
                RemoteAuthenticationError::Expired
            ))
        );

        let mut revoked = enrollment_service(&grant_credential);
        revoked.authority.grant.revoked_at = Some(UtcMicros(5));
        refresh_test_admission(&mut revoked);
        assert_eq!(
            revoked.enroll(
                protocol_enrollment_request("node.remote"),
                &grant_credential,
                &enrollment_credential,
            ),
            Err(RemoteEnrollmentServiceErrorV1::Authentication(
                RemoteAuthenticationError::Revoked
            ))
        );
    }

    #[test]
    fn enrollment_rejects_deadline_at_exact_request_boundary() {
        let grant_credential = credential(b'g');
        let enrollment_credential = credential(b'e');
        let mut service = enrollment_service(&grant_credential);
        service.authority.admission.effective_deadline = Deadline::new(UtcMicros(10)).unwrap();
        assert_eq!(
            service.enroll(
                protocol_enrollment_request("node.remote"),
                &grant_credential,
                &enrollment_credential,
            ),
            Err(RemoteEnrollmentServiceErrorV1::InvalidRequest)
        );
        assert!(service.authority.committed.lock().unwrap().is_none());
    }

    #[test]
    fn enrollment_admission_deserialization_rejects_noncanonical_identities() {
        let grant_credential = credential(b'g');
        let service = enrollment_service(&grant_credential);
        let encoded = serde_json::to_value(&service.authority.admission).unwrap();
        for (field, invalid) in [
            ("operation", serde_json::json!("use-case.remote.query")),
            ("effect_id", serde_json::json!("effect.remote.other")),
            ("idempotency_key", serde_json::json!("remote.other")),
            ("effect_class", serde_json::json!("configuration_write")),
        ] {
            let mut candidate = encoded.clone();
            candidate[field] = invalid;
            assert!(
                serde_json::from_value::<RemoteEnrollmentAdmissionEvidenceV1>(candidate).is_err(),
                "{field} must fail closed"
            );
        }
    }

    #[test]
    fn enrollment_protocol_success_binds_effect_receipt_request_and_scope() {
        let grant_credential = credential(b'g');
        let service = enrollment_service(&grant_credential);
        let adapter = RemoteEnrollmentProtocolAdapterV1::new(service.authority);
        let response = adapter.execute_enrollment(
            protocol_enrollment_request("node.remote"),
            grant_credential,
            credential(b'e'),
        );

        assert!(matches!(
            response.authority,
            CurrentRemoteAuthorityStateV1::Unavailable {
                reason: RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
                ..
            }
        ));
        let envelope = response.result.unwrap();
        assert_eq!(envelope.request_id.as_str(), "request.remote.enrollment");
        assert_eq!(envelope.scope.project_id.as_str(), "project.remote");
        let crate::ApplicationOutcome::Effect(effect) = envelope.outcome else {
            panic!("enrollment must return a canonical effect outcome");
        };
        assert_eq!(
            effect.receipt.request_id.as_str(),
            "request.remote.enrollment"
        );
        assert_eq!(
            effect.receipt.scope.scope_digest,
            envelope.scope.scope_digest
        );
        assert_eq!(
            effect.receipt.committed_state,
            Some(canonical_sha256(effect.payload.as_ref().unwrap()).unwrap())
        );
    }

    #[test]
    fn enrollment_protocol_maps_replayed_grant_to_stale_problem() {
        let grant_credential = credential(b'g');
        let service = enrollment_service(&grant_credential);
        let adapter = RemoteEnrollmentProtocolAdapterV1::new(service.authority);
        adapter.execute_enrollment(
            protocol_enrollment_request("node.remote"),
            grant_credential,
            credential(b'e'),
        );
        let replay = adapter.execute_enrollment(
            protocol_enrollment_request("node.remote"),
            credential(b'g'),
            credential(b'e'),
        );
        assert!(matches!(
            replay.result.unwrap_err().problem.source(),
            crate::ApplicationProblem::Stale { .. }
        ));
    }

    #[test]
    fn opaque_credential_debug_is_redacted() {
        let secret = credential(b'x');
        assert_eq!(format!("{secret:?}"), "OpaqueRemoteCredential([REDACTED])");
    }

    #[test]
    fn wrong_expired_and_revoked_credentials_fail_closed() {
        let secret = credential(b'a');
        let mut record = enroll(&secret, BTreeSet::from([RemoteCapabilityV1::Query]));

        assert_eq!(
            authenticate_caller(
                &record,
                &credential(b'b'),
                &record.brain_id,
                RemoteCapabilityV1::Query,
                &scope(),
                UtcMicros(50),
            ),
            Err(RemoteAuthenticationError::InvalidCredential)
        );
        assert_eq!(
            authenticate_caller(
                &record,
                &secret,
                &record.brain_id,
                RemoteCapabilityV1::Query,
                &scope(),
                UtcMicros(100),
            ),
            Err(RemoteAuthenticationError::Expired)
        );
        record.revoked_at = Some(UtcMicros(40));
        assert_eq!(
            authenticate_caller(
                &record,
                &secret,
                &record.brain_id,
                RemoteCapabilityV1::Query,
                &scope(),
                UtcMicros(50),
            ),
            Err(RemoteAuthenticationError::Revoked)
        );
    }

    #[test]
    fn rotation_invalidates_old_secret_and_advances_revision() {
        let current_secret = credential(b'a');
        let replacement = credential(b'b');
        let record = enroll(
            &current_secret,
            BTreeSet::from([
                RemoteCapabilityV1::DiscoverAuthority,
                RemoteCapabilityV1::RotateCredential,
                RemoteCapabilityV1::Query,
            ]),
        );
        let (rotated, receipt) = rotate_credential(
            &record,
            1,
            &current_secret,
            &replacement,
            UtcMicros(20),
            UtcMicros(200),
        )
        .unwrap();

        assert_eq!(receipt.prior_revision, 1);
        assert_eq!(receipt.current_revision, 2);
        assert_eq!(
            authenticate_caller(
                &rotated,
                &current_secret,
                &rotated.brain_id,
                RemoteCapabilityV1::Query,
                &scope(),
                UtcMicros(30),
            ),
            Err(RemoteAuthenticationError::InvalidCredential)
        );
        authenticate_caller(
            &rotated,
            &replacement,
            &rotated.brain_id,
            RemoteCapabilityV1::Query,
            &scope(),
            UtcMicros(30),
        )
        .unwrap();
    }

    struct RejectAuthority;

    impl RemoteAuthorityAuthenticationPort for RejectAuthority {
        fn authenticate_connected_authority(
            &self,
            _expected_authority: &CurrentRemoteAuthorityV1,
            _expected_credential: &EnrollmentCredentialRecordV1,
            _observed_at: UtcMicros,
        ) -> Result<(), RemoteAuthenticationError> {
            Err(RemoteAuthenticationError::AuthorityAuthenticationFailed)
        }
    }

    #[test]
    fn authority_authentication_cannot_be_skipped() {
        let secret = credential(b'a');
        let authority = enroll(
            &secret,
            BTreeSet::from([
                RemoteCapabilityV1::DiscoverAuthority,
                RemoteCapabilityV1::RotateCredential,
                RemoteCapabilityV1::Query,
                RemoteCapabilityV1::ServeAuthority,
            ]),
        );
        let fence = RemoteWriterFenceV1 {
            brain_id: authority.brain_id.clone(),
            shard_id: ShardId::new("shard.remote").unwrap(),
            generation_id: ProjectionGenerationId::new("generation.remote").unwrap(),
            placement_revision: RemotePlacementRevisionV1::new(1).unwrap(),
            authority_epoch: AuthorityEpoch(1),
            authority_node_id: authority.node_id.clone(),
        };
        let current_authority = CurrentRemoteAuthorityV1 {
            fence,
            credential_revision: authority.revision,
            observed_at: UtcMicros(50),
        };
        assert_eq!(
            authenticate_remote_request(
                &RejectAuthority,
                &current_authority,
                &authority,
                &authority,
                &secret,
                RemoteCapabilityV1::Query,
                &scope(),
                UtcMicros(50),
            ),
            Err(RemoteAuthenticationError::AuthorityAuthenticationFailed)
        );
    }
}
