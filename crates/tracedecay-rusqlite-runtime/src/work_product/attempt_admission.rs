//! One SQLite transaction for a graph-declared Work attempt and its runtime row.
//!
//! A product graph is profile-owned while an attempt row is exact-Work-authority
//! owned. The combined command carries the exact authority for write admission;
//! graph reads hydrate an accepted identity only when one canonical attempt row
//! exists for it, so no second binding journal or table is needed.

use tracedecay_application::{
    WorkAttemptInsertOutcome, WorkAttemptStorageError, WorkProductAttemptAdmissionErrorV1,
    WorkProductAttemptAdmissionOutcomeV1, WorkProductAttemptAdmissionPortV1,
    WorkProductAttemptAdmissionV1, WorkProductEventCommitOutcomeV1, WorkProductEventCommitV1,
    WorkProductEventPortErrorV1, WorkProductRetryAdmissionV1, WorkProductSynthesisAdmissionV1,
    WorkRetryAttemptOutcomeV1, WorkSynthesisInsertOutcome,
};
use tracedecay_domain::{WorkProductAuthorizedRelationScopeV1, WorkProductGraphV1};

use super::{fold_graph, load_journal};
use crate::exact_sql::ExactSqlTransaction;
use crate::work::WorkSqliteStorage;

type AdmissionError = WorkProductAttemptAdmissionErrorV1;

impl WorkProductAttemptAdmissionPortV1 for WorkSqliteStorage {
    fn admit_attempt(
        &self,
        admission: &WorkProductAttemptAdmissionV1,
    ) -> Result<WorkProductAttemptAdmissionOutcomeV1, AdmissionError> {
        admission.validate()?;
        require_declared_authority(admission)?;
        require_request_active(&admission.product_context)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| AdmissionError::Unavailable)?;
        let outcome = admit_attempt_in_transaction(&transaction, admission);
        match outcome {
            Ok(WorkProductAttemptAdmissionOutcomeV1::Inserted { product, attempt }) => {
                transaction
                    .commit()
                    .map_err(|_| AdmissionError::DurabilityUncertain)?;
                Ok(WorkProductAttemptAdmissionOutcomeV1::Inserted { product, attempt })
            }
            Ok(WorkProductAttemptAdmissionOutcomeV1::Replayed { product, attempt }) => {
                transaction
                    .rollback()
                    .map_err(|_| AdmissionError::DurabilityUncertain)?;
                Ok(WorkProductAttemptAdmissionOutcomeV1::Replayed { product, attempt })
            }
            Err(error) => rollback_after_failure(transaction, error),
        }
    }

    fn admit_retry(
        &self,
        admission: &WorkProductRetryAdmissionV1,
    ) -> Result<(WorkProductEventCommitV1, WorkRetryAttemptOutcomeV1), AdmissionError> {
        admission.validate()?;
        require_declared_authority(&admission.admission)?;
        require_request_active(&admission.admission.product_context)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| AdmissionError::Unavailable)?;
        let outcome = admit_retry_in_transaction(&transaction, admission);
        match outcome {
            Ok((product, WorkRetryAttemptOutcomeV1::Created { receipt, attempt })) => {
                transaction
                    .commit()
                    .map_err(|_| AdmissionError::DurabilityUncertain)?;
                Ok((
                    product,
                    WorkRetryAttemptOutcomeV1::Created { receipt, attempt },
                ))
            }
            Ok((product, WorkRetryAttemptOutcomeV1::Replayed { receipt, attempt })) => {
                transaction
                    .rollback()
                    .map_err(|_| AdmissionError::DurabilityUncertain)?;
                Ok((
                    product,
                    WorkRetryAttemptOutcomeV1::Replayed { receipt, attempt },
                ))
            }
            Err(error) => rollback_after_failure(transaction, error),
        }
    }

    fn admit_synthesis(
        &self,
        admission: &WorkProductSynthesisAdmissionV1,
    ) -> Result<(WorkProductEventCommitV1, WorkSynthesisInsertOutcome), AdmissionError> {
        admission.validate()?;
        require_declared_authority(&admission.admission)?;
        require_request_active(&admission.admission.product_context)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| AdmissionError::Unavailable)?;
        let outcome = admit_synthesis_in_transaction(&transaction, admission);
        match outcome {
            Ok((product, WorkSynthesisInsertOutcome::Inserted)) => {
                transaction
                    .commit()
                    .map_err(|_| AdmissionError::DurabilityUncertain)?;
                Ok((product, WorkSynthesisInsertOutcome::Inserted))
            }
            Ok((product, WorkSynthesisInsertOutcome::Replayed(result))) => {
                transaction
                    .rollback()
                    .map_err(|_| AdmissionError::DurabilityUncertain)?;
                Ok((product, WorkSynthesisInsertOutcome::Replayed(result)))
            }
            Err(error) => rollback_after_failure(transaction, error),
        }
    }
}

