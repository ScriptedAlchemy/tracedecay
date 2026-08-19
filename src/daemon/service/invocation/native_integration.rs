//! Plan 36 native-integration daemon invocation handler.
//!
//! This is the single transport entry point for `stack_snapshot`,
//! `preflight_native_integration`, `apply_native_integration`,
//! `native_integration_status`, and `cancel_native_integration`. It contains
//! no Git mechanics: selection resolution, preflight, apply, status,
//! cancellation, journaling, and recovery all live behind the application
//! `NativeIntegrationPort` / `NativeIntegrationStackResolutionPort`, composed
//! per project by the daemon native-integration registry at project open.
//!
//! A project without a mounted owner — a non-Git project, or a request
//! arriving before project-open admission finished — answers with the typed
//! `authority_unmounted` result rather than a guess, a partial apply, or a
//! local mutation fallback. Plan 36 slice 4 requires exactly this: "An
//! unavailable daemon or capability leaves the operation explicitly
//! preview-only or unavailable; no transport falls back to local mutation."
//!
//! Apply resolves its preview and one-use approval from the durable store by
//! exact identity and digest; a missing or mismatched fact is denied without
//! disclosing whether the target was absent or denied. Until the
//! owner-decided approval-issuance operation lands (see the dated record in
//! `docs/plans/tracedecay-v2/36-git-aware-change-context-and-index-transactions.md`),
//! no production surface mints an approval, so every apply truthfully denies.

use super::*;

use tracedecay_application::NATIVE_INTEGRATION_APPLY_OPERATION;
use tracedecay_application::git::NativeIntegrationApprovalProjectionV1;
use tracedecay_application::git::{
    NativeWorktreeSurfaceRequest, NativeWorktreeSurfaceResultV1, WorktreeCleanupReconciliationV1,
    WorktreeCleanupRemovalV1, WorktreeConfirmationOutcomeV1, WorktreeContractError,
    WorktreeInspectionOutcomeV1, WorktreeInventoryOutcomeV1,
};
use tracedecay_application::{
    CancellationSignal, CancellationState, NativeIntegrationApplyRequestV1,
    NativeIntegrationCancelRequestV1, NativeIntegrationContractError, NativeIntegrationPortError,
    NativeIntegrationPreflightOutcomeV1, NativeIntegrationPreflightRequestV1,
    NativeIntegrationReceiptProjectionV1, NativeIntegrationStatusProjectionV1,
    NativeIntegrationStatusRequestV1, NativeIntegrationSurfaceResultV1,
    NativeIntegrationSurfaceUnavailableV1, native_integration_surface_operation,
};
use tracedecay_domain::{
    NativeIntegrationApprovalId, NativeIntegrationApprovalV1,
    NativeIntegrationPreviewDispositionV1, NativeIntegrationPreviewId,
};
use tracedecay_store::NativeIntegrationStore;
use tracedecay_usecases::native_integration::NativeIntegrationStatusBroadcastV1;
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, WorkConflictObservationResultV1,
    WorkConflictObservationUnavailableV1, record_native_integration_transition,
    record_work_conflict_observation,
};
use tracedecay_usecases::stack_coordinator::StackCoordinatorErrorV1;

use crate::application_surface::NativeIntegrationSurfaceRequest;
use crate::daemon::native_integration::DaemonNativeIntegrationOwner;
use crate::daemon::native_integration::stack_signals::{
    signal_from_preflight, signal_from_receipt,
};

/// How long a minted preview stays approvable. The preview must outlive its
/// own request so the separate approval and apply operations can bind it, and
/// the expiry is re-checked by the apply validator and the authorization port
/// rather than trusted from storage.
const NATIVE_INTEGRATION_PREVIEW_TTL_MICROS: i64 = 15 * 60 * 1_000_000;

