//! Daemon-mountable owner for the four feedback reads.
//!
//! The owner resolves an opaque request handle through daemon authority, then
//! delegates to the transport-neutral application service. Durable
//! publications, authenticated cursor handling, and anchor hydration remain in
//! the injected store; this module creates no parallel feedback state.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_contracts::feedback::{
    FeedbackDiagnosticsReadRequestV1, FeedbackDiagnosticsReadResultV1, FeedbackExpandRequestV1,
    FeedbackExpandResultV1, FeedbackGetRequestV1, FeedbackGetResultV1, FeedbackHandleRequestV1,
    FeedbackListRequestV1, FeedbackListResultV1, FeedbackReadPort, FeedbackReadPortContext,
    FeedbackReadPortFuture, FeedbackReadService, FeedbackRouteAuthorizationPort,
    feedback_read_operations,
};
use tracedecay_contracts::{
    ApplicationContractError, ApplicationEnvelope, ApplicationOutcome, ApplicationResult,
    CancellationContext, Deadline, EvidencePacket, RequestContext,
};
use tracedecay_domain::UtcMicros;

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

/// Daemon-owned registry for opaque feedback request handles. Implementations must
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
/// publication ledger and anchor owner.
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
    Impact(ApplicationResult<CanonicalFeedbackImpactProjectionV1>),
    AffectedTests(ApplicationResult<CanonicalAffectedTestsProjectionV1>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackCanonicalProjectionKindV1 {
    Impact,
    AffectedTests,
}

// The canonical projection wire types live at the application boundary
// (`tracedecay_contracts::feedback`) so the catalog contribution can
// register their schema bodies; this owner re-exports them for its callers.
pub use tracedecay_contracts::feedback::{
    CanonicalAffectedTestsProjectionV1, CanonicalFeedbackImpactProjectionV1,
};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "projection", content = "result")]
pub enum FeedbackCanonicalProjectionResultV1 {
    Impact(CanonicalFeedbackImpactProjectionV1),
    AffectedTests(CanonicalAffectedTestsProjectionV1),
}

