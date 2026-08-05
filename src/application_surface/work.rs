//! HTTP adapter for the canonical Work application authority.

use std::sync::Arc;

use axum::response::Response;
use tracedecay_api::WorkOperation;
use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, AttachRuntimeEvidenceCommand,
    CreateWorkCommand, ExpandWorkEvidenceRequestV1, GenerateWorkProposalRequestV1,
    ReplanDependenciesCommand, ReviewProposalRequestV1, WorkAttemptAcquireLeaseRequestV1,
    WorkAttemptCancelRequestV1, WorkAttemptFinishRequestV1, WorkAttemptPublishArtifactRequestV1,
    WorkAttemptPublishProgressRequestV1, WorkAttemptRecoverRequestV1,
    WorkAttemptRenewLeaseRequestV1, WorkAttemptResponseV1, WorkAttemptStartRequestV1,
    WorkAttemptTerminalizeRequestV1, WorkEvidenceExpansionV1, WorkProductMutationReceiptV1,
    WorkProductMutationRequestV1, WorkProductProjectionReadV1, WorkProductProjectionsRequestV1,
    WorkProductSnapshotRequestV1, WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1,
    WorkTaskEvidenceRequestV1, WorkTopologyReadV1,
};
use tracedecay_domain::{
    WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1, WorkProposalV1,
    WorkTaskEvidenceV1,
};
use tracedecay_tool_catalog::RouteExposureV1;

use crate::daemon_client::DaemonInvocationExecutor;
use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationRequest, WorkApplicationInvocationV1,
    WorkApplicationOutcomeV1, WorkAttemptInvocationV1,
};

use super::{ApplicationSurfaceAdapterError, invoke_registered_http};

pub(super) fn router_with_executor(
    executor: Arc<dyn DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_catalog_bindings()?;
    Ok(tracedecay_api::work_application_router(WorkExecutorOwner {
        executor,
    }))
}

/// Refuse to mount Work unless the catalog advertises every descriptor
/// operation at exactly the path this build answers on.
pub(crate) fn validate_catalog_bindings() -> Result<(), ApplicationSurfaceAdapterError> {
    let registry = tracedecay_application::work_executable_binding_registry()
        .map_err(ApplicationSurfaceAdapterError::CatalogValidation)?;
    for operation in WorkOperation::ALL {
        let operation_id = tracedecay_tool_catalog::OperationId::new(operation.operation_id())
            .map_err(ApplicationSurfaceAdapterError::Identifier)?;
        let Some(binding) = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
        else {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        };
        let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        };
        if route_path != operation.application_route_path() {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        }
    }
    Ok(())
}

/// The single dispatch owner behind core, product, and attempt Work routes.
#[derive(Clone)]
pub(crate) struct WorkExecutorOwner {
    pub(crate) executor: Arc<dyn DaemonInvocationExecutor>,
}

impl tracedecay_api::WorkApplicationOwner for WorkExecutorOwner {
    fn invoke_work(
        &self,
        request: tracedecay_api::WorkHttpRequest,
    ) -> tracedecay_api::WorkInvocationFuture {
        Box::pin(invoke_work_operation(Arc::clone(&self.executor), request))
    }
}