/// Executes one native-integration surface request.
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_native_integration(
    wire_request_id: String,
    registered: Option<RegisteredConfigurationRuntime>,
    owner: Option<DaemonNativeIntegrationOwner>,
    observability_producer: Option<Arc<BoundedObservabilityProducerV1>>,
    status_broadcast: Option<Arc<NativeIntegrationStatusBroadcastV1>>,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: NativeIntegrationSurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    // A missing project route must stay indistinguishable from a denied one:
    // Plan 36 forbids leaking absence-versus-denial for a named target.
    let Some(registered) = registered else {
        return concealed_application_problem(wire_request_id);
    };
    if request.operation() != surface_operation {
        return application_problem(wire_request_id, invalid_native_integration_request());
    }

    let (context, authority) = match native_integration_authority(
        &wire_request_id,
        &registered,
        surface_operation,
        observed_at,
        deadline.clone(),
        cancellation.clone(),
    ) {
        Ok(bound) => bound,
        Err(problem) => return application_problem(wire_request_id, problem),
    };

    let request = match request {
        NativeIntegrationSurfaceRequest::Worktree(request) => {
            let result = match owner {
                Some(owner) => {
                    execute_worktree_with_owner(owner, request, &cancellation, observed_at).await
                }
                None => Ok(worktree_unavailable(
                    request,
                    WorktreeUnavailableReasonV1::Unavailable,
                )),
            };
            let result = match result {
                Ok(result) => result,
                Err(problem) => return application_problem(wire_request_id, problem),
            };
            let Ok(payload) = serde_json::to_value(result) else {
                return DaemonInvocationResponse::problem(
                    wire_request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            };
            return match native_integration_evidence(payload, authority, observed_at, deadline) {
                Ok(outcome) => DaemonInvocationResponse::with_outcome(
                    wire_request_id,
                    DaemonInvocationOutcome::NativeIntegration {
                        scope: registered.scope,
                        outcome,
                    },
                ),
                Err(problem) => application_problem(wire_request_id, problem),
            };
        }
        request => request,
    };

    let owner_mounted = owner.is_some();
    let execution = match owner {
        // No native-integration runtime authority is mounted for this
        // project. The result is read-only and truthful; it advances nothing
        // and authorizes nothing.
        None => NativeIntegrationExecutionV1::without_preview(
            NativeIntegrationSurfaceResultV1::unavailable(
                NativeIntegrationSurfaceUnavailableV1::AuthorityUnmounted,
            ),
        ),
        Some(owner) => {
            let signal = match live_cancellation_signal(&cancellation, observed_at) {
                Ok(signal) => signal,
                Err(problem) => return application_problem(wire_request_id, problem),
            };
            let executed = execute_with_owner(
                &wire_request_id,
                owner,
                context,
                request,
                observed_at,
                signal,
                status_broadcast,
            )
            .await;
            match executed {
                Ok(execution) => execution,
                Err(problem) => return application_problem(wire_request_id, problem),
            }
        }
    };
    let _ = record_native_integration_transition(
        registered.scope.project_id.as_str(),
        observability_producer.as_deref(),
        surface_operation.as_str(),
        owner_mounted,
        &execution.result,
        execution.owner_preview.as_ref(),
    );
    // Telemetry only: the preflight disposition and terminal apply receipt
    // additionally prove one mechanical conflict prediction/outcome pair.
    match record_work_conflict_observation(
        registered.scope.project_id.as_str(),
        observability_producer.as_deref(),
        surface_operation.as_str(),
        owner_mounted,
        &execution.result,
        execution.owner_preview.as_ref(),
    ) {
        WorkConflictObservationResultV1::Enqueued { .. }
        | WorkConflictObservationResultV1::Unavailable {
            reason: WorkConflictObservationUnavailableV1::NotAdjudicated,
        } => {}
        refused => {
            tracing::debug!(
                outcome = ?refused,
                operation = surface_operation.as_str(),
                "work-conflict observation was not recorded"
            );
        }
    }

    let Ok(payload) = serde_json::to_value(&execution.result) else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };

    match native_integration_evidence(payload, authority, observed_at, deadline) {
        Ok(outcome) => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::NativeIntegration {
                scope: registered.scope,
                outcome,
            },
        ),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

struct NativeIntegrationExecutionV1 {
    result: NativeIntegrationSurfaceResultV1,
    owner_preview: Option<tracedecay_domain::NativeIntegrationPreviewV1>,
}

impl NativeIntegrationExecutionV1 {
    const fn without_preview(result: NativeIntegrationSurfaceResultV1) -> Self {
        Self {
            result,
            owner_preview: None,
        }
    }

    const fn with_preview(
        result: NativeIntegrationSurfaceResultV1,
        owner_preview: tracedecay_domain::NativeIntegrationPreviewV1,
    ) -> Self {
        Self {
            result,
            owner_preview: Some(owner_preview),
        }
    }
}

/// Publishes one observed transaction status to the project's read-only
/// notification fan-out. Delivery is best-effort observation: a missing
/// broadcast changes nothing about the operation result.
fn publish_transaction_status(
    broadcast: Option<&Arc<NativeIntegrationStatusBroadcastV1>>,
    status: &tracedecay_domain::NativeIntegrationTransactionStatusV1,
) {
    if let Some(broadcast) = broadcast {
        broadcast.publish(NativeIntegrationStatusProjectionV1::from(status));
    }
}

/// Reads and publishes the durable status a mutation just advanced, from the
/// same blocking context that ran the mutation.
fn publish_current_transaction_status(
    broadcast: Option<&Arc<NativeIntegrationStatusBroadcastV1>>,
    owner: &DaemonNativeIntegrationOwner,
    transaction_id: &tracedecay_domain::NativeIntegrationTransactionId,
) {
    if broadcast.is_none() {
        return;
    }
    if let Ok(Some(status)) = owner.service().status(NativeIntegrationStatusRequestV1 {
        transaction_id: transaction_id.clone(),
    }) {
        publish_transaction_status(broadcast, &status);
    }
}

/// Runs one operation against the mounted per-project owner.
///
/// The kernel and its store bridge are synchronous (native Git plus a bounded
/// store actor), so every owner call crosses to a blocking thread; the
/// coordinator's own cancellation map keeps a running apply cancellable
/// through the separate cancel operation.
#[allow(clippy::too_many_arguments)]
async fn execute_with_owner(
    wire_request_id: &str,
    owner: DaemonNativeIntegrationOwner,
    context: RequestContext,
    request: NativeIntegrationSurfaceRequest,
    observed_at: UtcMicros,
    signal: CancellationSignal,
    status_broadcast: Option<Arc<NativeIntegrationStatusBroadcastV1>>,
) -> Result<NativeIntegrationExecutionV1, ApplicationProblem> {
    let invalid = invalid_native_integration_request;
    match request {
        NativeIntegrationSurfaceRequest::StackSnapshot(snapshot) => {
            let outcome = tokio::task::spawn_blocking(move || {
                let resolution = registered_topology_request(&owner, *snapshot, observed_at)?;
                owner.stack_snapshot(resolution, &signal)
            })
            .await
            .map_err(|_| unavailable_native_integration())?;
            match outcome {
                Ok(outcome) => NativeIntegrationSurfaceResultV1::from_stack_resolution(&outcome)
                    .map(NativeIntegrationExecutionV1::without_preview)
                    .map_err(|_| invalid()),
                Err(error) => surface_result_from_contract_error(error)
                    .map(NativeIntegrationExecutionV1::without_preview),
            }
        }
        NativeIntegrationSurfaceRequest::Preflight(preflight) => {
            let preview_id = NativeIntegrationPreviewId::new(format!(
                "preview.native-integration.{wire_request_id}"
            ))
            .map_err(|_| invalid())?;
            let preview_expires_at = UtcMicros(
                observed_at
                    .0
                    .saturating_add(NATIVE_INTEGRATION_PREVIEW_TTL_MICROS),
            );
            let signal_scope = context.scope().clone();
            let signal_context = context.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                let stack_runtime = owner.github_stack_runtime(context.scope())?;
                let topology =
                    registered_topology_request(&owner, preflight.snapshot, observed_at)?;
                let application_request = NativeIntegrationPreflightRequestV1 {
                    context,
                    topology,
                    evidence: preflight.evidence.into(),
                    preview_id,
                    preferred_mode: preflight.preferred_mode,
                    preview_expires_at,
                    observed_at,
                };
                let outcome = match stack_runtime.as_ref() {
                    Some(runtime) => runtime
                        .preflight(&application_request, &signal)
                        .map_err(stack_coordinator_contract_error),
                    None => owner.service().preflight(application_request, &signal),
                };
                if let (Some(runtime), Ok(NativeIntegrationPreflightOutcomeV1::Preview(preview))) =
                    (stack_runtime.as_ref(), &outcome)
                    && let Some(stack_signal) = signal_from_preflight(&signal_scope, preview)
                        .map_err(stack_coordinator_contract_error)?
                {
                    runtime
                        .enqueue_from_preflight(stack_signal, &signal_context)
                        .map_err(stack_coordinator_contract_error)?;
                }
                outcome
            })
            .await
            .map_err(|_| unavailable_native_integration())?;
            match outcome {
                Ok(outcome) => {
                    let owner_preview = match &outcome {
                        NativeIntegrationPreflightOutcomeV1::Preview(preview) => {
                            Some((**preview).clone())
                        }
                        _ => None,
                    };
                    NativeIntegrationSurfaceResultV1::from_preflight(&outcome)
                        .map(|result| NativeIntegrationExecutionV1 {
                            result,
                            owner_preview,
                        })
                        .map_err(|_| invalid())
                }
                Err(error) => surface_result_from_contract_error(error)
                    .map(NativeIntegrationExecutionV1::without_preview),
            }
        }
        NativeIntegrationSurfaceRequest::Approve(approve) => {
            // The owner-decided sixth operation: mint and durably record one
            // one-use approval bound to the exact preview content, the
            // requesting principal, the apply capability, and the current
            // grant lineage. Nothing here is caller-choosable beyond the
            // preview identity/digest pair.
            let approval_id = NativeIntegrationApprovalId::new(format!(
                "approval.native-integration.{wire_request_id}"
            ))
            .map_err(|_| invalid())?;
            let apply_operation =
                native_integration_surface_operation(NATIVE_INTEGRATION_APPLY_OPERATION)
                    .map_err(|_| invalid())?
                    .ok_or_else(invalid)?;
            let capability = tracedecay_domain::CapabilityId::new(
                apply_operation.capability_id().as_str().to_owned(),
            )
            .map_err(|_| invalid())?;
            tokio::task::spawn_blocking(move || {
                let store = owner.store();
                let preview = match store.read_preview(&approve.preview_id) {
                    Ok(Some(preview)) if preview.preview_digest == approve.preview_digest => {
                        preview
                    }
                    Ok(_) => {
                        return Ok(NativeIntegrationSurfaceResultV1::unavailable(
                            NativeIntegrationSurfaceUnavailableV1::Denied,
                        ));
                    }
                    Err(_) => return Err(unavailable_native_integration()),
                };
                if preview.expires_at.0 <= observed_at.0 {
                    return Ok(NativeIntegrationSurfaceResultV1::unavailable(
                        NativeIntegrationSurfaceUnavailableV1::Stale,
                    ));
                }
                // A preview frozen under a superseded grant lineage is stale
                // evidence; approving it would silently re-authorize it.
                if preview.grant_digest != context.grant().digest {
                    return Ok(NativeIntegrationSurfaceResultV1::unavailable(
                        NativeIntegrationSurfaceUnavailableV1::Stale,
                    ));
                }
                // Only a mechanically eligible preview is approvable; the
                // apply validator enforces the same predicate and issuance
                // must not manufacture approvals apply will reject.
                if !matches!(
                    preview.disposition,
                    NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_)
                ) {
                    return Ok(NativeIntegrationSurfaceResultV1::unavailable(
                        NativeIntegrationSurfaceUnavailableV1::Denied,
                    ));
                }
                let pending_digest =
                    tracedecay_domain::canonical_sha256(&"pending native integration approval")
                        .map_err(|_| invalid_native_integration_request())?;
                let approval = NativeIntegrationApprovalV1 {
                    approval_id,
                    preview_id: preview.preview_id.clone(),
                    preview_digest: preview.preview_digest.clone(),
                    principal: context.actor().clone(),
                    delegated_agent: None,
                    capability,
                    grant_digest: context.grant().digest.clone(),
                    issued_at: observed_at,
                    expires_at: preview.expires_at,
                    approval_digest: pending_digest,
                }
                .seal()
                .map_err(|_| invalid_native_integration_request())?;
                match store.save_approval(approval.clone()) {
                    Ok(()) => Ok(NativeIntegrationSurfaceResultV1::Approval(
                        NativeIntegrationApprovalProjectionV1::project(&approval),
                    )),
                    Err(tracedecay_store::NativeIntegrationStoreError::ApprovalConflict) => {
                        Ok(NativeIntegrationSurfaceResultV1::unavailable(
                            NativeIntegrationSurfaceUnavailableV1::ApprovalConflict,
                        ))
                    }
                    Err(_) => Err(unavailable_native_integration()),
                }
            })
            .await
            .map_err(|_| unavailable_native_integration())?
            .map(NativeIntegrationExecutionV1::without_preview)
        }
        NativeIntegrationSurfaceRequest::Apply(apply) => {
            let stack_runtime = owner
                .github_stack_runtime(context.scope())
                .map_err(|_| unavailable_native_integration())?;
            let signal_scope = context.scope().clone();
            let signal_context = context.clone();
            tokio::task::spawn_blocking(move || {
                // The caller names its preview and one-use approval by exact
                // identity and digest; both must already be durable. A
                // missing or mismatched fact is denied without disclosing
                // whether the target was absent or denied.
                let store = owner.store();
                let preview = match store.read_preview(&apply.preview_id) {
                    Ok(Some(preview)) if preview.preview_digest == apply.preview_digest => preview,
                    Ok(_) => {
                        return Ok(NativeIntegrationExecutionV1::without_preview(
                            NativeIntegrationSurfaceResultV1::unavailable(
                                NativeIntegrationSurfaceUnavailableV1::Denied,
                            ),
                        ));
                    }
                    Err(_) => return Err(unavailable_native_integration()),
                };
                let approval = match store.read_approval(&apply.approval_id) {
                    Ok(Some(approval)) if approval.approval_digest == apply.approval_digest => {
                        approval
                    }
                    Ok(_) => {
                        return Ok(NativeIntegrationExecutionV1::without_preview(
                            NativeIntegrationSurfaceResultV1::unavailable(
                                NativeIntegrationSurfaceUnavailableV1::Denied,
                            ),
                        ));
                    }
                    Err(_) => return Err(unavailable_native_integration()),
                };
                let signal_preview = preview.clone();
                let signal_approval = approval.clone();
                let applied_transaction_id = apply.transaction_id.clone();
                let application_request = NativeIntegrationApplyRequestV1 {
                    context,
                    transaction_id: apply.transaction_id,
                    preview,
                    approval,
                    observed_at,
                };
                let apply_outcome = owner.service().apply(application_request, &signal);
                publish_current_transaction_status(
                    status_broadcast.as_ref(),
                    &owner,
                    &applied_transaction_id,
                );
                match apply_outcome {
                    Ok(receipt) => {
                        if let Some(runtime) = stack_runtime.as_ref()
                            && let Some(stack_signal) =
                                signal_from_receipt(&signal_scope, &signal_preview, &receipt)
                                    .map_err(|_| unavailable_native_integration())?
                        {
                            runtime
                                .enqueue_from_approval(
                                    stack_signal,
                                    &signal_approval,
                                    &signal_context,
                                )
                                .map_err(|_| unavailable_native_integration())?;
                        }
                        NativeIntegrationReceiptProjectionV1::project(&receipt)
                            .map(NativeIntegrationSurfaceResultV1::Receipt)
                            .map(|result| {
                                NativeIntegrationExecutionV1::with_preview(result, signal_preview)
                            })
                            .map_err(|_| invalid_native_integration_request())
                    }
                    Err(error) => surface_result_from_contract_error(error)
                        .map(NativeIntegrationExecutionV1::without_preview),
                }
            })
            .await
            .map_err(|_| unavailable_native_integration())?
        }
        NativeIntegrationSurfaceRequest::Status(status) => {
            let application_request = NativeIntegrationStatusRequestV1 {
                transaction_id: status.transaction_id,
            };
            let outcome =
                tokio::task::spawn_blocking(move || owner.service().status(application_request))
                    .await
                    .map_err(|_| unavailable_native_integration())?;
            match outcome {
                Ok(Some(status)) => {
                    publish_transaction_status(status_broadcast.as_ref(), &status);
                    Ok(NativeIntegrationExecutionV1::without_preview(
                        NativeIntegrationSurfaceResultV1::Status(
                            NativeIntegrationStatusProjectionV1::from(&status),
                        ),
                    ))
                }
                Ok(None) => Ok(NativeIntegrationExecutionV1::without_preview(
                    NativeIntegrationSurfaceResultV1::unavailable(
                        NativeIntegrationSurfaceUnavailableV1::UnknownTransaction,
                    ),
                )),
                Err(error) => surface_result_from_contract_error(error)
                    .map(NativeIntegrationExecutionV1::without_preview),
            }
        }
        NativeIntegrationSurfaceRequest::Cancel(cancel) => {
            let cancelled_transaction_id = cancel.transaction_id.clone();
            let application_request = NativeIntegrationCancelRequestV1 {
                transaction_id: cancel.transaction_id,
                requested_at: observed_at,
            };
            let outcome = tokio::task::spawn_blocking(move || {
                let disposition = owner.service().cancel(application_request);
                publish_current_transaction_status(
                    status_broadcast.as_ref(),
                    &owner,
                    &cancelled_transaction_id,
                );
                disposition
            })
            .await
            .map_err(|_| unavailable_native_integration())?;
            match outcome {
                Ok(disposition) => Ok(NativeIntegrationExecutionV1::without_preview(
                    NativeIntegrationSurfaceResultV1::from_cancel(disposition),
                )),
                Err(error) => surface_result_from_contract_error(error)
                    .map(NativeIntegrationExecutionV1::without_preview),
            }
        }
        NativeIntegrationSurfaceRequest::Worktree(_) => Err(invalid()),
    }
}

