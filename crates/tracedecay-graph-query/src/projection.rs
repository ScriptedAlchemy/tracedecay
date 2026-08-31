//! Exact-scope admission for verified code-graph projection reads.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use thiserror::Error;
use tracedecay_application::{
    ApplicationOperation, CancellationSignal, Deadline, RequestAdmission, RequestContext,
    RequestId, ResolvedScope,
};
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphProjectionError, CodeGraphProjectionStore,
};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_domain::{CodeGenerationId, UtcMicros};
use tracedecay_graph_db::GraphCancellation;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CodeGraphReadError {
    #[error("the exact project code-graph registry is missing")]
    MissingRegistry,
    #[error("the exact project code graph is unavailable: {detail}")]
    Unavailable { detail: String },
    #[error("the exact project code graph requires reset: {detail}")]
    ResetRequired { detail: String },
    #[error("the requested code-graph generation is stale: {detail}")]
    Stale { detail: String },
    #[error("the code-graph read was cancelled")]
    Cancelled,
    #[error("the code-graph read timed out")]
    TimedOut,
    #[error("the code-graph read exceeded its budget: {detail}")]
    BudgetExhausted { detail: String },
    #[error("the code-graph read is not authorized")]
    Denied,
    #[error("the code-graph read request is invalid: {detail}")]
    InvalidRequest { detail: String },
    #[error("the verified code-graph projection is corrupt: {detail}")]
    Corrupt { detail: String },
}

#[derive(Clone)]
pub struct CodeGraphReadRequest<'a> {
    pub context: &'a RequestContext,
    pub observed_at: UtcMicros,
    pub cancellation: Arc<dyn GraphCancellation>,
    pub deadline: Option<Deadline>,
    pub live_cancellation: Option<&'a CancellationSignal>,
}

impl<'a> CodeGraphReadRequest<'a> {
    pub fn new(
        context: &'a RequestContext,
        observed_at: UtcMicros,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Self {
        Self {
            context,
            observed_at,
            cancellation,
            deadline: None,
            live_cancellation: None,
        }
    }

    pub fn with_deadline(mut self, deadline: Deadline) -> Self {
        self.deadline = Some(deadline);
        self
    }

    pub fn with_live_cancellation(mut self, cancellation: &'a CancellationSignal) -> Self {
        self.live_cancellation = Some(cancellation);
        self
    }

    pub fn from_context(context: &'a RequestContext, observed_at: UtcMicros) -> Self {
        Self::new(context, observed_at, request_graph_cancellation(context))
    }
}

pub type CodeGraphReadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<VerifiedCodeGraphRead, CodeGraphReadError>> + Send + 'a>>;

pub trait CodeGraphProjectionReadPort: Send + Sync {
    fn open<'a>(&'a self, request: CodeGraphReadRequest<'a>) -> CodeGraphReadFuture<'a>;
}

#[derive(Clone)]
pub struct CodeGraphReadAdmissionRequest<'a> {
    pub operation: &'a ApplicationOperation,
    pub request_id: RequestId,
    pub deadline: Deadline,
    pub cancellation: &'a CancellationSignal,
    pub observed_at: UtcMicros,
}

impl<'a> CodeGraphReadAdmissionRequest<'a> {
    pub fn new(
        operation: &'a ApplicationOperation,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: &'a CancellationSignal,
        observed_at: UtcMicros,
    ) -> Self {
        Self {
            operation,
            request_id,
            deadline,
            cancellation,
            observed_at,
        }
    }
}

pub type CodeGraphReadAdmissionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<RequestContext, CodeGraphReadError>> + Send + 'a>>;

/// Canonical admission boundary shared by every code-graph transport.
/// Implementations retain exact project/source authorization and build the
/// request context from the supplied operation and live transport controls.
pub trait CodeGraphReadAdmissionPort: Send + Sync {
    fn admit<'a>(
        &'a self,
        request: CodeGraphReadAdmissionRequest<'a>,
    ) -> CodeGraphReadAdmissionFuture<'a>;
}

