//! Transport-neutral application invocation contract.
//!
//! MCP, HTTP, CLI, and in-process daemon adapters share one request/response
//! vocabulary here. This module has no Axum, Tokio, store, or root-daemon
//! dependency: adapters own transport, and the daemon owns admission.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_tool_catalog::{BindingId, BindingSurface, SurfaceOperationName};

use crate::context::{CancellationSignal, Deadline, RequestId, ResolvedScope};
use crate::error::ApplicationContractError;
use crate::result::{ApplicationEnvelope, ApplicationProblem, ResultContractRef};
use crate::retrieval::PageRequest;

/// Where an invocation should resolve its project scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationTarget {
    CurrentProject,
    Resolved(ResolvedScope),
}

impl InvocationTarget {
    pub fn resolved(&self) -> Option<&ResolvedScope> {
        match self {
            Self::CurrentProject => None,
            Self::Resolved(scope) => Some(scope),
        }
    }
}

/// Bound catalog identity for a surface operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationInvocationBinding {
    binding_id: BindingId,
    surface: BindingSurface,
    operation: SurfaceOperationName,
    result_contract: ResultContractRef,
    page: PageRequest,
}

impl ApplicationInvocationBinding {
    pub fn new(
        binding_id: BindingId,
        surface: BindingSurface,
        operation: SurfaceOperationName,
        result_contract: ResultContractRef,
        page: PageRequest,
    ) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            binding_id,
            surface,
            operation,
            result_contract,
            page,
        })
    }

    pub fn binding_id(&self) -> &BindingId {
        &self.binding_id
    }

    pub const fn surface(&self) -> BindingSurface {
        self.surface
    }

    pub fn operation(&self) -> &SurfaceOperationName {
        &self.operation
    }

    pub fn result_contract(&self) -> &ResultContractRef {
        &self.result_contract
    }

    pub fn page(&self) -> &PageRequest {
        &self.page
    }

    pub fn into_parts(
        self,
    ) -> (
        BindingId,
        BindingSurface,
        SurfaceOperationName,
        ResultContractRef,
        PageRequest,
    ) {
        (
            self.binding_id,
            self.surface,
            self.operation,
            self.result_contract,
            self.page,
        )
    }
}

/// Request identity, scope target, deadline, and cancellation for one invoke.
#[derive(Clone, Debug)]
pub struct ApplicationInvocationContext {
    request_id: RequestId,
    target: InvocationTarget,
    deadline: Deadline,
    cancellation: CancellationSignal,
}

impl ApplicationInvocationContext {
    pub fn new(
        request_id: RequestId,
        target: InvocationTarget,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            request_id,
            target,
            deadline,
            cancellation,
        })
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn target(&self) -> &InvocationTarget {
        &self.target
    }

    pub fn deadline(&self) -> &Deadline {
        &self.deadline
    }

    pub fn cancellation(&self) -> &CancellationSignal {
        &self.cancellation
    }

    pub fn into_parts(self) -> (RequestId, InvocationTarget, Deadline, CancellationSignal) {
        (
            self.request_id,
            self.target,
            self.deadline,
            self.cancellation,
        )
    }
}

/// Closed set of transport-neutral application requests.
#[derive(Clone, Debug, PartialEq)]
pub enum ApplicationRequest {
    Surface {
        binding: ApplicationInvocationBinding,
        payload: Value,
    },
    OperationEvents {
        operation_id: RequestId,
        max_events: u32,
        after_sequence: Option<u64>,
    },
    OperationCancel {
        operation_id: RequestId,
    },
    FeedbackObservation {
        configuration_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: Value,
    },
}

impl ApplicationRequest {
    pub fn surface(
        binding: ApplicationInvocationBinding,
        payload: Value,
    ) -> Result<Self, ApplicationContractError> {
        if !payload.is_object() && !payload.is_null() {
            return Err(ApplicationContractError::InvalidRange {
                field: "application surface payload",
            });
        }
        Ok(Self::Surface { binding, payload })
    }