#[derive(Clone, Copy)]
enum WorktreeOperationV1 {
    Inventory,
    Inspection,
    Confirmation,
    Removal,
    Reconciliation,
}

#[derive(Clone, Copy)]
enum WorktreeUnavailableReasonV1 {
    Stale,
    Denied,
    DurabilityUncertain,
    Unavailable,
}

async fn execute_worktree_with_owner(
    owner: DaemonNativeIntegrationOwner,
    request: NativeWorktreeSurfaceRequest,
    cancellation: &CancellationContext,
    observed_at: UtcMicros,
) -> Result<NativeWorktreeSurfaceResultV1, ApplicationProblem> {
    let operation = match &request {
        NativeWorktreeSurfaceRequest::Inventory(_) => WorktreeOperationV1::Inventory,
        NativeWorktreeSurfaceRequest::Inspect(_) => WorktreeOperationV1::Inspection,
        NativeWorktreeSurfaceRequest::Confirm(_) => WorktreeOperationV1::Confirmation,
        NativeWorktreeSurfaceRequest::Remove(_) => WorktreeOperationV1::Removal,
        NativeWorktreeSurfaceRequest::Reconcile(_) => WorktreeOperationV1::Reconciliation,
    };
    let signal = live_cancellation_signal(cancellation, observed_at)?;
    let Some(service) = owner.worktree_service_arc() else {
        return Ok(worktree_unavailable(
            request,
            WorktreeUnavailableReasonV1::Unavailable,
        ));
    };
    tokio::task::spawn_blocking(move || {
        let outcome = match request {
            NativeWorktreeSurfaceRequest::Inventory(request) => service
                .inventory(&request, &signal)
                .map(NativeWorktreeSurfaceResultV1::Inventory),
            NativeWorktreeSurfaceRequest::Inspect(request) => service
                .inspect(&request, &signal)
                .map(NativeWorktreeSurfaceResultV1::Inspection),
            NativeWorktreeSurfaceRequest::Confirm(request) => service
                .confirm(&request, &signal)
                .map(NativeWorktreeSurfaceResultV1::Confirmation),
            NativeWorktreeSurfaceRequest::Remove(request) => service
                .remove(&request, &signal)
                .map(NativeWorktreeSurfaceResultV1::Removal),
            NativeWorktreeSurfaceRequest::Reconcile(request) => service
                .reconcile(&request, &signal)
                .map(NativeWorktreeSurfaceResultV1::Reconciliation),
        };
        outcome.or_else(|error| worktree_result_from_error(operation, error))
    })
    .await
    .map_err(|_| unavailable_native_integration())?
}