async fn invoke_work_operation(
    executor: Arc<dyn DaemonInvocationExecutor>,
    request: tracedecay_api::WorkHttpRequest,
) -> Response {
    let tracedecay_api::WorkHttpRequest {
        operation,
        request_id,
        controls,
        body,
    } = request;

    macro_rules! application {
        ($request_ty:ty, $variant:ident, $output:ty) => {{
            let Ok(decoded) = serde_json::from_value::<$request_ty>(body) else {
                return tracedecay_api::work_invalid_request_response(request_id);
            };
            let invocation = DaemonInvocationRequest::work_application(
                request_id.as_str(),
                WorkApplicationInvocationV1::$variant(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<$output, _>(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    DaemonInvocationOutcome::WorkApplication {
                        scope,
                        outcome: WorkApplicationOutcomeV1::$variant(outcome),
                    } => Some((scope, outcome)),
                    _ => None,
                },
            )
            .await
        }};
    }

    macro_rules! attempt {
        ($request_ty:ty, $variant:ident) => {{
            let Ok(decoded) = serde_json::from_value::<$request_ty>(body) else {
                return tracedecay_api::work_invalid_request_response(request_id);
            };
            let invocation = DaemonInvocationRequest::work_attempt(
                request_id.as_str(),
                WorkAttemptInvocationV1::$variant(decoded.into()),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<WorkAttemptResponseV1, _>(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    DaemonInvocationOutcome::WorkAttempt { scope, outcome } => {
                        Some((scope, *outcome))
                    }
                    _ => None,
                },
            )
            .await
        }};
    }

    match operation {
        WorkOperation::Snapshot => application!(
            WorkProjectionSnapshotRequestV1,
            Snapshot,
            WorkProjectionSnapshotV1
        ),
        WorkOperation::Delta => {
            application!(WorkProjectionDeltaRequestV1, Delta, WorkProjectionDeltaV1)
        }
        WorkOperation::Create => application!(CreateWorkCommand, Create, WorkProjection),
        WorkOperation::ReplanDependencies => application!(
            ReplanDependenciesCommand,
            ReplanDependencies,
            WorkProjection
        ),
        WorkOperation::ReviewProposal => {
            application!(ReviewProposalRequestV1, ReviewProposal, WorkProjection)
        }
        WorkOperation::AcceptProposal => {
            application!(AcceptProposalCommand, AcceptProposal, WorkProjection)
        }
        WorkOperation::AdmitExecution => {
            application!(AdmitExecutionCommand, AdmitExecution, WorkProjection)
        }
        WorkOperation::AttachRuntimeEvidence => application!(
            AttachRuntimeEvidenceCommand,
            AttachRuntimeEvidence,
            WorkProjection
        ),
        WorkOperation::AcceptTask => application!(AcceptTaskCommand, AcceptTask, WorkProjection),
        WorkOperation::ProductSnapshot => application!(
            WorkProductSnapshotRequestV1,
            ProductSnapshot,
            WorkTopologyReadV1
        ),
        WorkOperation::ProductProjections => application!(
            WorkProductProjectionsRequestV1,
            ProductProjections,
            WorkProductProjectionReadV1
        ),
        WorkOperation::TaskEvidence => {
            application!(WorkTaskEvidenceRequestV1, TaskEvidence, WorkTaskEvidenceV1)
        }
        WorkOperation::ExpandTaskEvidence => application!(
            ExpandWorkEvidenceRequestV1,
            ExpandTaskEvidence,
            WorkEvidenceExpansionV1
        ),
        WorkOperation::GenerateWorkProposal => application!(
            GenerateWorkProposalRequestV1,
            GenerateWorkProposal,
            WorkProposalV1
        ),
        WorkOperation::ApplyWorkCommand => application!(
            WorkProductMutationRequestV1,
            ApplyWorkCommand,
            WorkProductMutationReceiptV1
        ),
        WorkOperation::AttemptAcquireLease => {
            attempt!(WorkAttemptAcquireLeaseRequestV1, AcquireLease)
        }
        WorkOperation::AttemptRenewLease => attempt!(WorkAttemptRenewLeaseRequestV1, RenewLease),
        WorkOperation::AttemptStart => attempt!(WorkAttemptStartRequestV1, Start),
        WorkOperation::AttemptPublishProgress => {
            attempt!(WorkAttemptPublishProgressRequestV1, PublishProgress)
        }
        WorkOperation::AttemptPublishArtifact => {
            attempt!(WorkAttemptPublishArtifactRequestV1, PublishArtifact)
        }
        WorkOperation::AttemptCancel => attempt!(WorkAttemptCancelRequestV1, Cancel),
        WorkOperation::AttemptRecover => attempt!(WorkAttemptRecoverRequestV1, Recover),
        WorkOperation::AttemptFinish => attempt!(WorkAttemptFinishRequestV1, Finish),
        WorkOperation::AttemptTerminalize => attempt!(WorkAttemptTerminalizeRequestV1, Terminalize),
    }
}
