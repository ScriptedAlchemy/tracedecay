//! Authenticated application owner for backup, staged restore, and promotion.
//!
//! The owner retains no path, database, transport, or credential bytes. A
//! registered durable adapter performs the effects and returns an exact receipt
//! bound to the request, caller, authority fence, and committed state.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BrainNodeId, CurrentRemoteAuthorityStateV1, ManifestDigest, RemoteRepositoryScopeV1, UtcMicros,
    canonical_sha256,
};
use tracedecay_tool_catalog::{EffectClass, SchemaId, UseCaseId};

use super::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    RecoveryAuthorityExpectationV1, StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use crate::remote::auth::OpaqueRemoteCredential;
use crate::remote::credential_admission::{
    RemoteAuthenticatedSessionV1, RemoteCredentialAdmissionErrorV1,
    RemoteCredentialAdmissionPortV1, RemoteCredentialUseV1, RemoteSessionBoundProtocolBodyV1,
};
use crate::remote::protocol::{
    RemoteProtocolFailureV1, RemoteProtocolPortV1, RemoteProtocolRequestV1,
    RemoteProtocolResponseV1, remote_protocol_problem,
};
use crate::{
    ApplicationContractError, ApplicationEnvelope, CancellationObservation, CancellationStage,
    Deadline, EffectId, EffectReceipt, EffectResult, EffectTermination, IdempotencyKey,
    OperationReceipt, OperationTermination, ReconciliationState, RequestId, ResultContractRef,
};

pub const REMOTE_BACKUP_USE_CASE_ID_V1: &str = "use-case.remote.backup";
pub const REMOTE_RESTORE_USE_CASE_ID_V1: &str = "use-case.remote.restore";
pub const REMOTE_PROMOTION_USE_CASE_ID_V1: &str = "use-case.remote.promotion";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRecoveryInterruptionV1 {
    Cancelled,
    DeadlineExceeded,
}

pub trait RemoteRecoveryControlPortV1: Send + Sync {
    /// Once an interruption is returned for a request, later observations must
    /// return the same value.
    fn interruption(&self, request_id: &RequestId) -> Option<RemoteRecoveryInterruptionV1>;

    fn effective_deadline(&self, _request_id: &RequestId) -> Option<UtcMicros> {
        None
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteRecoveryCallerV1 {
    pub node_id: BrainNodeId,
    pub enrollment_id: tracedecay_domain::EntityId,
    pub enrollment_revision: u64,
    pub scope: RemoteRepositoryScopeV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRecoveryTerminationV1 {
    Completed,
    CancelledBeforeEffect,
    TimedOutBeforeEffect,
    RolledBackBeforePublication,
    ForwardRecoveryRequired,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteRecoveryOperationReceiptV1 {
    pub request_id: RequestId,
    pub operation_id: String,
    pub caller: RemoteRecoveryCallerV1,
    pub expected: RecoveryAuthorityExpectationV1,
    pub input_digest: ManifestDigest,
    pub pre_state_digest: ManifestDigest,
    pub committed_state_digest: Option<ManifestDigest>,
    pub policy_digest: ManifestDigest,
    pub started_at: UtcMicros,
    pub committed_at: UtcMicros,
    pub units_consumed: u64,
    pub bytes_consumed: u64,
    pub termination: RemoteRecoveryTerminationV1,
    pub interruption_observed_after_commit: Option<RemoteRecoveryInterruptionV1>,
}

impl RemoteRecoveryOperationReceiptV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.operation_id.is_empty()
            || self.operation_id.len() > 512
            || self.operation_id.trim() != self.operation_id
            || self.operation_id.chars().any(char::is_control)
            || self.caller.enrollment_revision == 0
            || self.committed_at < self.started_at
            || self.units_consumed == 0
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote recovery receipt",
            });
        }
        self.caller.node_id.validate()?;
        self.caller.enrollment_id.validate()?;
        self.caller.scope.validate()?;
        self.expected.validate()?;
        self.input_digest.validate()?;
        self.pre_state_digest.validate()?;
        self.policy_digest.validate()?;
        if let Some(digest) = &self.committed_state_digest {
            digest.validate()?;
        }
        if self.termination == RemoteRecoveryTerminationV1::Completed
            && self.committed_state_digest.is_none()
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "completed remote recovery receipt",
            });
        }
        if self.interruption_observed_after_commit.is_some()
            && self.termination != RemoteRecoveryTerminationV1::Completed
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote recovery post-commit interruption",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRecoveryCommittedV1<T> {
    pub authority: CurrentRemoteAuthorityStateV1,
    pub receipt: RemoteRecoveryOperationReceiptV1,
    pub output: T,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteRecoveryOperationErrorV1 {
    InvalidRequest,
    Authentication,
    StaleAuthority,
    Conflict,
    Cancelled,
    TimedOut,
    RecoveryRequired,
    Unavailable,
    Corruption,
}