fn worktree_result_from_error(
    operation: WorktreeOperationV1,
    error: WorktreeContractError,
) -> Result<NativeWorktreeSurfaceResultV1, ApplicationProblem> {
    let reason = match error {
        WorktreeContractError::Domain(_) | WorktreeContractError::Inconsistent { .. } => {
            return Err(invalid_native_integration_request());
        }
        WorktreeContractError::ScopeSetDenied | WorktreeContractError::Denied => {
            WorktreeUnavailableReasonV1::Denied
        }
        WorktreeContractError::Stale => WorktreeUnavailableReasonV1::Stale,
        WorktreeContractError::DurabilityUncertain => {
            WorktreeUnavailableReasonV1::DurabilityUncertain
        }
        WorktreeContractError::ScopeSetUnavailable
        | WorktreeContractError::AuthorityUnavailable
        | WorktreeContractError::Native(_) => WorktreeUnavailableReasonV1::Unavailable,
    };
    Ok(worktree_unavailable_for_operation(operation, reason))
}

fn worktree_unavailable(
    request: NativeWorktreeSurfaceRequest,
    reason: WorktreeUnavailableReasonV1,
) -> NativeWorktreeSurfaceResultV1 {
    let operation = match request {
        NativeWorktreeSurfaceRequest::Inventory(_) => WorktreeOperationV1::Inventory,
        NativeWorktreeSurfaceRequest::Inspect(_) => WorktreeOperationV1::Inspection,
        NativeWorktreeSurfaceRequest::Confirm(_) => WorktreeOperationV1::Confirmation,
        NativeWorktreeSurfaceRequest::Remove(_) => WorktreeOperationV1::Removal,
        NativeWorktreeSurfaceRequest::Reconcile(_) => WorktreeOperationV1::Reconciliation,
    };
    worktree_unavailable_for_operation(operation, reason)
}

