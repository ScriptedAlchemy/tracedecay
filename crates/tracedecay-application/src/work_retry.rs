//! Durable, evidence-authorized creation of a new Work attempt after failure.
//!
//! A retry is never an in-place transition of an existing attempt. The owner
//! resolves one canonical failure record, derives a fresh execution envelope
//! for an exact new [`AttemptId`], and commits the attempt and retry receipt in
//! one Work-storage transaction.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AttemptId, ManifestDigest, TopologyConcurrencyPolicyV1, UtcMicros, WorkAttemptIdentityV1,
    WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority,
    WorkCancellationStateV1, WorkCommandId, WorkEffectStateV1, WorkExecutionEnvelopeV1,
    WorkFenceEpochV1, WorkLeaseFenceV1, WorkLeaseId, WorkRecoveryStateV1, WorkRestartReasonV1,
    WorkRuntimeContractError, WorkTerminalEvidenceV1, WorkTopologyPolicyV1, canonical_sha256,
};

use crate::work::work_authority;
use crate::work_attempt::{
    CurrentWorkProductAttemptGraphV1, WorkAttemptStorageError, WorkAttemptStoragePort,
    accepted_attempt_draft,
    current_work_product_attempt_graph, product_admission_problem,
    product_attempt_projection_binding,
};
use crate::work_attempt_effect::{
    WorkAttemptEffectResolutionV1, WorkAttemptEffectStorageErrorV1, WorkAttemptEffectStoragePortV1,
};
use crate::work_read::{WorkProjectionPortError, WorkProjectionReadPort};
use crate::{
    ApplicationContractError, ApplicationProblem, LegalAction, RequestAdmission, RequestContext,
    RetryDirective, SafeDiagnostic, WorkGraphReadPortV1, WorkProductAttemptAdmissionPortV1,
    WorkProductAttemptAdmissionV1, WorkProductBindingV1, WorkProductOwnerAuthorizationPortV1,
    WorkProductRetryAdmissionV1, WorkProductRevisionPinsV1,
};

const RETRY_INPUT_DIGEST_DOMAIN: &str = "tracedecay.application.work-retry-input.v1";
const RETRY_RECEIPT_DIGEST_DOMAIN: &str = "tracedecay.application.work-retry-receipt.v1";
const RETRY_LEASE_DOMAIN: &str = "tracedecay.application.work-retry-lease.v1";
const WORK_PRODUCT_RETRY_INPUT_DIGEST_DOMAIN: &str =
    "tracedecay.application.work-product-retry-attempt.final-v2";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkRetrySourceV1 {
    Runtime,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkRetryCauseV1 {
    RuntimeFailure,
}

impl WorkRetryCauseV1 {
    const fn restart_reason(self) -> WorkRestartReasonV1 {
        match self {
            Self::RuntimeFailure => WorkRestartReasonV1::FailureObserved,
        }
    }
}

/// A selector into the owning runtime-terminal evidence authority.
///
/// `evidence_ref` is an opaque local reference. The Work retry owner resolves
/// it through [`WorkRetryEvidencePortV1`]; callers never submit the evidence
/// digest, outcome, or observation time that decides eligibility.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRetryFailureSelectorV1 {
    pub source: WorkRetrySourceV1,
    pub cause: WorkRetryCauseV1,
    pub evidence_ref: String,
}

impl WorkRetryFailureSelectorV1 {
    fn validate(&self) -> bool {
        self.source == WorkRetrySourceV1::Runtime
            && self.cause == WorkRetryCauseV1::RuntimeFailure
            && !self.evidence_ref.is_empty()
            && self.evidence_ref.len() <= 256
            && self.evidence_ref.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'-' | b'_')
            })
    }
}

/// Failure fact returned by a canonical evidence authority.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedWorkRetryFailureV1 {
    pub selector: WorkRetryFailureSelectorV1,
    pub evidence_digest: ManifestDigest,
    pub observed_at: UtcMicros,
}