/// Durable recovery authority. Implementations must return the original
/// receipt for an exact retry and reject the same operation identity with
/// different input. Physical effects are private to the adapter.
pub trait RemoteRecoveryOperationPortV1: Send + Sync {
    fn current_authority(
        &self,
        expected: &RecoveryAuthorityExpectationV1,
        observed_at: UtcMicros,
    ) -> CurrentRemoteAuthorityStateV1;

    fn create_backup(
        &self,
        request: &RemoteProtocolRequestV1<BackupRequestV1>,
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
    ) -> Result<RemoteRecoveryCommittedV1<BackupOperationStateV1>, RemoteRecoveryOperationErrorV1>;

    fn publish_staged_restore(
        &self,
        request: &RemoteProtocolRequestV1<StagedRestoreConfirmationV1>,
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
    ) -> Result<RemoteRecoveryCommittedV1<StagedRestoreProgressV1>, RemoteRecoveryOperationErrorV1>;

    fn promote(
        &self,
        request: &RemoteProtocolRequestV1<PromotionConfirmationV1>,
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
    ) -> Result<RemoteRecoveryCommittedV1<PromotionCasReceiptV1>, RemoteRecoveryOperationErrorV1>;
}

pub struct RemoteRecoveryProtocolOwnerV1 {
    credentials: Arc<dyn RemoteCredentialAdmissionPortV1>,
    operations: Arc<dyn RemoteRecoveryOperationPortV1>,
    control: Arc<dyn RemoteRecoveryControlPortV1>,
    clock: fn() -> UtcMicros,
}

impl RemoteRecoveryProtocolOwnerV1 {
    pub fn new(
        credentials: Arc<dyn RemoteCredentialAdmissionPortV1>,
        operations: Arc<dyn RemoteRecoveryOperationPortV1>,
        control: Arc<dyn RemoteRecoveryControlPortV1>,
        clock: fn() -> UtcMicros,
    ) -> Self {
        Self {
            credentials,
            operations,
            control,
            clock,
        }
    }

    fn admit<Request>(
        &self,
        request: &RemoteProtocolRequestV1<Request>,
        credential: &OpaqueRemoteCredential,
        use_case: RemoteCredentialUseV1,
        reauthorize: bool,
    ) -> Result<(RemoteAuthenticatedSessionV1, RemoteRecoveryCallerV1), RemoteProtocolFailureV1>
    where
        Request: RemoteSessionBoundProtocolBodyV1,
    {
        let observed_at = (self.clock)();
        let mut session = self
            .credentials
            .admit_before_body(credential, use_case, observed_at)
            .map_err(map_admission_error)?;
        Request::bind_authenticated_session(&session, request)
            .map_err(|_| RemoteProtocolFailureV1::CallerAuthenticationFailed)?;
        if reauthorize {
            session = self
                .credentials
                .reauthorize_publication(&session, (self.clock)())
                .map_err(map_admission_error)?;
            Request::bind_authenticated_session(&session, request)
                .map_err(|_| RemoteProtocolFailureV1::CallerAuthenticationFailed)?;
        }
        let enrollment = session
            .enrollment_commit_receipt()
            .ok_or(RemoteProtocolFailureV1::CallerAuthenticationFailed)?;
        let caller = RemoteRecoveryCallerV1 {
            node_id: session.node_id().clone(),
            enrollment_id: enrollment.enrollment.enrollment_id.clone(),
            enrollment_revision: enrollment.enrollment.revision,
            scope: session.scope().clone(),
        };
        Ok((session, caller))
    }

    fn failure_response<T>(
        &self,
        request_id: RequestId,
        expected: &RecoveryAuthorityExpectationV1,
        failure: RemoteProtocolFailureV1,
        contract: ResultContractRef,
    ) -> Result<RemoteProtocolResponseV1<T>, ApplicationContractError> {
        let authority = self.operations.current_authority(expected, (self.clock)());
        let problem = remote_protocol_problem(contract, request_id.clone(), failure)?;
        RemoteProtocolResponseV1::new(request_id, authority, Err(problem))
    }

