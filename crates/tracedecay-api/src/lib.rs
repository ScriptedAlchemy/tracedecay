//! Thin HTTP/SSE adapter contracts over `tracedecay-application`.
//!
//! The executable owns `CanonicalInvocation`; this crate receives the resolved
//! binding and its application result after dispatch, then encodes that result
//! for HTTP. It owns no store, query, policy, or LSP tunnel authority.
//!
//! [`read_model`] is the normative dashboard presentation envelope and the
//! generation source for the frontend's wire contracts; [`doctor`] owns the
//! read-only Doctor/health route descriptors and their DTO mapping. Both
//! translate admitted application contracts and evaluate nothing themselves.
//!
#![forbid(unsafe_code)]

pub mod configuration;
pub mod doctor;
pub mod feedback;
mod http;
pub mod multi_root;
pub mod read_model;
pub mod remediation;
mod sse;
pub mod work;
pub mod workflow;

use serde::Serialize;
use thiserror::Error;
use tracedecay_application::{
    ApplicationEnvelope, ApplicationProblemEnvelope, ApplicationProblemKind, ApplicationResult,
    OperationTermination, RequestId, StreamEvent, StreamEventKind, StreamFrontier, StreamGap,
    StreamTermination,
};
use tracedecay_tool_catalog::BindingId;

pub use http::{
    HttpApplicationControls, HttpApplicationInvocationFuture, HttpApplicationOperation,
    HttpApplicationOwnerKind, HttpApplicationOwners, HttpApplicationRequest, HttpRouteDocumentV1,
    application_problem_response, application_router, configuration_application_router,
    feedback_application_router, http_route_documents,
};
pub use multi_root::{
    MultiRootApplicationOwner, MultiRootHttpOperation, MultiRootHttpRequest,
    MultiRootInvocationFuture, multi_root_application_router,
};
pub use sse::sse_response;
pub use work::{
    WorkApplicationOwner, WorkHttpRequest, WorkInvocationFuture, WorkOperation,
    WorkOperationFamily, work_application_router, work_core_router, work_invalid_request_response,
};
pub use workflow::{
    WorkflowApplicationOwner, WorkflowHttpRequest, WorkflowInvocationFuture, WorkflowOperation,
    workflow_application_router, workflow_invalid_request_response,
};

/// A resolved canonical invocation result ready for HTTP presentation.
pub struct CanonicalInvocationResult<T> {
    pub binding_id: BindingId,
    pub result: ApplicationResult<T>,
}

impl<T> CanonicalInvocationResult<T> {
    pub fn new(binding_id: BindingId, result: ApplicationResult<T>) -> Self {
        Self { binding_id, result }
    }

    pub fn into_http_json(self) -> HttpJsonEnvelope<T> {
        match self.result {
            Ok(application) => HttpJsonEnvelope::Success(Box::new(HttpSuccessEnvelope {
                binding_id: self.binding_id,
                application,
            })),
            Err(application) => {
                let binding_id = (application.problem.kind()
                    != ApplicationProblemKind::NotFoundOrNotAuthorized)
                    .then_some(self.binding_id);
                HttpJsonEnvelope::Problem(Box::new(HttpProblemEnvelope {
                    binding_id,
                    application,
                }))
            }
        }
    }
}

/// Outbound HTTP JSON is either an admitted application result or a
/// pre-admission application problem.
#[derive(Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum HttpJsonEnvelope<T> {
    Success(Box<HttpSuccessEnvelope<T>>),
    Problem(Box<HttpProblemEnvelope>),
}

/// HTTP success preserves the application contract, request identity, scope,
/// and outcome without reimplementing application semantics.
#[derive(Serialize)]
pub struct HttpSuccessEnvelope<T> {
    pub binding_id: BindingId,
    #[serde(flatten)]
    pub application: ApplicationEnvelope<T>,
}

/// HTTP problem preserves the application's safe problem record verbatim.
#[derive(Serialize)]
pub struct HttpProblemEnvelope {
    /// Concealed denials omit this field so binding existence cannot become an
    /// authorization oracle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<BindingId>,
    #[serde(flatten)]
    pub application: ApplicationProblemEnvelope,
}