impl FeedbackCanonicalProjectionKindV1 {
    pub fn project(
        self,
        diagnostics: FeedbackDiagnosticsReadResultV1,
    ) -> FeedbackCanonicalProjectionResultV1 {
        let cycle = diagnostics.cycle;
        match self {
            Self::Impact => {
                FeedbackCanonicalProjectionResultV1::Impact(CanonicalFeedbackImpactProjectionV1 {
                    result_id: cycle.result_id,
                    cycle_id: cycle.cycle_id,
                    scope: cycle.scope,
                    content_identity: cycle.content_identity,
                    impact: cycle.impact,
                    state: cycle.impact_state,
                })
            }
            Self::AffectedTests => {
                let (target, affected_tests, evidence_anchors) = cycle.impact.map_or_else(
                    || (None, Vec::new(), Vec::new()),
                    |impact| {
                        (
                            Some(impact.target),
                            impact.affected_tests,
                            impact.evidence_anchors,
                        )
                    },
                );
                FeedbackCanonicalProjectionResultV1::AffectedTests(
                    CanonicalAffectedTestsProjectionV1 {
                        result_id: cycle.result_id,
                        cycle_id: cycle.cycle_id,
                        scope: cycle.scope,
                        content_identity: cycle.content_identity,
                        target,
                        affected_tests,
                        evidence_anchors,
                        state: cycle.affected_tests_state,
                    },
                )
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedbackReadOwnerErrorV1 {
    NotFoundOrNotAuthorized,
    Unavailable,
    Contract(ApplicationContractError),
}

/// Complete daemon-mountable feedback owner: opaque request authority plus the
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

    pub async fn invoke_projection_with_controls(
        &self,
        projection: FeedbackCanonicalProjectionKindV1,
        request_handle: &str,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Result<FeedbackReadInvocationResultV1, FeedbackReadOwnerErrorV1> {
        let diagnostics = self
            .invoke_resolved(
                FeedbackReadOperationV1::Diagnostics,
                request_handle,
                observed_at,
                Some((deadline, cancellation)),
            )
            .await?;
        let FeedbackReadInvocationResultV1::Diagnostics(result) = diagnostics else {
            unreachable!("diagnostics authority returned a non-diagnostics result");
        };
        Ok(match projection {
            FeedbackCanonicalProjectionKindV1::Impact => FeedbackReadInvocationResultV1::Impact(
                project_feedback_evidence(result, |diagnostics| {
                    let FeedbackCanonicalProjectionResultV1::Impact(projected) =
                        projection.project(diagnostics)
                    else {
                        unreachable!("impact projection returned affected tests");
                    };
                    projected
                }),
            ),
            FeedbackCanonicalProjectionKindV1::AffectedTests => {
                FeedbackReadInvocationResultV1::AffectedTests(project_feedback_evidence(
                    result,
                    |diagnostics| {
                        let FeedbackCanonicalProjectionResultV1::AffectedTests(projected) =
                            projection.project(diagnostics)
                        else {
                            unreachable!("affected-tests projection returned impact");
                        };
                        projected
                    },
                ))
            }
        })
    }

    #[hotpath::measure(label = "usecases.feedback.invoke", future = true)]
    async fn invoke_resolved(
        &self,
        operation: FeedbackReadOperationV1,
        request_handle: &str,
        observed_at: UtcMicros,
        controls: Option<(Deadline, CancellationContext)>,
    ) -> Result<FeedbackReadInvocationResultV1, FeedbackReadOwnerErrorV1> {
        if FeedbackHandleRequestV1::new(request_handle).is_err() {
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
        match authorized.request {
            FeedbackReadRequestV1::Diagnostics(request) => {
                Ok(FeedbackReadInvocationResultV1::Diagnostics(
                    self.service
                        .diagnostics(&context, request, observed_at)
                        .await
                        .map_err(FeedbackReadOwnerErrorV1::Contract)?,
                ))
            }
            FeedbackReadRequestV1::Get(request) => Ok(FeedbackReadInvocationResultV1::Get(
                self.service
                    .get(&context, request, observed_at)
                    .await
                    .map_err(FeedbackReadOwnerErrorV1::Contract)?,
            )),
            FeedbackReadRequestV1::Expand(request) => Ok(FeedbackReadInvocationResultV1::Expand(
                self.service
                    .expand(&context, request, observed_at)
                    .await
                    .map_err(FeedbackReadOwnerErrorV1::Contract)?,
            )),
            FeedbackReadRequestV1::List(request) => Ok(FeedbackReadInvocationResultV1::List(
                self.service
                    .list(&context, request, observed_at)
                    .await
                    .map_err(FeedbackReadOwnerErrorV1::Contract)?,
            )),
        }
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

fn project_feedback_evidence<T>(
    result: ApplicationResult<FeedbackDiagnosticsReadResultV1>,
    project: impl FnOnce(FeedbackDiagnosticsReadResultV1) -> T,
) -> ApplicationResult<T> {
    result.map(|envelope| {
        let ApplicationEnvelope {
            contract,
            request_id,
            scope,
            outcome,
        } = envelope;
        let ApplicationOutcome::Evidence(packet) = outcome else {
            unreachable!("feedback reads return evidence outcomes");
        };
        let EvidencePacket {
            temporal,
            authority,
            evidence_authorities,
            coverage,
            omissions,
            scores,
            contributions,
            page,
            execution,
            payload,
        } = packet;
        ApplicationEnvelope::evidence(
            contract,
            request_id,
            scope,
            EvidencePacket {
                temporal,
                authority,
                evidence_authorities,
                coverage,
                omissions,
                scores,
                contributions,
                page,
                execution,
                payload: payload.map(project),
            },
        )
    })
}