fn worktree_unavailable_for_operation(
    operation: WorktreeOperationV1,
    reason: WorktreeUnavailableReasonV1,
) -> NativeWorktreeSurfaceResultV1 {
    match operation {
        WorktreeOperationV1::Inventory => NativeWorktreeSurfaceResultV1::Inventory(match reason {
            WorktreeUnavailableReasonV1::Stale => WorktreeInventoryOutcomeV1::Stale,
            WorktreeUnavailableReasonV1::Denied => WorktreeInventoryOutcomeV1::Denied,
            WorktreeUnavailableReasonV1::DurabilityUncertain
            | WorktreeUnavailableReasonV1::Unavailable => WorktreeInventoryOutcomeV1::Unavailable,
        }),
        WorktreeOperationV1::Inspection => {
            NativeWorktreeSurfaceResultV1::Inspection(match reason {
                WorktreeUnavailableReasonV1::Stale => WorktreeInspectionOutcomeV1::Stale,
                WorktreeUnavailableReasonV1::Denied => WorktreeInspectionOutcomeV1::Denied,
                WorktreeUnavailableReasonV1::DurabilityUncertain
                | WorktreeUnavailableReasonV1::Unavailable => {
                    WorktreeInspectionOutcomeV1::Unavailable
                }
            })
        }
        WorktreeOperationV1::Confirmation => {
            NativeWorktreeSurfaceResultV1::Confirmation(match reason {
                WorktreeUnavailableReasonV1::Stale => WorktreeConfirmationOutcomeV1::Stale,
                WorktreeUnavailableReasonV1::Denied => WorktreeConfirmationOutcomeV1::Denied,
                WorktreeUnavailableReasonV1::DurabilityUncertain
                | WorktreeUnavailableReasonV1::Unavailable => {
                    WorktreeConfirmationOutcomeV1::Unavailable
                }
            })
        }
        WorktreeOperationV1::Removal => NativeWorktreeSurfaceResultV1::Removal(match reason {
            WorktreeUnavailableReasonV1::Stale => WorktreeCleanupRemovalV1::Stale,
            WorktreeUnavailableReasonV1::Denied => WorktreeCleanupRemovalV1::Denied,
            WorktreeUnavailableReasonV1::DurabilityUncertain => {
                WorktreeCleanupRemovalV1::DurabilityUncertain
            }
            WorktreeUnavailableReasonV1::Unavailable => WorktreeCleanupRemovalV1::Unavailable,
        }),
        WorktreeOperationV1::Reconciliation => {
            NativeWorktreeSurfaceResultV1::Reconciliation(match reason {
                WorktreeUnavailableReasonV1::Stale => WorktreeCleanupReconciliationV1::Stale,
                WorktreeUnavailableReasonV1::Denied => WorktreeCleanupReconciliationV1::Denied,
                WorktreeUnavailableReasonV1::DurabilityUncertain => {
                    WorktreeCleanupReconciliationV1::DurabilityUncertain
                }
                WorktreeUnavailableReasonV1::Unavailable => {
                    WorktreeCleanupReconciliationV1::Unavailable
                }
            })
        }
    }
}