impl<T> CodeGraphReadAdmissionPort for Arc<T>
where
    T: CodeGraphReadAdmissionPort + ?Sized,
{
    fn admit<'a>(
        &'a self,
        request: CodeGraphReadAdmissionRequest<'a>,
    ) -> CodeGraphReadAdmissionFuture<'a> {
        (**self).admit(request)
    }
}

impl<T> CodeGraphProjectionReadPort for Arc<T>
where
    T: CodeGraphProjectionReadPort + ?Sized,
{
    fn open<'a>(&'a self, request: CodeGraphReadRequest<'a>) -> CodeGraphReadFuture<'a> {
        (**self).open(request)
    }
}

/// Freshness of the generation a verified graph read serves.
///
/// Mirrors the search lane's `served_stale` contract: the serving arm of
/// query admission answers from the last complete seated generation while a
/// rebuild is in flight, and the answer must say so rather than present
/// itself as current.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeGraphReadFreshnessV1 {
    /// The ready gate proved the served generation current against the live
    /// checkout when the read opened.
    Current,
    /// The last complete seated generation answered because no proven-current
    /// generation was admissible (the scheduler owns a rebuild pass or the
    /// checkout drifted past the currency witness). Recall is sound for the
    /// served generation; freshness is not. The payload carries the evidence
    /// a caller needs to say how stale and whether a remedy is in motion: a
    /// seat sealed days ago with no rebuild pass in flight is a wedged route,
    /// not a routine rebuild window.
    LastCompleteStale {
        /// When the served generation was durably sealed.
        sealed_at: UtcMicros,
        /// Whether a reconcile pass or pending scheduler wake existed for the
        /// route when the read opened.
        rebuild_in_flight: bool,
    },
}

