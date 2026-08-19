//! Canonical public HTTP adapter for daemon-owned Workflow execution.

use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde_json::Value;
use tracedecay_application::{
    ApplicationProblem, RequestId, RetryDirective, TaskHandoffGrant, TaskHandoffIssueRequest,
    TaskHandoffRedeemRequest, TaskHandoffRedeemed, WorkflowDefinitionActivateRequest,
    WorkflowDefinitionDiff, WorkflowDefinitionDiffRequest, WorkflowDefinitionDisposition,
    WorkflowDefinitionGetRequest, WorkflowDefinitionHistoryRequest, WorkflowDefinitionListRequest,
    WorkflowDefinitionRegisterRequest, WorkflowDefinitionRejectRequest,
    WorkflowDefinitionRetireRequest, WorkflowDefinitionValidateRequest,
    WorkflowDefinitionValidation, WorkflowRunCancelRequest, WorkflowRunGetRequest,
    WorkflowRunPauseRequest, WorkflowRunResumeRequest, WorkflowRunStartRequest,
};
use tracedecay_domain::{WorkflowDefinition, WorkflowRunProjection};

use crate::http::{
    HttpApplicationControls, MAX_HTTP_APPLICATION_BODY_BYTES, adapter_problem_response,
    invalid_request_response,
};

fn schema_name<T: JsonSchema>() -> Cow<'static, str> {
    T::schema_name()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkflowOperation {
    RegisterDefinition,
    ActivateDefinition,
    RetireDefinition,
    RejectDefinition,
    ValidateDefinition,
    GetDefinition,
    ListDefinitions,
    DefinitionHistory,
    DiffDefinition,
    HandoffIssue,
    HandoffRedeem,
    StartRun,
    PauseRun,
    ResumeRun,
    CancelRun,
    GetRun,
}

impl WorkflowOperation {
    pub const ALL: [Self; 16] = [
        Self::RegisterDefinition,
        Self::ActivateDefinition,
        Self::RetireDefinition,
        Self::RejectDefinition,
        Self::ValidateDefinition,
        Self::GetDefinition,
        Self::ListDefinitions,
        Self::DefinitionHistory,
        Self::DiffDefinition,
        Self::HandoffIssue,
        Self::HandoffRedeem,
        Self::StartRun,
        Self::PauseRun,
        Self::ResumeRun,
        Self::CancelRun,
        Self::GetRun,
    ];