    pub fn operation_events(
        operation_id: RequestId,
        max_events: u32,
        after_sequence: Option<u64>,
    ) -> Result<Self, ApplicationContractError> {
        if max_events == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "operation event page size",
            });
        }
        Ok(Self::OperationEvents {
            operation_id,
            max_events,
            after_sequence,
        })
    }

    pub fn operation_cancel(operation_id: RequestId) -> Result<Self, ApplicationContractError> {
        Ok(Self::OperationCancel { operation_id })
    }

    pub fn feedback_observation(
        configuration_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: Value,
    ) -> Result<Self, ApplicationContractError> {
        configuration_digest.validate().map_err(|_| {
            ApplicationContractError::InvalidIdentifier {
                field: "feedback observation configuration digest",
            }
        })?;
        if observed_at.0 <= 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "feedback observation time",
            });
        }
        Ok(Self::FeedbackObservation {
            configuration_digest,
            observed_at,
            event,
        })
    }

    pub fn binding(&self) -> Option<&ApplicationInvocationBinding> {
        match self {
            Self::Surface { binding, .. } => Some(binding),
            Self::OperationEvents { .. }
            | Self::OperationCancel { .. }
            | Self::FeedbackObservation { .. } => None,
        }
    }

    pub fn surface_payload(&self) -> Option<&Value> {
        match self {
            Self::Surface { payload, .. } => Some(payload),
            Self::OperationEvents { .. }
            | Self::OperationCancel { .. }
            | Self::FeedbackObservation { .. } => None,
        }
    }

    pub const fn is_stream(&self) -> bool {
        matches!(self, Self::OperationEvents { .. })
    }

    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::OperationCancel { .. })
    }

    pub fn feedback_observation_parts(&self) -> Option<(&ManifestDigest, UtcMicros, &Value)> {
        match self {
            Self::FeedbackObservation {
                configuration_digest,
                observed_at,
                event,
            } => Some((configuration_digest, *observed_at, event)),
            Self::Surface { .. } | Self::OperationEvents { .. } | Self::OperationCancel { .. } => {
                None
            }
        }
    }
}

/// One complete transport-neutral invocation.
#[derive(Clone, Debug)]
pub struct ApplicationInvocation {
    context: ApplicationInvocationContext,
    request: ApplicationRequest,
}

impl ApplicationInvocation {
    pub fn new(
        context: ApplicationInvocationContext,
        request: ApplicationRequest,
    ) -> Result<Self, ApplicationContractError> {
        Ok(Self { context, request })
    }

    pub fn context(&self) -> &ApplicationInvocationContext {
        &self.context
    }

    pub fn request(&self) -> &ApplicationRequest {
        &self.request
    }

    pub fn into_parts(self) -> (ApplicationInvocationContext, ApplicationRequest) {
        (self.context, self.request)
    }
}

/// Non-disclosing invocation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationError {
    Unavailable,
    Denied,
    Cancelled,
    DeadlineExceeded,
    InvalidRequest,
    Conflict,
}

impl From<ApplicationContractError> for InvocationError {
    fn from(_error: ApplicationContractError) -> Self {
        Self::InvalidRequest
    }
}

impl From<ApplicationProblem> for InvocationError {
    fn from(_problem: ApplicationProblem) -> Self {
        Self::Unavailable
    }
}

/// Stream page for an in-flight operation.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplicationStream {
    pub operation_id: RequestId,
    pub events: Vec<crate::StreamEvent<Value>>,
    pub frontier: crate::StreamFrontier,
    pub next_sequence: Option<u64>,
    pub terminated: bool,
}

/// Stream response wrapper kept distinct from unary responses.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplicationStreamResponse {
    pub stream: ApplicationStream,
}

/// Cancellation acknowledgement for an in-flight operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationCancellation {
    pub operation_id: RequestId,
    pub cancelled: bool,
}

/// Closed successful responses from the invocation executor.
#[derive(Clone, Debug, PartialEq)]
pub enum ApplicationResponse {
    Unary {
        envelope: Box<ApplicationEnvelope<Value>>,
    },
    Stream(ApplicationStreamResponse),
    Cancellation(InvocationCancellation),
    ObservationAccepted,
}

impl ApplicationResponse {
    pub fn unary(envelope: ApplicationEnvelope<Value>) -> Self {
        Self::Unary {
            envelope: Box::new(envelope),
        }
    }

    pub fn envelope(&self) -> Option<&ApplicationEnvelope<Value>> {
        match self {
            Self::Unary { envelope } => Some(envelope.as_ref()),
            Self::Stream(_) | Self::Cancellation(_) | Self::ObservationAccepted => None,
        }
    }
}

