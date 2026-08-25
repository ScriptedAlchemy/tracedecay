//! HTTP and MCP ownership for the canonical Work surface.
//!
//! Both transports enter this module after normalizing their request controls.
//! Work request decoding, daemon invocation construction, catalog binding
//! lookup, and typed response envelopes therefore have one owner.

use std::sync::Arc;

use axum::response::Response;
use tracedecay_api::{WorkHttpRequest, WorkOperation};
use tracedecay_application::{
    AdjudicateWorkLeakCommandV1, AdmitWorkExecutionRequestV1, AdmitWorkPlacementCommand,
    AdmitWorkSynthesisCommand, CancelWorkAttemptCommand, CreateWorkTaskRequestV1,
    DecideWorkProposalRequestV1, ExecutionTopologyMetricsRequestV1, ExecutionTopologyMetricsV1,
    ExecutionTopologyViewV1, GenerateProposalRequest, GeneratedWorkProposal, PauseWorkRunCommand,
    PrepareWorkDuplicateAdjudicationRequestV1, PrepareWorkProductMutationRequestV1,
    ReleaseWorkPlacementCommand, ResumeWorkAttemptsCommand, ResumeWorkRunCommand,
    RetryWorkAttemptCommandV1, StartWorkAttemptCommand, WorkArtifactHydrationRequestV1,
    WorkArtifactHydrationV1, WorkAttemptListRequestV1, WorkAttemptListV1,
    WorkAttemptRecoveryReportV1, WorkAttemptStatusRequestV1,
    WorkDuplicateAdjudicationAppendOutcomeV1, WorkEvidenceRetrievalV1,
    WorkEvidenceRetrieveRequestV1, WorkExecutionHistoryV1, WorkExperienceRequestV1,
    WorkExperienceV1, WorkGraphReadRequestV1, WorkGraphReadV1, WorkLeakAdjudicationOutcomeV1,
    WorkPlacementPreflightRequestV1, WorkPlacementReadingV1, WorkPlacementStatusRequestV1,
    WorkProductMutationReceiptV1, WorkProductMutationRequestV1, WorkProposalComparisonRequestV1,
    WorkProposalComparisonV1, WorkRunControlReadingV1, WorkRunControlRequestV1,
    WorkSynthesisAttemptV1, WorkTopologyViewRequestV1,
};
use tracedecay_domain::{
    WorkAttemptV1, WorkDuplicateAdjudicationCommandV1, WorkPlacementPreflightV1, WorkPlacementV1,
    WorkRunControlV1,
};
use tracedecay_tool_catalog::RouteExposureV1;

use super::{ApplicationSurfaceAdapterError, invoke_registered_http};
use crate::daemon_client::DaemonInvocationExecutor;
use crate::daemon_contract::{WorkApplicationInvocationV1, WorkApplicationOutcomeV1};

pub(super) fn router_with_executor(
    executor: Arc<dyn DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_catalog_bindings()?;
    Ok(tracedecay_api::work_application_router(WorkExecutorOwner {
        executor,
    }))
}

pub(super) fn dashboard_router_with_executor(
    executor: Arc<dyn DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_catalog_bindings()?;
    Ok(tracedecay_api::work_dashboard_router(WorkExecutorOwner {
        executor,
    }))
}

/// Refuse to mount Work unless the executable catalog advertises every
/// canonical descriptor operation at the application path this build serves.
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

#[derive(Clone)]
struct WorkExecutorOwner {
    executor: Arc<dyn DaemonInvocationExecutor>,
}

impl tracedecay_api::WorkApplicationOwner for WorkExecutorOwner {
    fn invoke_work(&self, request: WorkHttpRequest) -> tracedecay_api::WorkInvocationFuture {
        let executor = Arc::clone(&self.executor);
        Box::pin(async move { invoke_work_operation(Some(executor.as_ref()), request).await })
    }
}

