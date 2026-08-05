//! Typed HTTP adapter for the canonical Plan 24 product journey.

use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde_json::Value;
use tracedecay_application::{
    ApplicationProblem, ExpandWorkEvidenceRequestV1, GenerateWorkProposalRequestV1, RequestId,
    RetryDirective, WorkEvidenceExpansionV1, WorkProductMutationReceiptV1,
    WorkProductMutationRequestV1, WorkProductOperationV1, WorkProductProjectionReadV1,
    WorkProductProjectionsRequestV1, WorkProductSnapshotRequestV1, WorkTaskEvidenceRequestV1,
    WorkTopologyReadV1,
};
use tracedecay_domain::{WorkProposalV1, WorkTaskEvidenceV1};

use crate::http::{
    HttpApplicationControls, MAX_HTTP_APPLICATION_BODY_BYTES, adapter_problem,
    application_problem_response, invalid_request_response,
};

fn schema_name<T: JsonSchema>() -> Cow<'static, str> {
    T::schema_name()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkProductOperationFamily {
    Product,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkProductHttpOperation {
    ProductSnapshot,
    ProductProjections,
    TaskEvidence,
    ExpandTaskEvidence,
    GenerateWorkProposal,
    ApplyWorkCommand,
}

impl WorkProductHttpOperation {
    pub const ALL: [Self; 6] = [
        Self::ProductSnapshot,
        Self::ProductProjections,
        Self::TaskEvidence,
        Self::ExpandTaskEvidence,
        Self::GenerateWorkProposal,
        Self::ApplyWorkCommand,
    ];

    pub const fn application_operation(self) -> WorkProductOperationV1 {
        match self {
            Self::ProductSnapshot => WorkProductOperationV1::ProductSnapshot,
            Self::ProductProjections => WorkProductOperationV1::ProductProjections,
            Self::TaskEvidence => WorkProductOperationV1::TaskEvidence,
            Self::ExpandTaskEvidence => WorkProductOperationV1::ExpandTaskEvidence,
            Self::GenerateWorkProposal => WorkProductOperationV1::GenerateWorkProposal,
            Self::ApplyWorkCommand => WorkProductOperationV1::ApplyWorkCommand,
        }
    }

    pub const fn operation_key(self) -> &'static str {
        self.application_operation().key()
    }

    pub const fn operation_id_str(self) -> &'static str {
        match self {
            Self::ProductSnapshot => "operation.work.product_snapshot",
            Self::ProductProjections => "operation.work.product_projections",
            Self::TaskEvidence => "operation.work.task_evidence",
            Self::ExpandTaskEvidence => "operation.work.expand_task_evidence",
            Self::GenerateWorkProposal => "operation.work.generate_work_proposal",
            Self::ApplyWorkCommand => "operation.work.apply_work_command",
        }
    }

    pub const fn family(self) -> WorkProductOperationFamily {
        WorkProductOperationFamily::Product
    }

    pub const fn route_segment(self) -> &'static str {
        match self {
            Self::ProductSnapshot => "snapshot",
            Self::ProductProjections => "projections",
            Self::TaskEvidence => "task-evidence",
            Self::ExpandTaskEvidence => "expand-task-evidence",
            Self::GenerateWorkProposal => "generate-proposal",
            Self::ApplyWorkCommand => "apply-command",
        }
    }

    pub const fn route_path(self) -> &'static str {
        match self {
            Self::ProductSnapshot => "/work/product/snapshot",
            Self::ProductProjections => "/work/product/projections",
            Self::TaskEvidence => "/work/product/task-evidence",
            Self::ExpandTaskEvidence => "/work/product/expand-task-evidence",
            Self::GenerateWorkProposal => "/work/product/generate-proposal",
            Self::ApplyWorkCommand => "/work/product/apply-command",
        }
    }

    pub const fn application_route_path(self) -> &'static str {
        match self {
            Self::ProductSnapshot => "/application/work/product/snapshot",
            Self::ProductProjections => "/application/work/product/projections",
            Self::TaskEvidence => "/application/work/product/task-evidence",
            Self::ExpandTaskEvidence => "/application/work/product/expand-task-evidence",
            Self::GenerateWorkProposal => "/application/work/product/generate-proposal",
            Self::ApplyWorkCommand => "/application/work/product/apply-command",
        }
    }

    pub const fn is_read_only(self) -> bool {
        self.application_operation().is_read_only()
    }

    pub fn request_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::ProductSnapshot => schema_name::<WorkProductSnapshotRequestV1>(),
            Self::ProductProjections => schema_name::<WorkProductProjectionsRequestV1>(),
            Self::TaskEvidence => schema_name::<WorkTaskEvidenceRequestV1>(),
            Self::ExpandTaskEvidence => schema_name::<ExpandWorkEvidenceRequestV1>(),
            Self::GenerateWorkProposal => schema_name::<GenerateWorkProposalRequestV1>(),
            Self::ApplyWorkCommand => schema_name::<WorkProductMutationRequestV1>(),
        }
    }

    pub fn result_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::ProductSnapshot => schema_name::<WorkTopologyReadV1>(),
            Self::ProductProjections => schema_name::<WorkProductProjectionReadV1>(),
            Self::TaskEvidence => schema_name::<WorkTaskEvidenceV1>(),
            Self::ExpandTaskEvidence => schema_name::<WorkEvidenceExpansionV1>(),
            Self::GenerateWorkProposal => schema_name::<WorkProposalV1>(),
            Self::ApplyWorkCommand => schema_name::<WorkProductMutationReceiptV1>(),
        }
    }

    fn parse(segment: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|operation| operation.route_segment() == segment)
    }
}

#[derive(Clone, Debug)]
pub struct WorkProductHttpRequest {
    pub operation: WorkProductHttpOperation,
    pub request_id: RequestId,
    pub controls: HttpApplicationControls,
    pub body: Value,
}

pub type WorkProductInvocationFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

pub trait WorkProductApplicationOwner: Clone + Send + Sync + 'static {
    fn invoke_work_product(&self, request: WorkProductHttpRequest) -> WorkProductInvocationFuture;
}

impl<F, Fut> WorkProductApplicationOwner for F
where
    F: Fn(WorkProductHttpRequest) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn invoke_work_product(&self, request: WorkProductHttpRequest) -> WorkProductInvocationFuture {
        Box::pin((self)(request))
    }
}

pub fn work_product_application_router<O>(owner: O) -> Router
where
    O: WorkProductApplicationOwner,
{
    Router::new()
        .route("/work/product/{operation}", post(operation::<O>))
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owner)
}

pub fn work_product_router<O>(owner: O) -> Router
where
    O: WorkProductApplicationOwner,
{
    Router::new()
        .route("/{operation}", post(operation::<O>))
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
    O: WorkProductApplicationOwner,
{
    let Some(operation) = WorkProductHttpOperation::parse(&segment) else {
        return application_problem_response(adapter_problem(
            request_id,
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        ));
    };
    let Ok(Json(body)) = body else {
        return invalid_request_response(
            request_id,
            "work.product.invalid_body",
            "The Work product request body is invalid or exceeds the configured limit",
        );
    };
    owner
        .invoke_work_product(WorkProductHttpRequest {
            operation,
            request_id,
            controls,
            body,
        })
        .await
}

pub fn work_product_invalid_request_response(request_id: RequestId) -> Response {
    invalid_request_response(
        request_id,
        "work.product.invalid_request",
        "The Work product application request is invalid",
    )
}