/// Evidence authority used before a retry can reserve capacity.
pub trait WorkRetryEvidencePortV1: Send + Sync {
    fn resolve_failure(
        &self,
        authority: &WorkAuthority,
        original: &WorkAttemptV1,
        selector: &WorkRetryFailureSelectorV1,
    ) -> Result<VerifiedWorkRetryFailureV1, WorkRetryEvidenceErrorV1>;
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkRetryEvidenceErrorV1 {
    #[error("retry failure evidence was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("retry failure evidence is stale or does not bind the original attempt")]
    Conflict,
    #[error("retry failure evidence authority is unavailable")]
    Unavailable,
}

/// Canonical runtime-terminal evidence owner.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeWorkRetryEvidenceV1;

impl WorkRetryEvidencePortV1 for RuntimeWorkRetryEvidenceV1 {
    fn resolve_failure(
        &self,
        _authority: &WorkAuthority,
        original: &WorkAttemptV1,
        selector: &WorkRetryFailureSelectorV1,
    ) -> Result<VerifiedWorkRetryFailureV1, WorkRetryEvidenceErrorV1> {
        let terminal = original
            .terminal()
            .ok_or(WorkRetryEvidenceErrorV1::Conflict)?;
        let (digest, observed_at, eligible) = match terminal {
            WorkTerminalEvidenceV1::Failed {
                evidence_digest,
                observed_at,
            }
            | WorkTerminalEvidenceV1::TimedOut {
                evidence_digest,
                observed_at,
            } => (evidence_digest, observed_at, true),
            WorkTerminalEvidenceV1::Succeeded {
                evidence_digest,
                observed_at,
            }
            | WorkTerminalEvidenceV1::Cancelled {
                evidence_digest,
                observed_at,
            } => (evidence_digest, observed_at, false),
        };
        let expected_ref = format!("runtime-terminal:{}", digest.as_str());
        if !eligible
            || selector.cause != WorkRetryCauseV1::RuntimeFailure
            || selector.evidence_ref != expected_ref
        {
            return Err(WorkRetryEvidenceErrorV1::Conflict);
        }
        Ok(VerifiedWorkRetryFailureV1 {
            selector: selector.clone(),
            evidence_digest: digest.clone(),
            observed_at: *observed_at,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "RetryWorkAttemptCommandV1")]
pub struct RetryWorkAttemptCommandV1 {
    pub original_attempt: WorkAttemptIdentityV1,
    pub new_attempt_id: AttemptId,
    pub failure: WorkRetryFailureSelectorV1,
    pub command_id: WorkCommandId,
}

impl RetryWorkAttemptCommandV1 {
    fn validate(&self) -> bool {
        self.failure.validate() && &self.new_attempt_id != self.original_attempt.attempt_id()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRetryReceiptV1 {
    pub command: RetryWorkAttemptCommandV1,
    pub failure: VerifiedWorkRetryFailureV1,
    pub new_attempt: WorkAttemptIdentityV1,
    /// Exact source-owned time at which the failure made a retry necessary.
    pub retry_required_at: UtcMicros,
    /// Daemon-owned admission time at which the new attempt was created.
    pub restarted_at: UtcMicros,
    pub canonical_input_digest: ManifestDigest,
    pub owner_receipt_digest: ManifestDigest,
}

impl WorkRetryReceiptV1 {
    pub fn new(
        command: RetryWorkAttemptCommandV1,
        failure: VerifiedWorkRetryFailureV1,
        new_attempt: WorkAttemptIdentityV1,
        retry_required_at: UtcMicros,
        restarted_at: UtcMicros,
    ) -> Result<Self, ApplicationContractError> {
        let canonical_input_digest = canonical_sha256(&(RETRY_INPUT_DIGEST_DOMAIN, &command))?;
        let owner_receipt_digest = retry_receipt_digest(
            &command,
            &failure,
            &new_attempt,
            retry_required_at,
            restarted_at,
            &canonical_input_digest,
        )?;
        let receipt = Self {
            command,
            failure,
            new_attempt,
            retry_required_at,
            restarted_at,
            canonical_input_digest,
            owner_receipt_digest,
        };
        if receipt.validate_for_observation() {
            Ok(receipt)
        } else {
            Err(ApplicationContractError::Inconsistent {
                field: "Work retry receipt",
            })
        }
    }

    pub fn validate_for_observation(&self) -> bool {
        self.command.validate()
            && self.failure.selector == self.command.failure
            && self.failure.selector.validate()
            && self.failure.evidence_digest.validate().is_ok()
            && self.failure.observed_at == self.retry_required_at
            && self.restarted_at.0 >= self.retry_required_at.0
            && self.new_attempt.task_id() == self.command.original_attempt.task_id()
            && self.new_attempt.run_id() == self.command.original_attempt.run_id()
            && self.new_attempt.attempt_id() == &self.command.new_attempt_id
            && canonical_sha256(&(RETRY_INPUT_DIGEST_DOMAIN, &self.command))
                .is_ok_and(|digest| digest == self.canonical_input_digest)
            && retry_receipt_digest(
                &self.command,
                &self.failure,
                &self.new_attempt,
                self.retry_required_at,
                self.restarted_at,
                &self.canonical_input_digest,
            )
            .is_ok_and(|digest| digest == self.owner_receipt_digest)
    }
}

fn retry_receipt_digest(
    command: &RetryWorkAttemptCommandV1,
    failure: &VerifiedWorkRetryFailureV1,
    new_attempt: &WorkAttemptIdentityV1,
    retry_required_at: UtcMicros,
    restarted_at: UtcMicros,
    canonical_input_digest: &ManifestDigest,
) -> Result<ManifestDigest, tracedecay_domain::research::DomainError> {
    canonical_sha256(&(
        RETRY_RECEIPT_DIGEST_DOMAIN,
        command,
        failure,
        new_attempt,
        retry_required_at,
        restarted_at,
        canonical_input_digest,
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkRetryWriteV1 {
    pub receipt: WorkRetryReceiptV1,
    pub attempt: WorkAttemptV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkRetryAttemptOutcomeV1 {
    Created {
        receipt: WorkRetryReceiptV1,
        attempt: WorkAttemptV1,
    },
    Replayed {
        receipt: WorkRetryReceiptV1,
        attempt: WorkAttemptV1,
    },
}

impl WorkRetryAttemptOutcomeV1 {
    pub const fn receipt(&self) -> &WorkRetryReceiptV1 {
        match self {
            Self::Created { receipt, .. } | Self::Replayed { receipt, .. } => receipt,
        }
    }

    pub const fn attempt(&self) -> &WorkAttemptV1 {
        match self {
            Self::Created { attempt, .. } | Self::Replayed { attempt, .. } => attempt,
        }
    }
}

pub trait WorkRetryStoragePortV1: WorkAttemptStoragePort {
    fn retry_by_command(
        &self,
        authority: &WorkAuthority,
        command_id: &WorkCommandId,
    ) -> Result<Option<WorkRetryAttemptOutcomeV1>, WorkAttemptStorageError>;

    fn insert_retry_bounded(
        &self,
        authority: &WorkAuthority,
        write: &WorkRetryWriteV1,
        concurrency: &TopologyConcurrencyPolicyV1,
    ) -> Result<WorkRetryAttemptOutcomeV1, WorkAttemptStorageError>;
}

pub struct WorkRetryServiceV1<S, P, E> {
    storage: S,
    projections: P,
    evidence: E,
}

impl<S, P, E> WorkRetryServiceV1<S, P, E>
where
    S: WorkRetryStoragePortV1 + WorkAttemptEffectStoragePortV1,
    P: WorkProjectionReadPort,
    E: WorkRetryEvidencePortV1,
{
    pub const fn new(storage: S, projections: P, evidence: E) -> Self {
        Self {
            storage,
            projections,
            evidence,
        }
    }

    pub fn retry(
        &self,
        context: &RequestContext,
        topology: &WorkTopologyPolicyV1,
        command: RetryWorkAttemptCommandV1,
        restarted_at: UtcMicros,
    ) -> Result<WorkRetryAttemptOutcomeV1, ApplicationProblem> {
        admit(context, restarted_at)?;
        if !command.validate() {
            return Err(invalid_problem());
        }
        let authority = work_authority(context)?;
        let input_digest = canonical_sha256(&(RETRY_INPUT_DIGEST_DOMAIN, &command))
            .map_err(|_| invalid_problem())?;
        if let Some(replayed) = self
            .storage
            .retry_by_command(&authority, &command.command_id)
            .map_err(storage_problem)?
        {
            return if replayed.receipt().canonical_input_digest == input_digest {
                Ok(replayed)
            } else {
                Err(conflict_problem(
                    "application.work-retry.idempotency-conflict",
                    "The Work retry command identity was already used with different input.",
                ))
            };
        }

        let original = self
            .storage
            .load(&authority, &command.original_attempt)
            .map_err(storage_problem)?;
        self.require_retry_effect_safe(&authority, &original)?;
        let failure = self
            .evidence
            .resolve_failure(&authority, &original, &command.failure)
            .map_err(evidence_problem)?;
        validate_failure(&command, &original, &failure)?;
        if failure.observed_at.0 > restarted_at.0 {
            return Err(conflict_problem(
                "application.work-retry.failure-conflict",
                "The retry failure was observed after retry admission.",
            ));
        }
        let attempt = self.prepare_attempt(context, topology, &command, restarted_at, &original)?;
        let retry_required_at = failure.observed_at;
        let receipt = WorkRetryReceiptV1::new(
            command,
            failure,
            attempt.identity().clone(),
            retry_required_at,
            restarted_at,
        )
        .map_err(retry_receipt_problem)?;
        if receipt.canonical_input_digest != input_digest {
            return Err(invalid_problem());
        }
        self.storage
            .insert_retry_bounded(
                &authority,
                &WorkRetryWriteV1 { receipt, attempt },
                &topology.concurrency,
            )
            .map_err(storage_problem)
    }

    fn require_retry_effect_safe(
        &self,
        authority: &WorkAuthority,
        original: &WorkAttemptV1,
    ) -> Result<(), ApplicationProblem> {
        if original.execution().effect_state() != WorkEffectStateV1::CompoundNonRepeatable {
            return Ok(());
        }
        let holder = self
            .storage
            .load_effect_dispatch(authority, original.identity())
            .map_err(effect_storage_problem)?;
        if holder.as_ref().is_some_and(|holder| {
            holder.resolution() == Some(WorkAttemptEffectResolutionV1::NoEffect)
        }) {
            Ok(())
        } else {
            Err(conflict_problem(
                "application.work-retry.effect-unknown",
                "The original Work attempt has an unresolved non-repeatable effect.",
            ))
        }
    }

    fn prepare_attempt(
        &self,
        context: &RequestContext,
        topology: &WorkTopologyPolicyV1,
        command: &RetryWorkAttemptCommandV1,
        restarted_at: UtcMicros,
        original: &WorkAttemptV1,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        if original.execution().execution_snapshot().topology() != topology
            || restarted_at.0 >= original.execution().deadline().0
        {
            return Err(conflict_problem(
                "application.work-retry.admission-conflict",
                "The original Work admission no longer permits this retry.",
            ));
        }
        let snapshot = self
            .projections
            .exact_snapshot(&work_authority(context)?, original.identity().task_id())
            .map_err(projection_problem)?;
        let projection = snapshot
            .projections()
            .iter()
            .find(|projection| projection.task_id() == original.identity().task_id())
            .ok_or_else(not_found_problem)?;
        if !projection.is_execution_admitted()
            || projection.accepted_proposal()
                != Some(original.projection_binding().accepted_proposal())
        {
            return Err(conflict_problem(
                "application.work-retry.projection-conflict",
                "The Work projection no longer admits the original attempt.",
            ));
        }
        let identity = WorkAttemptIdentityV1::new(
            original.identity().task_id().clone(),
            original.identity().run_id().clone(),
            command.new_attempt_id.clone(),
        )
        .map_err(contract_problem)?;
        let binding = WorkAttemptProjectionBindingV1::new(
            original.projection_binding().graph_version(),
            original.projection_binding().event_sequence(),
            original.projection_binding().source_watermark().clone(),
            original
                .projection_binding()
                .recovered_graph_digest()
                .clone(),
            original.projection_binding().accepted_proposal().clone(),
        )
        .map_err(contract_problem)?;
        let cancellation_generation = original
            .execution()
            .cancellation_generation()
            .checked_add(1)
            .ok_or_else(invalid_problem)?;
        let envelope = WorkExecutionEnvelopeV1::new(
            identity.clone(),
            binding.clone(),
            original.execution().operation().clone(),
            original.execution().execution_snapshot().clone(),
            context.scope().project_id.clone(),
            context.scope().repository_id.clone(),
            context.scope().worktree_id.clone(),
            original.execution().worktree_root().to_owned(),
            original.execution().reference().cloned(),
            original.execution().commit().clone(),
            original.execution().instructions().to_owned(),
            cancellation_generation,
            original.execution().effect_state(),
        )
        .map_err(contract_problem)?;
        let epoch = self
            .storage
            .next_fence_epoch(&work_authority(context)?)
            .map_err(storage_problem)?;
        let lease_digest =
            canonical_sha256(&(RETRY_LEASE_DOMAIN, &identity)).map_err(|_| invalid_problem())?;
        let lease_id = WorkLeaseId::new(format!(
            "work-retry-lease:{}",
            lease_digest.as_str().trim_start_matches("sha256:")
        ))
        .map_err(|_| invalid_problem())?;
        let lease = WorkLeaseFenceV1::new(
            lease_id,
            WorkFenceEpochV1::new(epoch).map_err(contract_problem)?,
        )
        .map_err(contract_problem)?;
        WorkAttemptV1::new(
            identity,
            binding,
            envelope,
            lease,
            WorkAttemptStateV1::RecoveryRequired,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::RecoveryRequired {
                source_attempt_id: Some(original.identity().attempt_id().clone()),
                reason: command.failure.cause.restart_reason(),
            },
            original.requested_route().clone(),
            None,
            None,
        )
        .map_err(contract_problem)
    }
}

/// Public retry admission over the verified Work product graph. The legacy
/// projection reader is deliberately absent: the accepted proposal and
/// execution admission are re-read from the product graph, then the combined
/// port commits the accepted-attempt link, retry receipt, and attempt row as
/// one transaction.
pub struct WorkProductRetryServiceV1<S, E> {
    storage: S,
    evidence: E,
}

impl<S, E> WorkProductRetryServiceV1<S, E>
where
    S: WorkRetryStoragePortV1
        + WorkAttemptEffectStoragePortV1
        + WorkGraphReadPortV1
        + WorkProductOwnerAuthorizationPortV1
        + WorkProductAttemptAdmissionPortV1,
    E: WorkRetryEvidencePortV1,
{
    pub const fn new(storage: S, evidence: E) -> Self {
        Self { storage, evidence }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn retry(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        revisions: &WorkProductRevisionPinsV1,
        topology: &WorkTopologyPolicyV1,
        command: RetryWorkAttemptCommandV1,
        restarted_at: UtcMicros,
    ) -> Result<WorkRetryAttemptOutcomeV1, ApplicationProblem> {
        admit(context, restarted_at)?;
        if !command.validate() {
            return Err(invalid_problem());
        }
        let authority = work_authority(context)?;
        let input_digest = canonical_sha256(&(RETRY_INPUT_DIGEST_DOMAIN, &command))
            .map_err(|_| invalid_problem())?;
        let product_digest = canonical_sha256(&(WORK_PRODUCT_RETRY_INPUT_DIGEST_DOMAIN, &command))
            .map_err(|_| invalid_problem())?;
        let product = current_work_product_attempt_graph(
            &self.storage,
            context,
            binding,
            restarted_at,
        )?;
        if let Some(replayed) = self
            .storage
            .retry_by_command(&authority, &command.command_id)
            .map_err(storage_problem)?
        {
            if replayed.receipt().canonical_input_digest != input_digest {
                return Err(conflict_problem(
                    "application.work-retry.idempotency-conflict",
                    "The Work retry command identity was already used with different input.",
                ));
            }
            let attempt = match &replayed {
                WorkRetryAttemptOutcomeV1::Created { attempt, .. }
                | WorkRetryAttemptOutcomeV1::Replayed { attempt, .. } => attempt.clone(),
            };
            require_product_retry_admission(&product, &attempt)?;
            let draft = accepted_attempt_draft(
                &product,
                revisions,
                command.command_id.clone(),
                product_digest,
                attempt.projection_binding().graph_version(),
                attempt.identity(),
                product.context.observed_at(),
            )?;
            let admission = WorkProductRetryAdmissionV1 {
                admission: WorkProductAttemptAdmissionV1 {
                    product_context: product.context,
                    product_draft: draft,
                    authority,
                    attempt: attempt.clone(),
                    concurrency: topology.concurrency.clone(),
                },
                retry: WorkRetryWriteV1 {
                    receipt: replayed.receipt().clone(),
                    attempt,
                },
            };
            return self
                .storage
                .admit_retry(&admission)
                .map(|(_, outcome)| outcome)
                .map_err(product_admission_problem);
        }

        let original = self
            .storage
            .load(&authority, &command.original_attempt)
            .map_err(storage_problem)?;
        require_retry_effect_safe(&self.storage, &authority, &original)?;
        let failure = self
            .evidence
            .resolve_failure(&authority, &original, &command.failure)
            .map_err(evidence_problem)?;
        validate_failure(&command, &original, &failure)?;
        if failure.observed_at.0 > restarted_at.0 {
            return Err(conflict_problem(
                "application.work-retry.failure-conflict",
                "The retry failure was observed after retry admission.",
            ));
        }
        require_product_retry_admission(&product, &original)?;
        let attempt = prepare_product_retry_attempt(
            &self.storage,
            context,
            topology,
            &product,
            &command,
            restarted_at,
            &authority,
            &original,
        )?;
        let retry_required_at = failure.observed_at;
        let receipt = WorkRetryReceiptV1::new(
            command.clone(),
            failure,
            attempt.identity().clone(),
            retry_required_at,
            restarted_at,
        )
        .map_err(retry_receipt_problem)?;
        if receipt.canonical_input_digest != input_digest {
            return Err(invalid_problem());
        }
        let draft = accepted_attempt_draft(
            &product,
            revisions,
            command.command_id.clone(),
            product_digest,
            attempt.projection_binding().graph_version(),
            attempt.identity(),
            product.context.observed_at(),
        )?;
        let admission = WorkProductRetryAdmissionV1 {
            admission: WorkProductAttemptAdmissionV1 {
                product_context: product.context,
                product_draft: draft,
                authority,
                attempt: attempt.clone(),
                concurrency: topology.concurrency.clone(),
            },
            retry: WorkRetryWriteV1 { receipt, attempt },
        };
        self.storage
            .admit_retry(&admission)
            .map(|(_, outcome)| outcome)
            .map_err(product_admission_problem)
    }
}

fn require_product_retry_admission(
    product: &CurrentWorkProductAttemptGraphV1,
    attempt: &WorkAttemptV1,
) -> Result<(), ApplicationProblem> {
    let item = product
        .graph
        .item(attempt.identity().task_id())
        .ok_or_else(not_found_problem)?;
    if !item.is_execution_admitted()
        || item.accepted_proposal() != Some(attempt.projection_binding().accepted_proposal())
    {
        return Err(conflict_problem(
            "application.work-retry.product-conflict",
            "The canonical Work product graph no longer admits this retry.",
        ));
    }
    Ok(())
}

fn require_retry_effect_safe<S>(
    storage: &S,
    authority: &WorkAuthority,
    original: &WorkAttemptV1,
) -> Result<(), ApplicationProblem>
where
    S: WorkAttemptEffectStoragePortV1,
{
    if original.execution().effect_state() != WorkEffectStateV1::CompoundNonRepeatable {
        return Ok(());
    }
    let holder = storage
        .load_effect_dispatch(authority, original.identity())
        .map_err(effect_storage_problem)?;
    if holder.as_ref().is_some_and(|holder| {
        holder.resolution() == Some(WorkAttemptEffectResolutionV1::NoEffect)
    }) {
        Ok(())
    } else {
        Err(conflict_problem(
            "application.work-retry.effect-unknown",
            "The original Work attempt has an unresolved non-repeatable effect.",
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_product_retry_attempt<S>(
    storage: &S,
    context: &RequestContext,
    topology: &WorkTopologyPolicyV1,
    product: &CurrentWorkProductAttemptGraphV1,
    command: &RetryWorkAttemptCommandV1,
    restarted_at: UtcMicros,
    authority: &WorkAuthority,
    original: &WorkAttemptV1,
) -> Result<WorkAttemptV1, ApplicationProblem>
where
    S: WorkAttemptStoragePort,
{
    if original.execution().execution_snapshot().topology() != topology
        || restarted_at.0 >= original.execution().deadline().0
    {
        return Err(conflict_problem(
            "application.work-retry.admission-conflict",
            "The original Work admission no longer permits this retry.",
        ));
    }
    let identity = WorkAttemptIdentityV1::new(
        original.identity().task_id().clone(),
        original.identity().run_id().clone(),
        command.new_attempt_id.clone(),
    )
    .map_err(contract_problem)?;
    let binding = product_attempt_projection_binding(
        product,
        original.projection_binding().accepted_proposal().clone(),
    )?;
    let cancellation_generation = original
        .execution()
        .cancellation_generation()
        .checked_add(1)
        .ok_or_else(invalid_problem)?;
    let envelope = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding.clone(),
        original.execution().operation().clone(),
        original.execution().execution_snapshot().clone(),
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        original.execution().worktree_root().to_owned(),
        original.execution().reference().cloned(),
        original.execution().commit().clone(),
        original.execution().instructions().to_owned(),
        cancellation_generation,
        original.execution().effect_state(),
    )
    .map_err(contract_problem)?;
    let epoch = storage.next_fence_epoch(authority).map_err(storage_problem)?;
    let lease_digest =
        canonical_sha256(&(RETRY_LEASE_DOMAIN, &identity)).map_err(|_| invalid_problem())?;
    let lease_id = WorkLeaseId::new(format!(
        "work-retry-lease:{}",
        lease_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| invalid_problem())?;
    let lease = WorkLeaseFenceV1::new(
        lease_id,
        WorkFenceEpochV1::new(epoch).map_err(contract_problem)?,
    )
    .map_err(contract_problem)?;
    WorkAttemptV1::new(
        identity,
        binding,
        envelope,
        lease,
        WorkAttemptStateV1::RecoveryRequired,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::RecoveryRequired {
            source_attempt_id: Some(original.identity().attempt_id().clone()),
            reason: command.failure.cause.restart_reason(),
        },
        original.requested_route().clone(),
        None,
        None,
    )
    .map_err(contract_problem)
}


fn validate_failure(
    command: &RetryWorkAttemptCommandV1,
    original: &WorkAttemptV1,
    failure: &VerifiedWorkRetryFailureV1,
) -> Result<(), ApplicationProblem> {
    if failure.selector != command.failure || failure.evidence_digest.validate().is_err() {
        return Err(conflict_problem(
            "application.work-retry.failure-conflict",
            "The resolved failure does not authorize this Work retry.",
        ));
    }
    let Some(terminal) = original.terminal() else {
        return Err(conflict_problem(
            "application.work-retry.original-not-terminal",
            "A runtime retry requires terminal failure evidence.",
        ));
    };
    let (digest, observed_at, eligible) = match terminal {
        WorkTerminalEvidenceV1::Failed {
            evidence_digest,
            observed_at,
        }
        | WorkTerminalEvidenceV1::TimedOut {
            evidence_digest,
            observed_at,
        } => (evidence_digest, observed_at, true),
        WorkTerminalEvidenceV1::Succeeded {
            evidence_digest,
            observed_at,
        }
        | WorkTerminalEvidenceV1::Cancelled {
            evidence_digest,
            observed_at,
        } => (evidence_digest, observed_at, false),
    };
    if !eligible || digest != &failure.evidence_digest || observed_at != &failure.observed_at {
        return Err(conflict_problem(
            "application.work-retry.runtime-evidence-conflict",
            "The runtime failure no longer matches the original terminal receipt.",
        ));
    }
    Ok(())
}

fn admit(context: &RequestContext, observed_at: UtcMicros) -> Result<(), ApplicationProblem> {
    match context.admission_at(observed_at) {
        RequestAdmission::Admitted => Ok(()),
        RequestAdmission::Cancelled => Err(ApplicationProblem::cancelled_before_admission()),
        RequestAdmission::TimedOut => Err(ApplicationProblem::timed_out_before_admission()),
    }
}

fn invalid_problem() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "application.work-retry.invalid".to_owned(),
            message: "The Work retry command is invalid.".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}

fn retry_receipt_problem(_error: ApplicationContractError) -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "application.work-retry.receipt-unavailable".to_owned(),
        message: "The Work retry receipt could not be sealed.".to_owned(),
    })
}

fn conflict_problem(code: &str, message: &str) -> ApplicationProblem {
    ApplicationProblem::Conflict {
        diagnostic: SafeDiagnostic {
            code: code.to_owned(),
            message: message.to_owned(),
        },
        retry: RetryDirective::AfterRevalidate,
        legal_actions: vec![LegalAction::Refresh],
    }
}

fn not_found_problem() -> ApplicationProblem {
    ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
}

fn evidence_problem(error: WorkRetryEvidenceErrorV1) -> ApplicationProblem {
    match error {
        WorkRetryEvidenceErrorV1::NotFoundOrNotAuthorized => not_found_problem(),
        WorkRetryEvidenceErrorV1::Conflict => conflict_problem(
            "application.work-retry.failure-conflict",
            "The Work retry failure evidence changed.",
        ),
        WorkRetryEvidenceErrorV1::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-retry.evidence-unavailable".to_owned(),
            message: "The Work retry failure evidence authority is unavailable.".to_owned(),
        }),
    }
}

fn storage_problem(error: WorkAttemptStorageError) -> ApplicationProblem {
    match error {
        WorkAttemptStorageError::NotFoundOrNotAuthorized => not_found_problem(),
        WorkAttemptStorageError::CapacityExceeded => conflict_problem(
            "application.work-retry.capacity-exhausted",
            "Work retry capacity is exhausted.",
        ),
        WorkAttemptStorageError::ReservationFenced => conflict_problem(
            "application.work-retry.reservation-fenced",
            "The Work run does not currently admit a retry reservation.",
        ),
        WorkAttemptStorageError::AttemptConflict
        | WorkAttemptStorageError::RunAdmissionConflict
        | WorkAttemptStorageError::FenceConflict => conflict_problem(
            "application.work-retry.conflict",
            "The Work retry authority changed.",
        ),
        WorkAttemptStorageError::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-retry.unavailable".to_owned(),
            message: "The Work retry authority is unavailable.".to_owned(),
        }),
    }
}

fn effect_storage_problem(error: WorkAttemptEffectStorageErrorV1) -> ApplicationProblem {
    match error {
        WorkAttemptEffectStorageErrorV1::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        WorkAttemptEffectStorageErrorV1::Conflict => conflict_problem(
            "application.work-retry.effect-conflict",
            "The original Work attempt effect receipt changed.",
        ),
        WorkAttemptEffectStorageErrorV1::Unavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.work-retry.effect-unavailable".to_owned(),
                message: "The Work attempt effect authority is unavailable.".to_owned(),
            })
        }
    }
}

fn projection_problem(error: WorkProjectionPortError) -> ApplicationProblem {
    match error {
        WorkProjectionPortError::NotFoundOrNotAuthorized => not_found_problem(),
        WorkProjectionPortError::StaleCursor => conflict_problem(
            "application.work-retry.projection-stale",
            "The Work projection changed before retry admission.",
        ),
        WorkProjectionPortError::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-retry.projection-unavailable".to_owned(),
            message: "The Work projection authority is unavailable.".to_owned(),
        }),
    }
}

fn contract_problem(_error: WorkRuntimeContractError) -> ApplicationProblem {
    invalid_problem()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{RunId, TaskId};

    fn identity(attempt: &str) -> WorkAttemptIdentityV1 {
        WorkAttemptIdentityV1::new(
            TaskId::new("task.retry".to_owned()).expect("task id"),
            RunId::new("run.retry".to_owned()).expect("run id"),
            AttemptId::new(attempt.to_owned()).expect("attempt id"),
        )
        .expect("attempt identity")
    }

    fn valid_receipt() -> WorkRetryReceiptV1 {
        let evidence_digest = canonical_sha256(&("runtime-retry-evidence", 1_u8)).expect("digest");
        let command = RetryWorkAttemptCommandV1 {
            original_attempt: identity("attempt.original"),
            new_attempt_id: AttemptId::new("attempt.retry".to_owned()).expect("attempt id"),
            failure: WorkRetryFailureSelectorV1 {
                source: WorkRetrySourceV1::Runtime,
                cause: WorkRetryCauseV1::RuntimeFailure,
                evidence_ref: format!("runtime-terminal:{}", evidence_digest.as_str()),
            },
            command_id: WorkCommandId::new("command.retry".to_owned()).expect("command id"),
        };
        let failure = VerifiedWorkRetryFailureV1 {
            selector: command.failure.clone(),
            evidence_digest,
            observed_at: UtcMicros(19),
        };
        WorkRetryReceiptV1::new(
            command,
            failure,
            identity("attempt.retry"),
            UtcMicros(19),
            UtcMicros(21),
        )
        .expect("retry receipt")
    }

    #[test]
    fn observation_validation_requires_exact_new_attempt_lineage() {
        let mut receipt = valid_receipt();
        assert!(receipt.validate_for_observation());

        receipt.new_attempt = receipt.command.original_attempt.clone();
        assert!(!receipt.validate_for_observation());
    }

    #[test]
    fn retry_failure_wire_refuses_nonruntime_source() {
        let decoded = serde_json::from_str::<WorkRetryFailureSelectorV1>(
            r#"{"source":"test","cause":"test_failure","evidence_ref":"test:failure"}"#,
        );
        assert!(decoded.is_err());
    }

    #[test]
    fn observation_validation_rejects_backdated_failure_and_changed_selector() {
        let mut receipt = valid_receipt();
        receipt.failure.observed_at = UtcMicros(22);
        assert!(!receipt.validate_for_observation());

        let mut receipt = valid_receipt();
        receipt.command.failure.evidence_ref = "runtime-terminal:other".to_owned();
        assert!(!receipt.validate_for_observation());
    }

    #[test]
    fn terminal_failure_retry_uses_truthful_recovery_reason() {
        assert_eq!(
            WorkRetryCauseV1::RuntimeFailure.restart_reason(),
            WorkRestartReasonV1::FailureObserved,
        );
    }
}