    fn effect_envelope<T>(
        &self,
        request_id: RequestId,
        operation: &str,
        session: &RemoteAuthenticatedSessionV1,
        committed: RemoteRecoveryCommittedV1<T>,
        contract: ResultContractRef,
    ) -> Result<ApplicationEnvelope<T>, RemoteProtocolFailureV1> {
        committed
            .receipt
            .validate()
            .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
        let enrollment = session
            .enrollment_commit_receipt()
            .ok_or(RemoteProtocolFailureV1::CallerAuthenticationFailed)?;
        let admission = &enrollment.admission;
        if admission.scope().project_id != committed.receipt.caller.scope.project_id
            || admission.scope().repository_id != committed.receipt.caller.scope.repository_id
            || admission.scope().worktree_id != committed.receipt.caller.scope.worktree_id
            || admission.scope().reference != committed.receipt.caller.scope.reference
        {
            return Err(RemoteProtocolFailureV1::ScopeMismatch);
        }
        let scope = admission.scope().clone();
        let operation_digest = canonical_sha256(&(
            "tracedecay.remote-recovery-effect.v1",
            operation,
            &committed.receipt.operation_id,
            &committed.receipt.input_digest,
        ))
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
        let identity = operation_digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(RemoteProtocolFailureV1::AuthorityUnavailable)?;
        let idempotency_key = IdempotencyKey::new(format!("remote.recovery.{identity}"))
            .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
        let effect_id = EffectId::new(format!("effect.remote.recovery.{identity}"))
            .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
        let enrollment_expires_at = session
            .enrollment_expires_at()
            .ok_or(RemoteProtocolFailureV1::EnrollmentExpired)?;
        let expires_at = self
            .control
            .effective_deadline(&request_id)
            .map_or(enrollment_expires_at, |request_deadline| {
                request_deadline.min(enrollment_expires_at)
            });
        let deadline =
            Deadline::new(expires_at).map_err(|_| RemoteProtocolFailureV1::EnrollmentExpired)?;
        let (effect_termination, operation_termination, cancellation, reconciliation) =
            termination_evidence(&committed.receipt);
        let execution = OperationReceipt {
            started_at: committed.receipt.started_at,
            ended_at: committed.receipt.committed_at,
            effective_deadline: deadline,
            cancellation,
            budget: crate::OperationBudgetUsage {
                units_consumed: committed.receipt.units_consumed,
                bytes_consumed: committed.receipt.bytes_consumed,
                elapsed_micros: committed
                    .receipt
                    .committed_at
                    .0
                    .saturating_sub(committed.receipt.started_at.0)
                    as u64,
            },
            termination: operation_termination,
        };
        let effect_receipt = EffectReceipt {
            operation: UseCaseId::new(operation)
                .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?,
            request_id: request_id.clone(),
            actor: admission.actor().clone(),
            scope: scope.clone(),
            effect_class: EffectClass::Administrative,
            idempotency_key: idempotency_key.clone(),
            input_digest: committed.receipt.input_digest,
            expected_state: committed.receipt.pre_state_digest.clone(),
            policy_digest: committed.receipt.policy_digest,
            configuration_digest: admission.configuration_digest().clone(),
            catalog_digest: admission.catalog_digest().clone(),
            privacy_digest: admission.privacy_digest().clone(),
            outcome: effect_termination,
            committed_state: committed.receipt.committed_state_digest,
            external_proof: None,
        };
        let effect = EffectResult::new(
            effect_id,
            EffectClass::Administrative,
            idempotency_key,
            admission.authority().clone(),
            committed.receipt.pre_state_digest,
            execution,
            reconciliation,
            effect_receipt,
            Some(committed.output),
        )
        .map_err(|_| RemoteProtocolFailureV1::AuthorityUnavailable)?;
        Ok(ApplicationEnvelope::effect(
            contract, request_id, scope, effect,
        ))
    }
}