fn admit_attempt_in_transaction(
    transaction: &ExactSqlTransaction,
    admission: &WorkProductAttemptAdmissionV1,
) -> Result<WorkProductAttemptAdmissionOutcomeV1, AdmissionError> {
    let product = super::events::append_in_transaction(
        transaction,
        &admission.product_context,
        &admission.product_draft,
    )
    .map_err(map_product_error)?;
    let graph = graph_for_product(
        transaction,
        &admission.product_context,
        product_commit(&product),
    )?;
    admission
        .attempt
        .validate_graph_admission(&graph)
        .map_err(map_graph_admission_error)?;
    let attempt = crate::work_attempt::insert_attempt_in_transaction(
        transaction,
        &admission.authority,
        &admission.attempt,
        Some(&admission.concurrency),
    )
    .map_err(map_attempt_error)?;
    match (product, attempt) {
        (
            WorkProductEventCommitOutcomeV1::Appended(product),
            WorkAttemptInsertOutcome::Inserted,
        ) => Ok(WorkProductAttemptAdmissionOutcomeV1::Inserted {
            product,
            attempt: admission.attempt.clone(),
        }),
        (
            WorkProductEventCommitOutcomeV1::Replayed(product),
            WorkAttemptInsertOutcome::Replayed(attempt),
        ) => Ok(WorkProductAttemptAdmissionOutcomeV1::Replayed {
            product,
            attempt: *attempt,
        }),
        _ => Err(AdmissionError::IdentityConflict),
    }
}

fn admit_retry_in_transaction(
    transaction: &ExactSqlTransaction,
    admission: &WorkProductRetryAdmissionV1,
) -> Result<(WorkProductEventCommitV1, WorkRetryAttemptOutcomeV1), AdmissionError> {
    let product = super::events::append_in_transaction(
        transaction,
        &admission.admission.product_context,
        &admission.admission.product_draft,
    )
    .map_err(map_product_error)?;
    let graph = graph_for_product(
        transaction,
        &admission.admission.product_context,
        product_commit(&product),
    )?;
    admission
        .admission
        .attempt
        .validate_graph_admission(&graph)
        .map_err(map_graph_admission_error)?;
    let retry = crate::work::insert_retry_bounded_in_transaction(
        transaction,
        &admission.admission.authority,
        &admission.retry,
        &admission.admission.concurrency,
    )
    .map_err(map_attempt_error)?;
    match (product, retry) {
        (
            WorkProductEventCommitOutcomeV1::Appended(product),
            retry @ WorkRetryAttemptOutcomeV1::Created { .. },
        ) => Ok((product, retry)),
        (
            WorkProductEventCommitOutcomeV1::Replayed(product),
            retry @ WorkRetryAttemptOutcomeV1::Replayed { .. },
        ) => Ok((product, retry)),
        _ => Err(AdmissionError::IdentityConflict),
    }
}

fn admit_synthesis_in_transaction(
    transaction: &ExactSqlTransaction,
    admission: &WorkProductSynthesisAdmissionV1,
) -> Result<(WorkProductEventCommitV1, WorkSynthesisInsertOutcome), AdmissionError> {
    let product = super::events::append_in_transaction(
        transaction,
        &admission.admission.product_context,
        &admission.admission.product_draft,
    )
    .map_err(map_product_error)?;
    let graph = graph_for_product(
        transaction,
        &admission.admission.product_context,
        product_commit(&product),
    )?;
    admission
        .synthesis
        .result
        .attempt
        .validate_graph_admission(&graph)
        .map_err(map_graph_admission_error)?;
    let synthesis = crate::work_attempt::insert_synthesis_in_transaction(
        transaction,
        &admission.admission.authority,
        &admission.synthesis,
        Some(&admission.admission.concurrency),
    )
    .map_err(map_attempt_error)?;
    match (product, synthesis) {
        (
            WorkProductEventCommitOutcomeV1::Appended(product),
            WorkSynthesisInsertOutcome::Inserted,
        ) => Ok((product, WorkSynthesisInsertOutcome::Inserted)),
        (
            WorkProductEventCommitOutcomeV1::Replayed(product),
            synthesis @ WorkSynthesisInsertOutcome::Replayed(_),
        ) => Ok((product, synthesis)),
        _ => Err(AdmissionError::IdentityConflict),
    }
}

fn product_commit(outcome: &WorkProductEventCommitOutcomeV1) -> &WorkProductEventCommitV1 {
    match outcome {
        WorkProductEventCommitOutcomeV1::Appended(commit)
        | WorkProductEventCommitOutcomeV1::Replayed(commit) => commit,
    }
}