impl CodeGraphReadFreshnessV1 {
    pub fn is_stale(self) -> bool {
        match self {
            Self::Current => false,
            Self::LastCompleteStale { .. } => true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedCodeGraphRead {
    scope: ResolvedScope,
    store: Arc<CodeGraphProjectionStore>,
    freshness: CodeGraphReadFreshnessV1,
}

impl VerifiedCodeGraphRead {
    pub fn new(
        scope: ResolvedScope,
        store: Arc<CodeGraphProjectionStore>,
        freshness: CodeGraphReadFreshnessV1,
    ) -> Result<Self, CodeGraphReadError> {
        scope
            .validate()
            .map_err(|error| CodeGraphReadError::InvalidRequest {
                detail: error.to_string(),
            })?;
        Ok(Self {
            scope,
            store,
            freshness,
        })
    }

    pub fn scope(&self) -> &ResolvedScope {
        &self.scope
    }

    pub fn freshness(&self) -> CodeGraphReadFreshnessV1 {
        self.freshness
    }

    pub fn generation(&self) -> &CodeGenerationId {
        self.store.generation()
    }

    pub fn store(&self) -> &Arc<CodeGraphProjectionStore> {
        &self.store
    }

    pub fn reader(
        &self,
        context: &RequestContext,
        observed_at: UtcMicros,
    ) -> Result<CodeGraphInteractiveReader, CodeGraphReadError> {
        self.reader_with_cancellation(context, observed_at, request_graph_cancellation(context))
    }

    #[hotpath::measure(label = "usecases.graph.reader")]
    pub fn reader_with_cancellation(
        &self,
        context: &RequestContext,
        observed_at: UtcMicros,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphInteractiveReader, CodeGraphReadError> {
        context.validate().map_err(|_| CodeGraphReadError::Denied)?;
        // Checkout identity, not label equality: this read may be bound to
        // the scope retained at project open while the request scope was
        // resolved against live HEAD; a branch-label move between the two
        // must not deny the exact checkout its own graph.
        if !context.scope().identifies_same_checkout(&self.scope) {
            return Err(CodeGraphReadError::Denied);
        }
        match context.admission_at(observed_at) {
            RequestAdmission::Admitted => {}
            RequestAdmission::Cancelled => return Err(CodeGraphReadError::Cancelled),
            RequestAdmission::TimedOut => return Err(CodeGraphReadError::TimedOut),
        }
        self.store
            .interactive_reader_with_cancellation(self.store.generation(), cancellation)
            .map_err(map_projection_error)
    }
}

struct RequestGraphCancellation {
    cancelled: bool,
}

struct LiveApplicationGraphCancellation(CancellationSignal);

impl GraphCancellation for LiveApplicationGraphCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

pub fn application_graph_cancellation(
    cancellation: &CancellationSignal,
) -> Arc<dyn GraphCancellation> {
    Arc::new(LiveApplicationGraphCancellation(cancellation.clone()))
}

impl GraphCancellation for RequestGraphCancellation {
    fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

pub fn request_graph_cancellation(context: &RequestContext) -> Arc<dyn GraphCancellation> {
    Arc::new(RequestGraphCancellation {
        cancelled: context.cancellation().is_cancelled(),
    })
}

pub fn map_projection_error(error: CodeGraphProjectionError) -> CodeGraphReadError {
    match error {
        CodeGraphProjectionError::Cancelled => CodeGraphReadError::Cancelled,
        CodeGraphProjectionError::DeadlineExceeded => CodeGraphReadError::TimedOut,
        CodeGraphProjectionError::Contract(detail) => CodeGraphReadError::InvalidRequest { detail },
        stale @ (CodeGraphProjectionError::GenerationMismatch
        | CodeGraphProjectionError::RecoveredGenerationMismatch { .. }) => {
            CodeGraphReadError::Stale {
                detail: stale.to_string(),
            }
        }
        CodeGraphProjectionError::ResetRequired(detail) => {
            CodeGraphReadError::ResetRequired { detail }
        }
        corrupt @ (CodeGraphProjectionError::ProjectionMismatch { .. }
        | CodeGraphProjectionError::Corrupt(_)) => CodeGraphReadError::Corrupt {
            detail: corrupt.to_string(),
        },
        CodeGraphProjectionError::Unavailable(detail) => CodeGraphReadError::Unavailable { detail },
        budget @ CodeGraphProjectionError::BudgetExhausted { .. } => {
            CodeGraphReadError::BudgetExhausted {
                detail: budget.to_string(),
            }
        }
        unavailable @ (CodeGraphProjectionError::Conflict { .. }
        | CodeGraphProjectionError::DurabilityUncertain(_)
        | CodeGraphProjectionError::Closed) => CodeGraphReadError::Unavailable {
            detail: unavailable.to_string(),
        },
    }
}

pub fn map_code_graph_read_runtime_error(error: CodeGraphReadError) -> TraceDecayError {
    match error {
        CodeGraphReadError::ResetRequired { detail } => TraceDecayError::ResetRequired {
            authority: "verified code graph".to_owned(),
            reason: detail,
        },
        error => TraceDecayError::ProjectRoute {
            reason_code: match &error {
                CodeGraphReadError::MissingRegistry => "code-graph-registry-missing",
                CodeGraphReadError::Unavailable { .. } => "code-graph-unavailable",
                CodeGraphReadError::Stale { .. } => "code-graph-stale",
                CodeGraphReadError::Cancelled => "code-graph-cancelled",
                CodeGraphReadError::TimedOut => "code-graph-timed-out",
                CodeGraphReadError::BudgetExhausted { .. } => "code-graph-budget-exhausted",
                CodeGraphReadError::Denied => "code-graph-denied",
                CodeGraphReadError::InvalidRequest { .. } => "code-graph-invalid-request",
                CodeGraphReadError::Corrupt { .. } => "code-graph-corrupt",
                CodeGraphReadError::ResetRequired { .. } => "code-graph-reset-required",
            }
            .to_owned(),
            retryable: matches!(
                &error,
                CodeGraphReadError::MissingRegistry
                    | CodeGraphReadError::Unavailable { .. }
                    | CodeGraphReadError::Stale { .. }
                    | CodeGraphReadError::TimedOut
            ),
            detail: error.to_string(),
        },
    }
}
