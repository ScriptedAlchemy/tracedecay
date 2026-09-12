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

    #[hotpath::skip]
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

    #[hotpath::skip]
    pub const fn is_stream(&self) -> bool {
        matches!(self, Self::OperationEvents { .. })
    }

    #[hotpath::skip]
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

/// Invocation failure. Bare variants describe failures raised before the
/// daemon produced an authoritative answer; once the daemon has answered with
/// a typed [`ApplicationProblem`], that problem IS the failure and must reach
/// the caller intact — `SafeDiagnostic` is already the sanctioned disclosure
/// surface, so carrying it here discloses nothing new.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationError {
    Unavailable,
    /// No daemon accepted the connection after the transport's restart grace,
    /// so the request was never sent. Distinct from [`Self::Unavailable`]
    /// because re-dispatching in-process cannot succeed until a daemon is
    /// back: dispatchers fail fast with the typed connect diagnostic instead
    /// of retrying to their deadline.
    Unreachable {
        reason_code: String,
        detail: String,
    },
    Denied,
    Cancelled,
    DeadlineExceeded,
    InvalidRequest,
    Conflict,
    /// The daemon's own typed problem, carried whole so surface adapters
    /// republish the authoritative diagnostic (e.g. `configuration.conflict`)
    /// instead of fabricating a generic one.
    Problem(Box<ApplicationProblem>),
}

impl From<ApplicationContractError> for InvocationError {
    fn from(_error: ApplicationContractError) -> Self {
        Self::InvalidRequest
    }
}

impl From<ApplicationProblem> for InvocationError {
    fn from(problem: ApplicationProblem) -> Self {
        Self::Problem(Box::new(problem))
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