fn graph_for_product(
    transaction: &ExactSqlTransaction,
    context: &tracedecay_application::WorkProductPortContextV1,
    product: &WorkProductEventCommitV1,
) -> Result<WorkProductGraphV1, AdmissionError> {
    let journal =
        load_journal(transaction, context.authorized_scope()).ok_or(AdmissionError::Unavailable)?;
    let graph =
        fold_graph(&journal, product.event().sequence()).ok_or(AdmissionError::Unavailable)?;
    if graph.version() != product.verified_graph_version().graph_version() {
        return Err(AdmissionError::VersionConflict);
    }
    Ok(graph)
}

fn require_declared_authority(
    admission: &WorkProductAttemptAdmissionV1,
) -> Result<(), AdmissionError> {
    let expected_project = WorkProductAuthorizedRelationScopeV1::Project {
        project_id: admission.authority.project_id().clone(),
    };
    let expected_repository = WorkProductAuthorizedRelationScopeV1::Repository {
        project_id: admission.authority.project_id().clone(),
        repository_id: admission.authority.repository_id().clone(),
    };
    if admission
        .product_draft
        .authorized_relation_scopes
        .iter()
        .any(|scope| scope == &expected_project || scope == &expected_repository)
    {
        Ok(())
    } else {
        Err(AdmissionError::InvalidAdmission)
    }
}

fn map_product_error(error: WorkProductEventPortErrorV1) -> AdmissionError {
    match error {
        WorkProductEventPortErrorV1::NotFoundOrNotAuthorized => {
            AdmissionError::NotFoundOrNotAuthorized
        }
        WorkProductEventPortErrorV1::VersionConflict => AdmissionError::VersionConflict,
        WorkProductEventPortErrorV1::IdempotencyConflict => AdmissionError::IdempotencyConflict,
        WorkProductEventPortErrorV1::Unavailable => AdmissionError::Unavailable,
        WorkProductEventPortErrorV1::Cancelled => AdmissionError::Cancelled,
        WorkProductEventPortErrorV1::TimedOut => AdmissionError::TimedOut,
    }
}

fn map_attempt_error(error: WorkAttemptStorageError) -> AdmissionError {
    match error {
        WorkAttemptStorageError::NotFoundOrNotAuthorized => AdmissionError::NotFoundOrNotAuthorized,
        WorkAttemptStorageError::CapacityExceeded => AdmissionError::CapacityExceeded,
        WorkAttemptStorageError::AttemptConflict
        | WorkAttemptStorageError::RunAdmissionConflict
        | WorkAttemptStorageError::ReservationFenced
        | WorkAttemptStorageError::FenceConflict => AdmissionError::IdentityConflict,
        WorkAttemptStorageError::Unavailable => AdmissionError::Unavailable,
    }
}

fn map_graph_admission_error(error: tracedecay_domain::WorkRuntimeContractError) -> AdmissionError {
    match error {
        tracedecay_domain::WorkRuntimeContractError::ProjectionMismatch => {
            AdmissionError::VersionConflict
        }
        tracedecay_domain::WorkRuntimeContractError::InvalidFenceEpoch
        | tracedecay_domain::WorkRuntimeContractError::InvalidProgress
        | tracedecay_domain::WorkRuntimeContractError::InvalidArtifact
        | tracedecay_domain::WorkRuntimeContractError::TooManyArtifacts
        | tracedecay_domain::WorkRuntimeContractError::DuplicateArtifact
        | tracedecay_domain::WorkRuntimeContractError::InvalidCancellationOrder
        | tracedecay_domain::WorkRuntimeContractError::InconsistentAttemptState
        | tracedecay_domain::WorkRuntimeContractError::InvalidAttemptTransition
        | tracedecay_domain::WorkRuntimeContractError::MixedAttemptIdentity
        | tracedecay_domain::WorkRuntimeContractError::StaleLeaseFence
        | tracedecay_domain::WorkRuntimeContractError::SelfRecovery
        | tracedecay_domain::WorkRuntimeContractError::ExecutionNotAdmitted
        | tracedecay_domain::WorkRuntimeContractError::InvalidExecutionEnvelope
        | tracedecay_domain::WorkRuntimeContractError::InvalidExecutionSnapshot => {
            AdmissionError::InvalidAdmission
        }
    }
}

fn require_request_active(
    context: &tracedecay_application::WorkProductPortContextV1,
) -> Result<(), AdmissionError> {
    if context.cancellation().is_cancelled() {
        return Err(AdmissionError::Cancelled);
    }
    if context.deadline().is_elapsed_at(context.observed_at()) {
        return Err(AdmissionError::TimedOut);
    }
    Ok(())
}

fn rollback_after_failure<T>(
    transaction: ExactSqlTransaction,
    error: AdmissionError,
) -> Result<T, AdmissionError> {
    transaction
        .rollback()
        .map_err(|_| AdmissionError::DurabilityUncertain)?;
    Err(error)
}