fn registered_topology_request(
    owner: &DaemonNativeIntegrationOwner,
    snapshot: tracedecay_application::NativeIntegrationStackSnapshotSurfaceRequest,
    observed_at: UtcMicros,
) -> Result<
    tracedecay_application::NativeIntegrationStackResolutionRequestV1,
    NativeIntegrationPortError,
> {
    let scope_set = owner.authorized_scope_set(
        &snapshot.authorized_scope_set_id,
        snapshot.authorized_scope_set_revision,
        &snapshot.authorized_scope_set_digest,
    )?;
    snapshot
        .into_resolution_request(scope_set, observed_at)
        .map_err(|_| NativeIntegrationPortError::Stale)
}

/// Contract violations are the caller's invalid request; port failures map to
/// the typed unavailable reason the wire contract declares for them.
fn surface_result_from_contract_error(
    error: NativeIntegrationContractError,
) -> Result<NativeIntegrationSurfaceResultV1, ApplicationProblem> {
    match error {
        NativeIntegrationContractError::Contract(_) => Err(invalid_native_integration_request()),
        NativeIntegrationContractError::Port(error) => {
            Ok(NativeIntegrationSurfaceResultV1::unavailable(
                NativeIntegrationSurfaceUnavailableV1::from(&error),
            ))
        }
    }
}