fn termination_evidence(
    receipt: &RemoteRecoveryOperationReceiptV1,
) -> (
    EffectTermination,
    OperationTermination,
    Option<CancellationObservation>,
    ReconciliationState,
) {
    match receipt.termination {
        RemoteRecoveryTerminationV1::Completed => (
            EffectTermination::Completed,
            OperationTermination::Completed,
            receipt
                .interruption_observed_after_commit
                .map(|_| CancellationObservation {
                    stage: CancellationStage::AfterCommit,
                    observed_at: receipt.committed_at,
                }),
            ReconciliationState::Reconciled,
        ),
        RemoteRecoveryTerminationV1::CancelledBeforeEffect => (
            EffectTermination::Cancelled,
            OperationTermination::Cancelled,
            Some(CancellationObservation {
                stage: CancellationStage::BeforeEffect,
                observed_at: receipt.committed_at,
            }),
            ReconciliationState::Reconciled,
        ),
        RemoteRecoveryTerminationV1::TimedOutBeforeEffect => (
            EffectTermination::TimedOut,
            OperationTermination::TimedOut,
            Some(CancellationObservation {
                stage: CancellationStage::BeforeEffect,
                observed_at: receipt.committed_at,
            }),
            ReconciliationState::Reconciled,
        ),
        RemoteRecoveryTerminationV1::RolledBackBeforePublication => (
            EffectTermination::Failed,
            OperationTermination::Failed,
            None,
            ReconciliationState::Reconciled,
        ),
        RemoteRecoveryTerminationV1::ForwardRecoveryRequired => (
            EffectTermination::EffectUnknown,
            OperationTermination::EffectUnknown,
            None,
            ReconciliationState::Pending,
        ),
    }
}

fn result_contract(schema_id: &str) -> Result<ResultContractRef, ApplicationContractError> {
    let schema_id =
        SchemaId::new(schema_id).map_err(|_| ApplicationContractError::InvalidIdentifier {
            field: "remote recovery result schema",
        })?;
    ResultContractRef::new(schema_id, 1)
}

pub fn remote_backup_result_contract_v1() -> Result<ResultContractRef, ApplicationContractError> {
    result_contract("remote.backup.result")
}

pub fn remote_restore_result_contract_v1() -> Result<ResultContractRef, ApplicationContractError> {
    result_contract("remote.restore.result")
}

pub fn remote_promotion_result_contract_v1() -> Result<ResultContractRef, ApplicationContractError>
{
    result_contract("remote.promotion.result")
}

fn map_admission_error(error: RemoteCredentialAdmissionErrorV1) -> RemoteProtocolFailureV1 {
    match error {
        RemoteCredentialAdmissionErrorV1::NotYetValid
        | RemoteCredentialAdmissionErrorV1::Expired => RemoteProtocolFailureV1::EnrollmentExpired,
        RemoteCredentialAdmissionErrorV1::Revoked => RemoteProtocolFailureV1::EnrollmentRevoked,
        RemoteCredentialAdmissionErrorV1::InsufficientCapability => {
            RemoteProtocolFailureV1::InsufficientCapability
        }
        RemoteCredentialAdmissionErrorV1::BindingMismatch
        | RemoteCredentialAdmissionErrorV1::Rejected => {
            RemoteProtocolFailureV1::CallerAuthenticationFailed
        }
        RemoteCredentialAdmissionErrorV1::ResetRequired
        | RemoteCredentialAdmissionErrorV1::Unavailable => {
            RemoteProtocolFailureV1::AuthorityUnavailable
        }
    }
}

fn map_operation_error(error: RemoteRecoveryOperationErrorV1) -> RemoteProtocolFailureV1 {
    match error {
        RemoteRecoveryOperationErrorV1::Authentication => {
            RemoteProtocolFailureV1::CallerAuthenticationFailed
        }
        RemoteRecoveryOperationErrorV1::StaleAuthority
        | RemoteRecoveryOperationErrorV1::Conflict => RemoteProtocolFailureV1::StaleAuthorityFence,
        RemoteRecoveryOperationErrorV1::InvalidRequest => RemoteProtocolFailureV1::ScopeMismatch,
        RemoteRecoveryOperationErrorV1::Cancelled
        | RemoteRecoveryOperationErrorV1::TimedOut
        | RemoteRecoveryOperationErrorV1::RecoveryRequired
        | RemoteRecoveryOperationErrorV1::Unavailable
        | RemoteRecoveryOperationErrorV1::Corruption => {
            RemoteProtocolFailureV1::AuthorityUnavailable
        }
    }
}