/// SSE presentation of canonical stream events.
#[derive(Serialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum HttpSseEvent<T> {
    Open {
        correlation_id: RequestId,
        frontier: StreamFrontier,
    },
    Item {
        sequence: u64,
        item: T,
    },
    Progress {
        sequence: u64,
        completed: u64,
        total: Option<u64>,
    },
    ResumeGap {
        sequence: u64,
        gap: StreamGap,
    },
    Completed {
        sequence: u64,
        terminal: StreamTermination,
    },
    Cancelled {
        sequence: u64,
        terminal: StreamTermination,
    },
    TimedOut {
        sequence: u64,
        terminal: StreamTermination,
    },
    Failed {
        sequence: u64,
        terminal: StreamTermination,
    },
    Partial {
        sequence: u64,
        terminal: StreamTermination,
    },
    EffectUnknown {
        sequence: u64,
        terminal: StreamTermination,
    },
}

impl<T> HttpSseEvent<T> {
    pub const fn event_name(&self) -> &'static str {
        match self {
            Self::Open { .. } => "open",
            Self::Item { .. } => "item",
            Self::Progress { .. } => "progress",
            Self::ResumeGap { .. } => "resume_gap",
            Self::Completed { .. } => "completed",
            Self::Cancelled { .. } => "cancelled",
            Self::TimedOut { .. } => "timed_out",
            Self::Failed { .. } => "failed",
            Self::Partial { .. } => "partial",
            Self::EffectUnknown { .. } => "effect_unknown",
        }
    }

    pub const fn sequence(&self) -> Option<u64> {
        match self {
            Self::Open { .. } => None,
            Self::Item { sequence, .. }
            | Self::Progress { sequence, .. }
            | Self::ResumeGap { sequence, .. }
            | Self::Completed { sequence, .. }
            | Self::Cancelled { sequence, .. }
            | Self::TimedOut { sequence, .. }
            | Self::Failed { sequence, .. }
            | Self::Partial { sequence, .. }
            | Self::EffectUnknown { sequence, .. } => Some(*sequence),
        }
    }

    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. }
                | Self::Cancelled { .. }
                | Self::TimedOut { .. }
                | Self::Failed { .. }
                | Self::Partial { .. }
                | Self::EffectUnknown { .. }
        )
    }
}

impl<T> From<StreamEvent<T>> for HttpSseEvent<T> {
    fn from(event: StreamEvent<T>) -> Self {
        let sequence = event.sequence;
        match event.kind {
            StreamEventKind::Item(item) => Self::Item { sequence, item },
            StreamEventKind::Progress { completed, total } => Self::Progress {
                sequence,
                completed,
                total,
            },
            StreamEventKind::Gap(gap) => Self::ResumeGap { sequence, gap },
            StreamEventKind::Terminal(terminal) => match terminal.termination {
                OperationTermination::Completed => Self::Completed { sequence, terminal },
                OperationTermination::Cancelled => Self::Cancelled { sequence, terminal },
                OperationTermination::TimedOut => Self::TimedOut { sequence, terminal },
                OperationTermination::Failed => Self::Failed { sequence, terminal },
                OperationTermination::Partial => Self::Partial { sequence, terminal },
                OperationTermination::EffectUnknown => Self::EffectUnknown { sequence, terminal },
            },
        }
    }
}

/// SSE framing failures. Application failures remain canonical terminal events.
#[derive(Debug, Error)]
pub enum HttpAdapterError {
    #[error("canonical SSE event could not be encoded")]
    EventEncoding,
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::http::invalid_request_problem;
    use super::{
        CanonicalInvocationResult, HttpApplicationOperation, HttpApplicationOwnerKind,
        HttpSseEvent, application_router,
    };
    use tracedecay_application::{
        ApplicationProblem, ApplicationProblemEnvelope, RequestId, ResultContractRef,
        RetryDirective, SafeDiagnostic, StreamEvent, StreamEventKind,
    };
    use tracedecay_tool_catalog::{BindingId, SchemaId};

