//! Daemon-mountable owner for the four feedback reads.
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
    FeedbackExpandResultV1, FeedbackGetRequestV1, FeedbackGetResultV1, FeedbackHandleRequestV1,
    FeedbackListRequestV1, FeedbackListResultV1, FeedbackReadPort, FeedbackReadPortContext,
    FeedbackReadPortFuture, FeedbackReadService, FeedbackRouteAuthorizationPort,
    feedback_read_operations,
};
use tracedecay_application::{
    ApplicationContractError, ApplicationEnvelope, ApplicationOutcome, ApplicationResult,
    CancellationContext, Deadline, EvidencePacket, RequestContext,
};
use tracedecay_domain::{
    FeedbackContentIdentityV1, FeedbackCycleId, FeedbackImpactStateV1, FeedbackImpactV1,
    FeedbackResultId, FeedbackScopeV1, FeedbackTargetV1, RetrievalAnchorId, SymbolOccurrenceId,
    UtcMicros,
};

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
    Impact(ApplicationResult<CanonicalFeedbackImpactProjectionV1>),
    AffectedTests(ApplicationResult<CanonicalAffectedTestsProjectionV1>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackCanonicalProjectionKindV1 {
    Impact,
    AffectedTests,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalFeedbackImpactProjectionV1 {
    pub result_id: FeedbackResultId,
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    pub content_identity: Option<FeedbackContentIdentityV1>,
    pub impact: Option<FeedbackImpactV1>,
    pub state: Option<FeedbackImpactStateV1>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalAffectedTestsProjectionV1 {
    pub result_id: FeedbackResultId,
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    pub content_identity: Option<FeedbackContentIdentityV1>,
    pub target: Option<FeedbackTargetV1>,
    pub affected_tests: Vec<SymbolOccurrenceId>,
    pub evidence_anchors: Vec<RetrievalAnchorId>,
    pub state: Option<FeedbackImpactStateV1>,
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackReadOwnerErrorV1 {
    NotFoundOrNotAuthorized,
    Unavailable,
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

#[cfg(test)]
mod tests {
    use tracedecay_application::feedback::FeedbackDiagnosticsReadResultV1;
    use tracedecay_domain::{
        CommitId, FeedbackCycleId, FeedbackCycleResultV1, FeedbackCycleTerminationV1,
        FeedbackDurabilityV1, FeedbackImpactStateV1, FeedbackResultId, FeedbackScopeV1,
        ManifestDigest, ProjectId, RepositoryId, WorktreeId,
    };

    use super::{FeedbackCanonicalProjectionKindV1, FeedbackCanonicalProjectionResultV1};

    #[test]
    fn canonical_feedback_projections_preserve_cycle_identity_and_state() {
        let scope = FeedbackScopeV1 {
            project_id: ProjectId::new("project.feedback-projection").expect("project"),
            repository_id: RepositoryId::new("repository.feedback-projection").expect("repository"),
            worktree_id: WorktreeId::new("worktree.feedback-projection").expect("worktree"),
            branch_ref: "refs/heads/main".to_owned(),
            head_commit_id: CommitId::new("commit.feedback-projection").expect("commit"),
        };
        let result_id =
            FeedbackResultId::new("result.feedback-projection").expect("feedback result");
        let cycle_id = FeedbackCycleId::new("cycle.feedback-projection").expect("feedback cycle");
        let diagnostics = || FeedbackDiagnosticsReadResultV1 {
            cycle: FeedbackCycleResultV1 {
                result_id: result_id.clone(),
                cycle_id: cycle_id.clone(),
                scope: scope.clone(),
                content_identity: None,
                durability: FeedbackDurabilityV1::Durable,
                policy_digest: digest('a'),
                configuration_digest: digest('b'),
                termination: FeedbackCycleTerminationV1::IncompleteCoverage,
                provider_states: Vec::new(),
                baseline_states: Vec::new(),
                impact: None,
                impact_state: Some(FeedbackImpactStateV1::Unavailable),
                affected_tests_state: Some(FeedbackImpactStateV1::Stale),
                findings: Vec::new(),
                total_findings: 0,
                returned_findings: 0,
                omitted_findings: 0,
                advisory_only: true,
            },
        };

        let FeedbackCanonicalProjectionResultV1::Impact(impact) =
            FeedbackCanonicalProjectionKindV1::Impact.project(diagnostics())
        else {
            panic!("impact projection kind");
        };
        assert_eq!(impact.result_id, result_id);
        assert_eq!(impact.cycle_id, cycle_id);
        assert_eq!(impact.scope, scope);
        assert!(impact.content_identity.is_none());
        assert_eq!(impact.state, Some(FeedbackImpactStateV1::Unavailable));

        let FeedbackCanonicalProjectionResultV1::AffectedTests(affected) =
            FeedbackCanonicalProjectionKindV1::AffectedTests.project(diagnostics())
        else {
            panic!("affected-tests projection kind");
        };
        assert_eq!(affected.result_id, result_id);
        assert_eq!(affected.cycle_id, cycle_id);
        assert_eq!(affected.scope, scope);
        assert!(affected.content_identity.is_none());
        assert!(affected.target.is_none());
        assert!(affected.affected_tests.is_empty());
        assert!(affected.evidence_anchors.is_empty());
        assert_eq!(affected.state, Some(FeedbackImpactStateV1::Stale));
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }
}