macro_rules! impl_recovery_protocol {
    (
        $request:ty,
        $output:ty,
        $use_case:expr,
        $reauthorize:expr,
        $method:ident,
        $operation:expr,
        $schema:expr,
        $expected:expr
    ) => {
        impl RemoteProtocolPortV1<$request> for RemoteRecoveryProtocolOwnerV1 {
            type Output = $output;

            fn execute(
                &self,
                request: RemoteProtocolRequestV1<$request>,
                credential: OpaqueRemoteCredential,
            ) -> Result<RemoteProtocolResponseV1<Self::Output>, ApplicationContractError> {
                let request_id = request.request_id.clone();
                let expected = ($expected)(&request);
                let contract = result_contract($schema)?;
                let (session, caller) =
                    match self.admit(&request, &credential, $use_case, $reauthorize) {
                        Ok(admitted) => admitted,
                        Err(failure) => {
                            return self.failure_response(request_id, &expected, failure, contract);
                        }
                    };
                match self
                    .operations
                    .$method(&request, &caller, self.control.as_ref())
                {
                    Ok(committed) => {
                        let authority = committed.authority.clone();
                        let result = match self.effect_envelope(
                            request_id.clone(),
                            $operation,
                            &session,
                            committed,
                            contract.clone(),
                        ) {
                            Ok(envelope) => Ok(envelope),
                            Err(failure) => Err(remote_protocol_problem(
                                contract.clone(),
                                request_id.clone(),
                                failure,
                            )?),
                        };
                        RemoteProtocolResponseV1::new(request_id, authority, result)
                    }
                    Err(error) => self.failure_response(
                        request_id,
                        &expected,
                        map_operation_error(error),
                        contract,
                    ),
                }
            }
        }
    };
}

impl_recovery_protocol!(
    BackupRequestV1,
    BackupOperationStateV1,
    RemoteCredentialUseV1::CreateBackup,
    false,
    create_backup,
    REMOTE_BACKUP_USE_CASE_ID_V1,
    "remote.backup.result",
    |request: &RemoteProtocolRequestV1<BackupRequestV1>| request.body.expected.clone()
);
impl_recovery_protocol!(
    StagedRestoreConfirmationV1,
    StagedRestoreProgressV1,
    RemoteCredentialUseV1::PublishRestore,
    true,
    publish_staged_restore,
    REMOTE_RESTORE_USE_CASE_ID_V1,
    "remote.restore.result",
    |request: &RemoteProtocolRequestV1<StagedRestoreConfirmationV1>| {
        request
            .expected_authority
            .as_ref()
            .map(|writer| RecoveryAuthorityExpectationV1 {
                brain_id: writer.brain_id.as_str().to_owned(),
                shard_id: writer.shard_id.as_str().to_owned(),
                generation_id: writer.generation_id.as_str().to_owned(),
                authority_node_id: writer.authority_node_id.as_str().to_owned(),
                placement_revision: request.body.expected_placement_revision,
                authority_epoch: request.body.expected_authority_epoch,
            })
            .unwrap_or_else(|| RecoveryAuthorityExpectationV1 {
                brain_id: request.brain_id.as_str().to_owned(),
                shard_id: "unavailable".to_owned(),
                generation_id: "unavailable".to_owned(),
                authority_node_id: "unavailable".to_owned(),
                placement_revision: request.body.expected_placement_revision,
                authority_epoch: request.body.expected_authority_epoch,
            })
    }
);
impl_recovery_protocol!(
    PromotionConfirmationV1,
    PromotionCasReceiptV1,
    RemoteCredentialUseV1::Promote,
    true,
    promote,
    REMOTE_PROMOTION_USE_CASE_ID_V1,
    "remote.promotion.result",
    |request: &RemoteProtocolRequestV1<PromotionConfirmationV1>| {
        request
            .expected_authority
            .as_ref()
            .map(|writer| RecoveryAuthorityExpectationV1 {
                brain_id: writer.brain_id.as_str().to_owned(),
                shard_id: writer.shard_id.as_str().to_owned(),
                generation_id: writer.generation_id.as_str().to_owned(),
                authority_node_id: writer.authority_node_id.as_str().to_owned(),
                placement_revision: request.body.expected_placement_revision,
                authority_epoch: request.body.expected_authority_epoch,
            })
            .unwrap_or_else(|| RecoveryAuthorityExpectationV1 {
                brain_id: request.brain_id.as_str().to_owned(),
                shard_id: "unavailable".to_owned(),
                generation_id: "unavailable".to_owned(),
                authority_node_id: "unavailable".to_owned(),
                placement_revision: request.body.expected_placement_revision,
                authority_epoch: request.body.expected_authority_epoch,
            })
    }
);