pub type ApplicationInvocationFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One canonical invoke path for every adapter surface.
pub trait ApplicationInvocationExecutor: Send + Sync {
    fn invoke<'a>(
        &'a self,
        invocation: ApplicationInvocation,
    ) -> ApplicationInvocationFuture<'a, Result<ApplicationResponse, InvocationError>>;
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::{
        ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{BindingId, BindingSurface, SchemaId, SurfaceOperationName};

    use crate::{
        CancellationSignal, Deadline, OpaqueCursor, PageRequest, RequestId, ResolvedScope,
        ResultContractRef, StreamFrontier,
    };

    use super::{
        ApplicationInvocation, ApplicationInvocationBinding, ApplicationInvocationContext,
        ApplicationRequest, ApplicationResponse, ApplicationStream, ApplicationStreamResponse,
        InvocationCancellation, InvocationTarget,
    };

    fn scope() -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new("project.invocation-test").unwrap(),
            RepositoryId::new("repository.invocation-test").unwrap(),
            WorktreeId::new("worktree.invocation-test").unwrap(),
            Some(RefId::new("refs/heads/test").unwrap()),
        )
        .unwrap()
    }

    fn binding(operation: &str) -> ApplicationInvocationBinding {
        ApplicationInvocationBinding::new(
            BindingId::new(format!("binding.mcp.{operation}.v1")).unwrap(),
            BindingSurface::Mcp,
            SurfaceOperationName::new(operation).unwrap(),
            ResultContractRef::new(
                SchemaId::new(format!("schema.application.{operation}.result")).unwrap(),
                1,
            )
            .unwrap(),
            PageRequest::new(
                10,
                Some(OpaqueCursor::new("cursor.invocation-test").unwrap()),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn invocation_context_pins_scope_without_accepting_a_grant() {
        let pinned = scope();
        let context = ApplicationInvocationContext::new(
            RequestId::new("request.invocation-test").unwrap(),
            InvocationTarget::Resolved(pinned.clone()),
            Deadline::new(UtcMicros(100)).unwrap(),
            CancellationSignal::active("cancel.invocation-test").unwrap(),
        )
        .unwrap();

        assert_eq!(context.target().resolved(), Some(&pinned));
        assert_eq!(context.request_id().as_str(), "request.invocation-test");
        assert_eq!(
            context.cancellation().context().token_id.as_str(),
            "cancel.invocation-test"
        );
    }

    #[test]
    fn bound_surface_request_keeps_exact_operation_and_payload() {
        let request = ApplicationRequest::surface(
            binding("configuration_get"),
            json!({"key": "mcp.tool_timings"}),
        )
        .unwrap();
        let invocation = ApplicationInvocation::new(
            ApplicationInvocationContext::new(
                RequestId::new("request.configuration-get").unwrap(),
                InvocationTarget::CurrentProject,
                Deadline::new(UtcMicros(100)).unwrap(),
                CancellationSignal::active("cancel.configuration-get").unwrap(),
            )
            .unwrap(),
            request,
        )
        .unwrap();

        let binding = invocation.request().binding().unwrap();
        assert_eq!(binding.surface(), BindingSurface::Mcp);
        assert_eq!(binding.operation().as_str(), "configuration_get");
        assert_eq!(
            invocation.request().surface_payload(),
            Some(&json!({"key": "mcp.tool_timings"}))
        );
    }

    #[test]
    fn stream_and_cancellation_requests_are_closed_contract_variants() {
        let operation_id = RequestId::new("request.originating-operation").unwrap();
        let stream = ApplicationRequest::operation_events(operation_id.clone(), 4, None).unwrap();
        let cancellation = ApplicationRequest::operation_cancel(operation_id.clone()).unwrap();

        assert!(stream.binding().is_none());
        assert!(cancellation.binding().is_none());
        assert!(stream.is_stream());
        assert!(cancellation.is_cancellation());
        let stream_response = ApplicationResponse::Stream(ApplicationStreamResponse {
            stream: ApplicationStream {
                operation_id: operation_id.clone(),
                events: Vec::new(),
                frontier: StreamFrontier {
                    next_sequence: 0,
                    retained_from_sequence: 0,
                    resume_token: None,
                },
                next_sequence: None,
                terminated: false,
            },
        });
        assert!(matches!(
            stream_response,
            ApplicationResponse::Stream(ApplicationStreamResponse {
                stream: ApplicationStream {
                    operation_id: stream_id,
                    events,
                    frontier: StreamFrontier {
                        next_sequence: 0,
                        retained_from_sequence: 0,
                        resume_token: None,
                    },
                    next_sequence: None,
                    terminated: false,
                },
            }) if stream_id == operation_id && events.is_empty()
        ));
        let cancellation_response = ApplicationResponse::Cancellation(InvocationCancellation {
            operation_id: operation_id.clone(),
            cancelled: true,
        });
        assert!(matches!(
            cancellation_response,
            ApplicationResponse::Cancellation(InvocationCancellation {
                operation_id: cancelled_id,
                cancelled: true,
            }) if cancelled_id == operation_id
        ));
    }

    #[test]
    fn observation_payload_is_data_not_invocation_authority() {
        let request = ApplicationRequest::feedback_observation(
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            UtcMicros(41),
            json!({"event": "delivered"}),
        )
        .unwrap();

        assert!(request.binding().is_none());
        assert_eq!(
            request
                .feedback_observation_parts()
                .map(|(_, _, event)| event),
            Some(&json!({"event": "delivered"}))
        );
    }
}