    #[test]
    fn sse_preserves_canonical_item_and_progress_events() {
        let item = HttpSseEvent::from(StreamEvent::item(7, "value").expect("item"));
        assert_eq!(item.sequence(), Some(7));
        assert!(!item.is_terminal());
        assert_eq!(
            serde_json::to_value(item).expect("serialize item"),
            serde_json::json!({
                "event": "item",
                "data": {"sequence": 7, "item": "value"}
            })
        );

        let progress = HttpSseEvent::<()>::from(StreamEvent {
            sequence: 8,
            kind: StreamEventKind::Progress {
                completed: 2,
                total: Some(5),
            },
        });
        assert_eq!(progress.sequence(), Some(8));
        assert!(!progress.is_terminal());
        assert_eq!(
            serde_json::to_value(progress).expect("serialize progress"),
            serde_json::json!({
                "event": "progress",
                "data": {"sequence": 8, "completed": 2, "total": 5}
            })
        );
    }

    #[test]
    fn http_operations_dispatch_to_concrete_owner_families() {
        assert_eq!(
            HttpApplicationOperation::DiagnosticsRead.owner_kind(),
            HttpApplicationOwnerKind::Primitive
        );
        for operation in [
            "multi_root_scope_set_read",
            "multi_root_scope_set_compare_and_swap",
            "multi_root_execute",
        ] {
            assert!(
                HttpApplicationOperation::from_catalog_name(operation).is_none(),
                "{operation} must not be catalog-addressable"
            );
        }
    }

    #[tokio::test]
    async fn application_router_does_not_mount_multi_root_routes() {
        let app = application_router(|request: super::HttpApplicationRequest| async move {
            CanonicalInvocationResult::<serde_json::Value>::new(
                BindingId::new("binding.http.test.v1").expect("binding"),
                Err(ApplicationProblemEnvelope::new(
                    ResultContractRef::new(SchemaId::new("schema.test.result").expect("schema"), 1)
                        .expect("contract"),
                    request.request_id,
                    ApplicationProblem::unavailable(
                        SafeDiagnostic::new("test.unavailable", "Unavailable").expect("diagnostic"),
                    ),
                )),
            )
        });

        for path in [
            "/multi-root/scope-set/read",
            "/multi-root/scope-set/compare-and-swap",
            "/multi-root/execute",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::post(path)
                        .body(Body::empty())
                        .expect("HTTP request"),
                )
                .await
                .expect("router response");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[test]
    fn adapter_rejections_use_the_canonical_problem_envelope() {
        let envelope = invalid_request_problem(
            RequestId::new("request.http.invalid").unwrap(),
            "http.invalid_query",
            "The HTTP query is invalid",
        );
        let value = serde_json::to_value(envelope).expect("serialize canonical problem");

        assert_eq!(value["request_id"], "request.http.invalid");
        assert_eq!(value["problem"]["kind"], "invalid_request");
        assert_eq!(value["problem"]["code"], "http.invalid_query");
        assert_eq!(value["problem"]["owning_layer"], "adapter");
        assert_eq!(value["problem"]["diagnostic"]["code"], "http.invalid_query");
    }

    #[test]
    fn concealed_http_problem_omits_binding_identity() {
        let result = Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(SchemaId::new("schema.test.result").unwrap(), 1).unwrap(),
            RequestId::new("request.test").unwrap(),
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        ));
        let value = serde_json::to_value(
            CanonicalInvocationResult::<()>::new(
                BindingId::new("binding.http.test.v1").unwrap(),
                result,
            )
            .into_http_json(),
        )
        .unwrap();

        assert_eq!(value["kind"], "problem");
        assert!(value["value"].get("binding_id").is_none());
    }

    #[test]
    fn non_concealed_http_problem_preserves_binding_identity() {
        let result = Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(SchemaId::new("schema.test.result").unwrap(), 1).unwrap(),
            RequestId::new("request.test").unwrap(),
            ApplicationProblem::unavailable(
                SafeDiagnostic::new("test.unavailable", "Temporarily unavailable").unwrap(),
            ),
        ));
        let value = serde_json::to_value(
            CanonicalInvocationResult::<()>::new(
                BindingId::new("binding.http.test.v1").unwrap(),
                result,
            )
            .into_http_json(),
        )
        .unwrap();

        assert_eq!(value["kind"], "problem");
        assert_eq!(value["value"]["binding_id"], "binding.http.test.v1");
    }
}