/// Invoke the typed Work owner shared by the HTTP router and MCP adapter.
///
/// A missing executor remains a canonical Work runtime-unavailable response;
/// it never becomes a transport-specific MCP error.
pub(crate) async fn invoke_work_operation(
    executor: Option<&dyn DaemonInvocationExecutor>,
    request: WorkHttpRequest,
) -> Response {
    let WorkHttpRequest {
        operation,
        request_id,
        controls,
        body,
    } = request;

    macro_rules! core {
        // Same as the plain arm, for outcome variants whose payload is boxed
        // to keep the wire outcome enum compact.
        (boxed $request_ty:ty, $variant:ident, $output:ty) => {{
            let Ok(decoded) = serde_json::from_value::<$request_ty>(body) else {
                return tracedecay_api::work_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::work_application(
                request_id.as_str(),
                WorkApplicationInvocationV1::$variant(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            let Some(executor) = executor else {
                return super::registered_executor_unavailable::<$output, _>(operation, request_id);
            };
            invoke_registered_http::<$output, _>(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::WorkApplication {
                        scope,
                        outcome: WorkApplicationOutcomeV1::$variant(outcome),
                    } => Some((scope, *outcome)),
                    _ => None,
                },
            )
            .await
        }};
        ($request_ty:ty, $variant:ident, $output:ty) => {{
            let Ok(decoded) = serde_json::from_value::<$request_ty>(body) else {
                return tracedecay_api::work_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::work_application(
                request_id.as_str(),
                WorkApplicationInvocationV1::$variant(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            let Some(executor) = executor else {
                return super::registered_executor_unavailable::<$output, _>(operation, request_id);
            };
            invoke_registered_http::<$output, _>(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::WorkApplication {
                        scope,
                        outcome: WorkApplicationOutcomeV1::$variant(outcome),
                    } => Some((scope, outcome)),
                    _ => None,
                },
            )
            .await
        }};
    }

    match operation {
        WorkOperation::GenerateProposal => core!(
            GenerateProposalRequest,
            GenerateProposal,
            GeneratedWorkProposal
        ),
        WorkOperation::Create => core!(
            CreateWorkTaskRequestV1,
            Create,
            WorkProductMutationReceiptV1
        ),
        WorkOperation::ReviewProposal => {
            core!(
                DecideWorkProposalRequestV1,
                ReviewProposal,
                WorkProductMutationReceiptV1
            )
        }
        WorkOperation::AcceptProposal => {
            core!(
                DecideWorkProposalRequestV1,
                AcceptProposal,
                WorkProductMutationReceiptV1
            )
        }
        WorkOperation::AdmitExecution => {
            core!(
                AdmitWorkExecutionRequestV1,
                AdmitExecution,
                WorkProductMutationReceiptV1
            )
        }
        WorkOperation::StartAttempt => {
            core!(StartWorkAttemptCommand, StartAttempt, WorkAttemptV1)
        }
        WorkOperation::Synthesize => {
            core!(
                AdmitWorkSynthesisCommand,
                Synthesize,
                WorkSynthesisAttemptV1
            )
        }
        WorkOperation::AttemptStatus => {
            core!(WorkAttemptStatusRequestV1, AttemptStatus, WorkAttemptV1)
        }
        WorkOperation::CancelAttempt => {
            core!(CancelWorkAttemptCommand, CancelAttempt, WorkAttemptV1)
        }
        WorkOperation::ResumeAttempts => {
            core!(
                ResumeWorkAttemptsCommand,
                ResumeAttempts,
                WorkAttemptRecoveryReportV1
            )
        }
        WorkOperation::RetryAttempt => core!(
            boxed RetryWorkAttemptCommandV1,
            RetryAttempt,
            tracedecay_application::WorkRetryAttemptOutcomeV1
        ),
        WorkOperation::ListAttempts => {
            core!(WorkAttemptListRequestV1, ListAttempts, WorkAttemptListV1)
        }
        WorkOperation::ExecutionHistory => core!(
            WorkAttemptListRequestV1,
            ExecutionHistory,
            WorkExecutionHistoryV1
        ),
        WorkOperation::HydrateArtifacts => {
            core!(
                WorkArtifactHydrationRequestV1,
                HydrateArtifacts,
                WorkArtifactHydrationV1
            )
        }
        WorkOperation::RetrieveEvidence => core!(
            WorkEvidenceRetrieveRequestV1,
            RetrieveEvidence,
            WorkEvidenceRetrievalV1
        ),
        WorkOperation::Views => core!(WorkGraphReadRequestV1, Views, WorkGraphReadV1),
        WorkOperation::Experience => core!(WorkExperienceRequestV1, Experience, WorkExperienceV1),
        WorkOperation::CompareProposal => core!(
            WorkProposalComparisonRequestV1,
            CompareProposal,
            WorkProposalComparisonV1
        ),
        WorkOperation::PrepareGraphMutation => core!(
            PrepareWorkProductMutationRequestV1,
            PrepareGraphMutation,
            WorkProductMutationRequestV1
        ),
        WorkOperation::MutateGraph => core!(
            WorkProductMutationRequestV1,
            MutateGraph,
            WorkProductMutationReceiptV1
        ),
        WorkOperation::Topology => {
            core!(WorkTopologyViewRequestV1, Topology, ExecutionTopologyViewV1)
        }
        WorkOperation::TopologyMetrics => core!(
            ExecutionTopologyMetricsRequestV1,
            TopologyMetrics,
            ExecutionTopologyMetricsV1
        ),
        WorkOperation::PrepareDuplicateAdjudication => core!(
            PrepareWorkDuplicateAdjudicationRequestV1,
            PrepareDuplicateAdjudication,
            WorkDuplicateAdjudicationCommandV1
        ),
        WorkOperation::AdjudicateDuplicate => core!(
            WorkDuplicateAdjudicationCommandV1,
            AdjudicateDuplicate,
            WorkDuplicateAdjudicationAppendOutcomeV1
        ),
        WorkOperation::AdjudicateLeak => core!(
            AdjudicateWorkLeakCommandV1,
            AdjudicateLeak,
            WorkLeakAdjudicationOutcomeV1
        ),
        WorkOperation::PauseRun => core!(PauseWorkRunCommand, PauseRun, WorkRunControlV1),
        WorkOperation::ResumeRun => core!(ResumeWorkRunCommand, ResumeRun, WorkRunControlV1),
        WorkOperation::RunControl => {
            core!(WorkRunControlRequestV1, RunControl, WorkRunControlReadingV1)
        }
        WorkOperation::PlacementPreflight => core!(
            WorkPlacementPreflightRequestV1,
            PlacementPreflight,
            WorkPlacementPreflightV1
        ),
        WorkOperation::AdmitPlacement => {
            core!(AdmitWorkPlacementCommand, AdmitPlacement, WorkPlacementV1)
        }
        WorkOperation::PlacementStatus => {
            core!(
                WorkPlacementStatusRequestV1,
                PlacementStatus,
                WorkPlacementReadingV1
            )
        }
        WorkOperation::ReleasePlacement => {
            core!(
                ReleaseWorkPlacementCommand,
                ReleasePlacement,
                WorkPlacementV1
            )
        }
    }
}
