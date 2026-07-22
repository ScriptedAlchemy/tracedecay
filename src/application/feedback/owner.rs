//! Daemon-mountable owner for the four PR12 feedback reads.
//!
//! The owner resolves an opaque request handle through daemon authority, then
//! delegates to the transport-neutral application service. Durable
//! publications, authenticated cursor handling, and anchor hydration remain in
//! the injected store; this module creates no parallel feedback state.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_application::feedback::{
    FeedbackDiagnosticsReadRequestV1, FeedbackDiagnosticsReadResultV1, FeedbackExpandRequestV1,
    FeedbackExpandResultV1, FeedbackGetRequestV1, FeedbackGetResultV1, FeedbackListRequestV1,
    FeedbackListResultV1, FeedbackReadPort, FeedbackReadPortContext, FeedbackReadPortFuture,
    FeedbackReadService, FeedbackRouteAuthorizationPort, feedback_read_operations,
};
use tracedecay_application::{
    ApplicationContractError, ApplicationResult, CancellationContext, Deadline, RequestContext,
};
use tracedecay_domain::UtcMicros;

const MAX_FEEDBACK_REQUEST_HANDLE_BYTES: usize = 256;

/// Closed operation set used by central daemon invocation integration.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackReadOperationV1 {
    Diagnostics,
    Get,
    Expand,
    List,
}

/// Typed request resolved from a daemon-issued opaque request handle.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub enum FeedbackReadRequestV1 {
    Diagnostics(FeedbackDiagnosticsReadRequestV1),
    Get(FeedbackGetRequestV1),
    Expand(FeedbackExpandRequestV1),
    List(FeedbackListRequestV1),
}

impl FeedbackReadRequestV1 {
    pub const fn operation(&self) -> FeedbackReadOperationV1 {
        match self {
            Self::Diagnostics(_) => FeedbackReadOperationV1::Diagnostics,
            Self::Get(_) => FeedbackReadOperationV1::Get,
            Self::Expand(_) => FeedbackReadOperationV1::Expand,
            Self::List(_) => FeedbackReadOperationV1::List,
        }
    }
}

/// All authority needed by the application read is supplied by the daemon
/// request-handle owner. No actor, scope, grant, deadline, or cancellation
/// input is decoded from the client-provided handle.
pub struct AuthorizedFeedbackReadRequestV1 {
    pub context: RequestContext,
    pub request: FeedbackReadRequestV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackReadRequestResolutionV1 {
    NotFoundOrNotAuthorized,
    Unavailable,
}

pub type FeedbackReadRequestAuthorityFuture<'a> = Pin<
    Box<
        dyn Future<
                Output = Result<AuthorizedFeedbackReadRequestV1, FeedbackReadRequestResolutionV1>,
            > + Send
            + 'a,
    >,
>;

/// Daemon-owned registry for opaque PR12 request handles. Implementations must
/// mint handles server-side, bind them to one operation and exact authorized
/// scope, enforce expiry and one-shot/reuse policy, and return the same
/// non-disclosing outcome for unknown, expired, cross-operation, and
/// cross-scope handles.
pub trait FeedbackReadRequestAuthority: Send + Sync {
    fn resolve<'a>(
        &'a self,
        operation: FeedbackReadOperationV1,
        request_handle: &'a str,
        observed_at: UtcMicros,
    ) -> FeedbackReadRequestAuthorityFuture<'a>;
}

/// Physical durable-read boundary implemented by the daemon's canonical
/// completed-publication ledger and anchor owner.
///
/// `list` owns stable finding-id ordering and authenticated cursor validation.
/// `expand` accepts only the exact `RetrievalAnchorId` resolved by the
/// server-owned request authority, then hydrates through the canonical anchor
/// owner.
pub trait DurableFeedbackReadStoreV1: Send + Sync {
    fn diagnostics<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackDiagnosticsReadRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackDiagnosticsReadResultV1>;

    fn get<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackGetRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackGetResultV1>;

    fn expand<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackExpandRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackExpandResultV1>;

    fn list<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackListRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackListResultV1>;
}

/// Concrete application-port owner over the durable feedback store.
pub struct CanonicalFeedbackReadOwnerV1<S> {
    store: S,
}

impl<S> CanonicalFeedbackReadOwnerV1<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S> FeedbackReadPort for CanonicalFeedbackReadOwnerV1<S>
where
    S: DurableFeedbackReadStoreV1,
{
    fn diagnostics<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackDiagnosticsReadRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackDiagnosticsReadResultV1> {
        self.store.diagnostics(context, request)
    }

    fn get<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackGetRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackGetResultV1> {
        self.store.get(context, request)
    }

    fn expand<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackExpandRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackExpandResultV1> {
        self.store.expand(context, request)
    }

    fn list<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackListRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackListResultV1> {
        self.store.list(context, request)
    }
}