    pub const fn operation_id_str(self) -> &'static str {
        match self {
            Self::RegisterDefinition => "operation.workflow.register_definition",
            Self::ActivateDefinition => "operation.workflow.activate_definition",
            Self::RetireDefinition => "operation.workflow.retire_definition",
            Self::RejectDefinition => "operation.workflow.reject_definition",
            Self::ValidateDefinition => "operation.workflow.validate_definition",
            Self::GetDefinition => "operation.workflow.get_definition",
            Self::ListDefinitions => "operation.workflow.list_definitions",
            Self::DefinitionHistory => "operation.workflow.definition_history",
            Self::DiffDefinition => "operation.workflow.diff_definition",
            Self::HandoffIssue => "operation.workflow.handoff_issue",
            Self::HandoffRedeem => "operation.workflow.handoff_redeem",
            Self::StartRun => "operation.workflow.start_run",
            Self::PauseRun => "operation.workflow.pause_run",
            Self::ResumeRun => "operation.workflow.resume_run",
            Self::CancelRun => "operation.workflow.cancel_run",
            Self::GetRun => "operation.workflow.get_run",
        }
    }

    pub const fn operation_key(self) -> &'static str {
        match self {
            Self::RegisterDefinition => "register_definition",
            Self::ActivateDefinition => "activate_definition",
            Self::RetireDefinition => "retire_definition",
            Self::RejectDefinition => "reject_definition",
            Self::ValidateDefinition => "validate_definition",
            Self::GetDefinition => "get_definition",
            Self::ListDefinitions => "list_definitions",
            Self::DefinitionHistory => "definition_history",
            Self::DiffDefinition => "diff_definition",
            Self::HandoffIssue => "handoff_issue",
            Self::HandoffRedeem => "handoff_redeem",
            Self::StartRun => "start_run",
            Self::PauseRun => "pause_run",
            Self::ResumeRun => "resume_run",
            Self::CancelRun => "cancel_run",
            Self::GetRun => "get_run",
        }
    }

    pub fn from_operation_key(key: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|operation| operation.operation_key() == key)
    }

    /// Whether the operation reads without producing a durable effect.
    /// Parity with the catalog's effect class is pinned by
    /// `read_only_operations_mirror_the_catalog_effect_class`.
    pub const fn is_read_only(self) -> bool {
        matches!(
            self,
            Self::ValidateDefinition
                | Self::GetDefinition
                | Self::ListDefinitions
                | Self::DefinitionHistory
                | Self::DiffDefinition
                | Self::GetRun
        )
    }

    pub fn from_cli_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|operation| {
            operation.operation_key() == name || operation.route_segment() == name
        })
    }

    pub const fn route_segment(self) -> &'static str {
        match self {
            Self::RegisterDefinition => "register-definition",
            Self::ActivateDefinition => "activate-definition",
            Self::RetireDefinition => "retire-definition",
            Self::RejectDefinition => "reject-definition",
            Self::ValidateDefinition => "validate-definition",
            Self::GetDefinition => "get-definition",
            Self::ListDefinitions => "list-definitions",
            Self::DefinitionHistory => "definition-history",
            Self::DiffDefinition => "diff-definition",
            Self::HandoffIssue => "handoff-issue",
            Self::HandoffRedeem => "handoff-redeem",
            Self::StartRun => "start-run",
            Self::PauseRun => "pause-run",
            Self::ResumeRun => "resume-run",
            Self::CancelRun => "cancel-run",
            Self::GetRun => "get-run",
        }
    }

    pub const fn route_path(self) -> &'static str {
        match self {
            Self::RegisterDefinition => "/workflow/register-definition",
            Self::ActivateDefinition => "/workflow/activate-definition",
            Self::RetireDefinition => "/workflow/retire-definition",
            Self::RejectDefinition => "/workflow/reject-definition",
            Self::ValidateDefinition => "/workflow/validate-definition",
            Self::GetDefinition => "/workflow/get-definition",
            Self::ListDefinitions => "/workflow/list-definitions",
            Self::DefinitionHistory => "/workflow/definition-history",
            Self::DiffDefinition => "/workflow/diff-definition",
            Self::HandoffIssue => "/workflow/handoff-issue",
            Self::HandoffRedeem => "/workflow/handoff-redeem",
            Self::StartRun => "/workflow/start-run",
            Self::PauseRun => "/workflow/pause-run",
            Self::ResumeRun => "/workflow/resume-run",
            Self::CancelRun => "/workflow/cancel-run",
            Self::GetRun => "/workflow/get-run",
        }
    }

    pub const fn application_route_path(self) -> &'static str {
        match self {
            Self::RegisterDefinition => "/application/workflow/register-definition",
            Self::ActivateDefinition => "/application/workflow/activate-definition",
            Self::RetireDefinition => "/application/workflow/retire-definition",
            Self::RejectDefinition => "/application/workflow/reject-definition",
            Self::ValidateDefinition => "/application/workflow/validate-definition",
            Self::GetDefinition => "/application/workflow/get-definition",
            Self::ListDefinitions => "/application/workflow/list-definitions",
            Self::DefinitionHistory => "/application/workflow/definition-history",
            Self::DiffDefinition => "/application/workflow/diff-definition",
            Self::HandoffIssue => "/application/workflow/handoff-issue",
            Self::HandoffRedeem => "/application/workflow/handoff-redeem",
            Self::StartRun => "/application/workflow/start-run",
            Self::PauseRun => "/application/workflow/pause-run",
            Self::ResumeRun => "/application/workflow/resume-run",
            Self::CancelRun => "/application/workflow/cancel-run",
            Self::GetRun => "/application/workflow/get-run",
        }
    }

    pub fn request_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::RegisterDefinition => schema_name::<WorkflowDefinitionRegisterRequest>(),
            Self::ActivateDefinition => schema_name::<WorkflowDefinitionActivateRequest>(),
            Self::RetireDefinition => schema_name::<WorkflowDefinitionRetireRequest>(),
            Self::RejectDefinition => schema_name::<WorkflowDefinitionRejectRequest>(),
            Self::ValidateDefinition => schema_name::<WorkflowDefinitionValidateRequest>(),
            Self::GetDefinition => schema_name::<WorkflowDefinitionGetRequest>(),
            Self::ListDefinitions => schema_name::<WorkflowDefinitionListRequest>(),
            Self::DefinitionHistory => schema_name::<WorkflowDefinitionHistoryRequest>(),
            Self::DiffDefinition => schema_name::<WorkflowDefinitionDiffRequest>(),
            Self::HandoffIssue => schema_name::<TaskHandoffIssueRequest>(),
            Self::HandoffRedeem => schema_name::<TaskHandoffRedeemRequest>(),
            Self::StartRun => schema_name::<WorkflowRunStartRequest>(),
            Self::PauseRun => schema_name::<WorkflowRunPauseRequest>(),
            Self::ResumeRun => schema_name::<WorkflowRunResumeRequest>(),
            Self::CancelRun => schema_name::<WorkflowRunCancelRequest>(),
            Self::GetRun => schema_name::<WorkflowRunGetRequest>(),
        }
    }

    pub fn result_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::RegisterDefinition => schema_name::<WorkflowDefinition>(),
            Self::ActivateDefinition | Self::RetireDefinition | Self::RejectDefinition => {
                schema_name::<WorkflowDefinitionDisposition>()
            }
            Self::ValidateDefinition => schema_name::<WorkflowDefinitionValidation>(),
            Self::GetDefinition => schema_name::<WorkflowDefinition>(),
            Self::ListDefinitions | Self::DefinitionHistory => {
                schema_name::<Vec<WorkflowDefinition>>()
            }
            Self::DiffDefinition => schema_name::<WorkflowDefinitionDiff>(),
            Self::HandoffIssue => schema_name::<TaskHandoffGrant>(),
            Self::HandoffRedeem => schema_name::<TaskHandoffRedeemed>(),
            Self::StartRun | Self::PauseRun | Self::ResumeRun | Self::CancelRun | Self::GetRun => {
                schema_name::<WorkflowRunProjection>()
            }
        }
    }

    pub fn from_route_segment(segment: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|operation| operation.route_segment() == segment)
    }
}