/// A live process-local cancellation signal carrying the caller's transport
/// cancellation identity and any already-observed cancellation.
pub(super) fn live_cancellation_signal(
    cancellation: &CancellationContext,
    observed_at: UtcMicros,
) -> Result<CancellationSignal, ApplicationProblem> {
    let signal = CancellationSignal::active(cancellation.token_id.as_str())
        .map_err(|_| invalid_native_integration_request())?;
    if let CancellationState::Cancelled { requested_at } = cancellation.state {
        let requested_at = if requested_at.0 == 0 {
            observed_at
        } else {
            requested_at
        };
        signal.cancel(requested_at);
    }
    Ok(signal)
}

fn unavailable_native_integration() -> ApplicationProblem {
    ApplicationProblem::Unavailable {
        classification: tracedecay_application::ApplicationUnavailableClassV1::Authority,
        diagnostic: SafeDiagnostic {
            code: "native_integration_runtime_unavailable".to_owned(),
            message: "The native-integration runtime did not complete the request".to_owned(),
        },
        retry: RetryDirective::AfterDelay,
        legal_actions: vec![tracedecay_application::LegalAction::Retry],
    }
}

fn stack_coordinator_contract_error(
    error: StackCoordinatorErrorV1,
) -> NativeIntegrationContractError {
    let port = match error {
        StackCoordinatorErrorV1::Cancelled => NativeIntegrationPortError::Cancelled,
        StackCoordinatorErrorV1::Stale => NativeIntegrationPortError::Stale,
        StackCoordinatorErrorV1::Denied => NativeIntegrationPortError::Denied,
        StackCoordinatorErrorV1::Unavailable | StackCoordinatorErrorV1::Saturated => {
            NativeIntegrationPortError::Unavailable
        }
        StackCoordinatorErrorV1::Invalid(message) => NativeIntegrationPortError::Native(message),
    };
    NativeIntegrationContractError::Port(port)
}