/// Typed result retained until the central invocation layer serializes the
/// operation-specific canonical application envelope.
pub enum FeedbackReadInvocationResultV1 {
    Diagnostics(ApplicationResult<FeedbackDiagnosticsReadResultV1>),
    Get(ApplicationResult<FeedbackGetResultV1>),
    Expand(ApplicationResult<FeedbackExpandResultV1>),
    List(ApplicationResult<FeedbackListResultV1>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackReadOwnerErrorV1 {
    NotFoundOrNotAuthorized,
    Unavailable,
}

/// Complete daemon-mountable PR12 owner: opaque request authority plus the
/// canonical authorized application service.
pub struct DaemonFeedbackReadOwnerV1<R, P, A> {
    requests: R,
    service: FeedbackReadService<P, A>,
}

impl<R, P, A> DaemonFeedbackReadOwnerV1<R, P, A>
where
    R: FeedbackReadRequestAuthority,
    P: FeedbackReadPort,
    A: FeedbackRouteAuthorizationPort,
{
    pub fn new(requests: R, service: FeedbackReadService<P, A>) -> Self {
        Self { requests, service }
    }

    pub async fn invoke(
        &self,
        operation: FeedbackReadOperationV1,
        request_handle: &str,
        observed_at: UtcMicros,
    ) -> Result<FeedbackReadInvocationResultV1, FeedbackReadOwnerErrorV1> {
        self.invoke_resolved(operation, request_handle, observed_at, None)
            .await
    }

    pub async fn invoke_with_controls(
        &self,
        operation: FeedbackReadOperationV1,
        request_handle: &str,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Result<FeedbackReadInvocationResultV1, FeedbackReadOwnerErrorV1> {
        self.invoke_resolved(
            operation,
            request_handle,
            observed_at,
            Some((deadline, cancellation)),
        )
        .await
    }

    async fn invoke_resolved(
        &self,
        operation: FeedbackReadOperationV1,
        request_handle: &str,
        observed_at: UtcMicros,
        controls: Option<(Deadline, CancellationContext)>,
    ) -> Result<FeedbackReadInvocationResultV1, FeedbackReadOwnerErrorV1> {
        if !valid_request_handle(request_handle) {
            return Err(FeedbackReadOwnerErrorV1::NotFoundOrNotAuthorized);
        }
        let authorized = self
            .requests
            .resolve(operation, request_handle, observed_at)
            .await
            .map_err(|resolution| match resolution {
                FeedbackReadRequestResolutionV1::NotFoundOrNotAuthorized => {
                    FeedbackReadOwnerErrorV1::NotFoundOrNotAuthorized
                }
                FeedbackReadRequestResolutionV1::Unavailable => {
                    FeedbackReadOwnerErrorV1::Unavailable
                }
            })?;
        if authorized.request.operation() != operation {
            return Err(FeedbackReadOwnerErrorV1::NotFoundOrNotAuthorized);
        }
        let context = match controls {
            Some((deadline, cancellation)) => {
                let deadline = if deadline.expires_at < authorized.context.deadline().expires_at {
                    deadline
                } else {
                    authorized.context.deadline().clone()
                };
                authorized
                    .context
                    .with_deadline(deadline)
                    .with_cancellation(cancellation)
            }
            None => authorized.context,
        };
        Ok(match authorized.request {
            FeedbackReadRequestV1::Diagnostics(request) => {
                FeedbackReadInvocationResultV1::Diagnostics(
                    self.service
                        .diagnostics(&context, request, observed_at)
                        .await,
                )
            }
            FeedbackReadRequestV1::Get(request) => FeedbackReadInvocationResultV1::Get(
                self.service.get(&context, request, observed_at).await,
            ),
            FeedbackReadRequestV1::Expand(request) => FeedbackReadInvocationResultV1::Expand(
                self.service.expand(&context, request, observed_at).await,
            ),
            FeedbackReadRequestV1::List(request) => FeedbackReadInvocationResultV1::List(
                self.service.list(&context, request, observed_at).await,
            ),
        })
    }
}

impl<R, S, A> DaemonFeedbackReadOwnerV1<R, CanonicalFeedbackReadOwnerV1<S>, A>
where
    R: FeedbackReadRequestAuthority,
    S: DurableFeedbackReadStoreV1,
    A: FeedbackRouteAuthorizationPort,
{
    /// Mounts the canonical durable store with the exact callable catalog
    /// operations. This is the concise constructor used by central daemon
    /// integration.
    pub fn from_store(
        requests: R,
        store: S,
        authorization: A,
    ) -> Result<Self, ApplicationContractError> {
        let service = FeedbackReadService::new(
            CanonicalFeedbackReadOwnerV1::new(store),
            authorization,
            feedback_read_operations()?,
        );
        Ok(Self::new(requests, service))
    }
}

fn valid_request_handle(handle: &str) -> bool {
    !handle.is_empty()
        && handle.trim() == handle
        && handle.len() <= MAX_FEEDBACK_REQUEST_HANDLE_BYTES
        && !handle.chars().any(char::is_control)
}