impl FromStr for WorkflowOperation {
    type Err = String;

    fn from_str(segment: &str) -> Result<Self, Self::Err> {
        Self::from_route_segment(segment)
            .ok_or_else(|| format!("unknown Workflow operation route segment: {segment}"))
    }
}

#[derive(Clone, Debug)]
pub struct WorkflowHttpRequest {
    pub operation: WorkflowOperation,
    pub request_id: RequestId,
    pub controls: HttpApplicationControls,
    pub body: Value,
}

pub type WorkflowInvocationFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

pub trait WorkflowApplicationOwner: Clone + Send + Sync + 'static {
    fn invoke_workflow(&self, request: WorkflowHttpRequest) -> WorkflowInvocationFuture;
}

impl<F, Fut> WorkflowApplicationOwner for F
where
    F: Fn(WorkflowHttpRequest) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn invoke_workflow(&self, request: WorkflowHttpRequest) -> WorkflowInvocationFuture {
        Box::pin((self)(request))
    }
}

pub fn workflow_application_router<O>(owner: O) -> Router
where
    O: WorkflowApplicationOwner,
{
    Router::new()
        .route("/workflow/{operation}", post(operation::<O>))
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owner)
}

async fn operation<O>(
    Path(segment): Path<String>,
    State(owner): State<O>,
    Extension(request_id): Extension<RequestId>,
    Extension(controls): Extension<HttpApplicationControls>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: WorkflowApplicationOwner,
{
    let Some(operation) = WorkflowOperation::from_route_segment(&segment) else {
        return adapter_problem_response(
            request_id,
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        );
    };
    let Ok(Json(body)) = body else {
        return invalid_request_response(
            request_id,
            "workflow.invalid_body",
            "The Workflow request body is invalid or exceeds the configured limit",
        );
    };
    owner
        .invoke_workflow(WorkflowHttpRequest {
            operation,
            request_id,
            controls,
            body,
        })
        .await
}

pub fn workflow_invalid_request_response(request_id: RequestId) -> Response {
    invalid_request_response(
        request_id,
        "workflow.invalid_request",
        "The Workflow application request is invalid",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::extract::Extension;
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use tower::ServiceExt;
    use tracedecay_application::{CancellationSignal, Deadline, RequestId};
    use tracedecay_domain::UtcMicros;

    use super::{
        HttpApplicationControls, WorkflowHttpRequest, WorkflowOperation,
        workflow_application_router,
    };

    #[test]
    fn descriptor_derives_route_and_catalog_identity() {
        for operation in WorkflowOperation::ALL {
            assert_eq!(
                operation.application_route_path(),
                format!("/application{}", operation.route_path())
            );
            assert!(
                operation
                    .operation_id_str()
                    .starts_with("operation.workflow.")
            );
        }
    }

    #[test]
    fn read_only_operations_mirror_the_catalog_effect_class() {
        let registry = tracedecay_application::workflow_executable_binding_registry()
            .expect("canonical Workflow executable registry");
        for operation in WorkflowOperation::ALL {
            let operation_id =
                tracedecay_tool_catalog::OperationId::new(operation.operation_id_str().to_owned())
                    .expect("catalog operation id");
            let binding = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .expect("every mounted Workflow operation has an executable binding");
            assert_eq!(
                operation.is_read_only(),
                binding.effect() == tracedecay_tool_catalog::EffectClass::Read,
                "{} read-only declaration must mirror the catalog effect class",
                operation.operation_key()
            );
        }
    }

    #[tokio::test]
    async fn router_dispatches_every_advertised_definition_and_runtime_operation() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let owner_seen = Arc::clone(&seen);
        let app = workflow_application_router(move |request: WorkflowHttpRequest| {
            let owner_seen = Arc::clone(&owner_seen);
            async move {
                owner_seen
                    .lock()
                    .expect("captured Workflow operations")
                    .push(request.operation);
                StatusCode::NO_CONTENT.into_response()
            }
        });
        let deadline = Deadline::new(UtcMicros(9_999_999)).expect("deadline");

        for (index, operation) in WorkflowOperation::ALL.into_iter().enumerate() {
            let request_id =
                RequestId::new(format!("request.http.workflow.{index}")).expect("request");
            let cancellation =
                CancellationSignal::active(format!("cancellation.http.workflow.{index}"))
                    .expect("cancellation");
            let response = app
                .clone()
                .layer(Extension(request_id))
                .layer(Extension(HttpApplicationControls {
                    deadline: deadline.clone(),
                    cancellation,
                }))
                .oneshot(
                    Request::post(operation.route_path())
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .expect("HTTP request"),
                )
                .await
                .expect("HTTP response");
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }

        assert_eq!(
            *seen.lock().expect("captured Workflow operations"),
            WorkflowOperation::ALL
        );
    }
}