/// The typed problem for a native-integration request that does not satisfy
/// its operation contract, or whose bounded authority receipt cannot be
/// minted from the values the request supplied.
fn invalid_native_integration_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "invalid_native_integration_request".to_owned(),
            message: "The native-integration request does not match its operation contract"
                .to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
    }
}

/// Mint the request context and authority for exactly one native-integration
/// capability.
///
/// Stack resolution, preflight, apply, status, and cancellation are separate
/// capabilities, so the grant names exactly the one operation being invoked.
/// A preflight grant can never satisfy an apply request.
fn native_integration_authority(
    request_id: &str,
    registered: &RegisteredConfigurationRuntime,
    operation: crate::application_surface::ApplicationSurfaceOperation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<(RequestContext, AuthorityReceipt), ApplicationProblem> {
    if observed_at >= registered.grants.expires_at {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let invalid = invalid_native_integration_request;
    let application_operation = native_integration_surface_operation(operation.as_str())
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    let expires_at = UtcMicros(deadline.expires_at.0.min(registered.grants.expires_at.0));
    // The grant digest is the grant *lineage* identity, deliberately stable
    // per (scope, policy revision) rather than per request or per operation:
    // the authorization port and the apply validator bind previews and
    // approvals to "the grant now in hand" by this digest across the
    // stack-snapshot -> preflight -> approval -> apply journey. Per-operation
    // enforcement stays in the grant's capability/use-case set below — a
    // preflight grant still can never satisfy apply.
    let grant_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.daemon.native-integration-route-grant.v1",
        &registered.scope,
        registered.grants.policy_digest.as_str(),
        registered.grants.policy_epoch,
    ))
    .map_err(|_| invalid())?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.daemon.native-integration.{request_id}"))
            .map_err(|_| invalid())?,
        1,
        grant_digest,
        ActorId::new("actor.tracedecay-daemon").map_err(|_| invalid())?,
        observed_at,
        expires_at,
        registered.scope.clone(),
        std::collections::BTreeSet::from([application_operation.capability_id().clone()]),
        std::collections::BTreeSet::from([application_operation.use_case_id().clone()]),
        DisclosureClass::Sensitive,
    )
    .map_err(|_| invalid())?;
    let context = RequestContext::new(
        registered.actor.clone(),
        registered.scope.clone(),
        grant,
        RequestId::new(request_id).map_err(|_| invalid())?,
        deadline,
        cancellation,
    )
    .map_err(|_| invalid())?;
    let policy_digest = ManifestDigest::new(registered.grants.policy_digest.as_str().to_owned())
        .map_err(|_| invalid())?;
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.daemon.native-integration.v1",
            registered.grants.policy_epoch,
            policy_digest,
            ComponentVersion::new("tracedecay.daemon.native-integration-policy.v1")
                .map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?,
        observed_at,
    )
    .map_err(|_| invalid())?;
    Ok((context, authority))
}

fn native_integration_evidence(
    payload: serde_json::Value,
    authority: AuthorityReceipt,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<serde_json::Value>, ApplicationProblem> {
    let invalid = invalid_native_integration_request;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(|_| invalid())?;
    Ok(ApplicationOutcome::Evidence(EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)
            .map_err(|_| invalid())?,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.native-integration.stable.v1").map_err(|_| invalid())?,
            1,
            Some(1),
            1,
        )
        .map_err(|_| invalid())?,
        execution,
        payload: Some(payload),
    }))
}
