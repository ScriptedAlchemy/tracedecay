//! Application-admitted retrieval over one exact mounted session root.

use std::future::Future;
use std::pin::Pin;

use sha2::{Digest, Sha256};
use tracedecay_application::{CancellationSignal, RequestContext, ResolvedScope};
use tracedecay_domain::{
    ComponentRevision, EphemeralSanitizedQueryViewV1, RetrievalRequest, ScoreDomainId,
};
use tracedecay_query::retrieval::evidence_lanes::TaskSessionBindingV1;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_session_memory::context::{
    CapabilityDigest, ConfigurationDigest, PolicyDigest, RequestBudgets, ResolvedSessionIdentity,
};
use tracedecay_session_memory::session::{
    SessionRequestBinding, SessionRetrievalConfiguration, SessionTemporalQuery,
    TaskSessionRetrievalOutcomeV1,
};
use tracedecay_session_temporal_store::execution::TaskSessionRankSelectorV1;
use tracedecay_store::StoreShardScopeV1;

use super::contract::{
    LcmDescribeServiceCommand, LcmDescribeServiceFuture, LcmDescribeServiceOutcome,
    LcmExpandServiceCommand, LcmExpandServiceFuture, LcmExpandServiceOutcome,
    SessionRetrievalServiceOutcome, SessionRetrievalStoreScope, SessionRetrievalUnavailable,
    SessionRetrievalUnavailableReason,
};
use super::{
    DaemonSessionRetrievalRoot, DaemonSessionRetrievalService, message_search_digest,
    requires_refresh_worker,
};

use super::APPLICATION_RETRIEVAL_MAX_BYTES;

const APPLICATION_RETRIEVAL_MAX_RESULTS: u64 = 100;
const APPLICATION_RETRIEVAL_MAX_WORK_UNITS: u64 = 100_000;

impl DaemonSessionRetrievalService {
    /// Mount the canonical profile-session retrieval service over the exact
    /// registered profile shard and retained session identity supplied by the
    /// daemon composition root.
    pub fn new_admitted_profile(
        database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        identity: ResolvedSessionIdentity,
    ) -> Option<Self> {
        if identity.project_id().is_some()
            || database.binding().shard_id.profile_id.as_str() != identity.profile_id().as_str()
            || !matches!(
                &database.binding().shard_id.scope,
                StoreShardScopeV1::ProfileSessions
            )
        {
            return None;
        }
        let expected_runtime_shard = database.binding().shard_id.clone();
        Self::new(
            database,
            DaemonSessionRetrievalRoot {
                store_scope: SessionRetrievalStoreScope::Profile,
                identity,
                project_id: None,
                authorized_root: None,
                expected_runtime_shard: Some(expected_runtime_shard),
            },
            None,
        )
    }
}

pub type SessionApplicationRetrievalFutureV1<'a> =
    Pin<Box<dyn Future<Output = SessionRetrievalServiceOutcome> + Send + 'a>>;

pub(crate) type TaskSessionApplicationRetrievalFutureV1<'a> =
    Pin<Box<dyn Future<Output = TaskSessionRetrievalOutcomeV1> + Send + 'a>>;

/// Application-level retrieval over one already-mounted session root.
///
/// The caller has already crossed its application admission boundary. This
/// port therefore accepts the original immutable context and never routes
/// through MCP command parsing or mints a replacement application identity.
pub trait SessionApplicationRetrievalPortV1: Send + Sync {
    fn retrieve_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        query: SessionTemporalQuery,
    ) -> SessionApplicationRetrievalFutureV1<'a>;

    #[allow(clippy::too_many_arguments)]
    fn retrieve_task_session_admitted<'a>(
        &'a self,
        _context: &'a RequestContext,
        _temporal_query: SessionTemporalQuery,
        _task_binding: TaskSessionBindingV1,
        _retrieval_request: RetrievalRequest,
        _query: EphemeralSanitizedQueryViewV1,
        _retriever_revision: ComponentRevision,
        _score_domain: ScoreDomainId,
        _policy_revision: ComponentRevision,
        _selector: &'a dyn TaskSessionRankSelectorV1,
    ) -> TaskSessionApplicationRetrievalFutureV1<'a> {
        Box::pin(async { TaskSessionRetrievalOutcomeV1::Unavailable })
    }

    fn retrieve_admitted_with_cancellation<'a>(
        &'a self,
        context: &'a RequestContext,
        cancellation: &'a CancellationSignal,
        query: SessionTemporalQuery,
    ) -> SessionApplicationRetrievalFutureV1<'a> {
        Box::pin(hotpath::future!(
            async move {
                if cancellation.context().token_id != context.cancellation().token_id {
                    return SessionRetrievalServiceOutcome::Denied;
                }
                if cancellation.is_cancelled() {
                    return SessionRetrievalServiceOutcome::Cancelled;
                }
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => {
                        hotpath::gauge!("daemon.session_retrieval.cancelled").inc(1.0);
                        SessionRetrievalServiceOutcome::Cancelled
                    }
                    outcome = self.retrieve_admitted(context, query) => outcome,
                }
            },
            label = "daemon.session_retrieval.retrieve"
        ))
    }

    fn describe_lcm_admitted<'a>(
        &'a self,
        _context: &'a RequestContext,
        _cancellation: &'a CancellationSignal,
        _command: LcmDescribeServiceCommand,
    ) -> LcmDescribeServiceFuture<'a> {
        Box::pin(async {
            LcmDescribeServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::service_not_configured(),
            )
        })
    }

    fn expand_lcm_admitted<'a>(
        &'a self,
        _context: &'a RequestContext,
        _cancellation: &'a CancellationSignal,
        _command: LcmExpandServiceCommand,
    ) -> LcmExpandServiceFuture<'a> {
        Box::pin(async {
            LcmExpandServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::service_not_configured(),
            )
        })
    }
}

/// Scope-bound terminal used when a project has no mounted session-retrieval
/// identity (for example, an enrolled non-Git project with no graph scope).
///
/// The missing optional authority must not abort the rest of project
/// composition. It also must not fabricate an empty session store: requests
/// for the admitted project receive the canonical typed unavailable outcome,
/// while any other scope remains denied.
pub struct UnavailableSessionApplicationRetrievalV1 {
    scope: ResolvedScope,
}

impl UnavailableSessionApplicationRetrievalV1 {
    pub fn new(scope: ResolvedScope) -> Self {
        Self { scope }
    }
}

impl SessionApplicationRetrievalPortV1 for UnavailableSessionApplicationRetrievalV1 {
    fn retrieve_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        _query: SessionTemporalQuery,
    ) -> SessionApplicationRetrievalFutureV1<'a> {
        Box::pin(async move {
            if !context.scope().identifies_same_checkout(&self.scope) {
                SessionRetrievalServiceOutcome::Denied
            } else {
                SessionRetrievalServiceOutcome::Unavailable(
                    SessionRetrievalUnavailable::service_not_configured(),
                )
            }
        })
    }
}

/// RAII count of admitted retrievals currently executing, so cancellation,
/// deadline, and panic exits can never leak the in-flight gauge.
struct SessionRetrievalInFlightObservation;

impl SessionRetrievalInFlightObservation {
    fn begin() -> Self {
        hotpath::gauge!("daemon.session_retrieval.in_flight").inc(1.0);
        Self
    }
}

impl Drop for SessionRetrievalInFlightObservation {
    fn drop(&mut self) {
        hotpath::gauge!("daemon.session_retrieval.in_flight").inc(-1.0);
    }
}

impl SessionApplicationRetrievalPortV1 for DaemonSessionRetrievalService {
    fn retrieve_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        query: SessionTemporalQuery,
    ) -> SessionApplicationRetrievalFutureV1<'a> {
        Box::pin(async move {
            let _in_flight = SessionRetrievalInFlightObservation::begin();
            if requires_refresh_worker(query.freshness_policy())
                && let Some(unavailable) = self.refresh_not_current()
            {
                return SessionRetrievalServiceOutcome::Unavailable(unavailable);
            }
            let binding = match admitted_session_binding(&self.root, self.configuration, context) {
                Ok(binding) => binding,
                Err(outcome) => return *outcome,
            };
            let outcome = self
                .execute_temporal_query_with_context(
                    context,
                    &binding,
                    query,
                    "grant.application.session-retrieval",
                )
                .await;
            self.public_outcome(outcome).await
        })
    }

    fn retrieve_task_session_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        temporal_query: SessionTemporalQuery,
        task_binding: TaskSessionBindingV1,
        retrieval_request: RetrievalRequest,
        query: EphemeralSanitizedQueryViewV1,
        retriever_revision: ComponentRevision,
        score_domain: ScoreDomainId,
        policy_revision: ComponentRevision,
        selector: &'a dyn TaskSessionRankSelectorV1,
    ) -> TaskSessionApplicationRetrievalFutureV1<'a> {
        Box::pin(async move {
            if requires_refresh_worker(temporal_query.freshness_policy())
                && self.refresh_not_current().is_some()
            {
                return TaskSessionRetrievalOutcomeV1::Unavailable;
            }
            let binding = match admitted_session_binding(&self.root, self.configuration, context) {
                Ok(binding) => binding,
                Err(outcome) => return task_session_binding_outcome(*outcome),
            };
            let authorizer = super::DaemonSessionRetrievalAuthorizer {
                actor: context.actor().clone(),
                identity: self.root.identity.clone(),
                session_id: temporal_query.session_id().clone(),
                retrieval_scope: temporal_query.retrieval_scope().clone(),
                temporal_mode: temporal_query.temporal_mode(),
                grain: temporal_query.grain(),
                provider: temporal_query.provider().map(str::to_owned),
                grant_id: "grant.application.work-task-session-retrieval",
            };
            let Ok(execution) = self.registered_execution() else {
                return TaskSessionRetrievalOutcomeV1::Unavailable;
            };
            tracedecay_session_memory::session::SessionRetrievalService::new(
                authorizer,
                execution,
                super::MessageSearchWordEstimator,
                self.configuration,
            )
            .execute_task_session(
                context,
                &binding,
                temporal_query,
                task_binding,
                retrieval_request,
                query,
                retriever_revision,
                score_domain,
                policy_revision,
                selector,
            )
            .await
        })
    }

    fn describe_lcm_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        cancellation: &'a CancellationSignal,
        command: LcmDescribeServiceCommand,
    ) -> LcmDescribeServiceFuture<'a> {
        Box::pin(async move {
            let _in_flight = SessionRetrievalInFlightObservation::begin();
            if cancellation.context().token_id != context.cancellation().token_id {
                return LcmDescribeServiceOutcome::Denied;
            }
            if cancellation.is_cancelled() {
                return LcmDescribeServiceOutcome::Cancelled;
            }
            let binding =
                match admitted_lcm_session_binding(&self.root, self.configuration, context) {
                    Ok(binding) => binding,
                    Err(outcome) => return describe_binding_outcome(*outcome),
                };
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    hotpath::gauge!("daemon.session_retrieval.cancelled").inc(1.0);
                    LcmDescribeServiceOutcome::Cancelled
                }
                outcome = self.execute_lcm_describe_admitted(context, &binding, command) => outcome,
            }
        })
    }

    fn expand_lcm_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        cancellation: &'a CancellationSignal,
        command: LcmExpandServiceCommand,
    ) -> LcmExpandServiceFuture<'a> {
        Box::pin(async move {
            let _in_flight = SessionRetrievalInFlightObservation::begin();
            if cancellation.context().token_id != context.cancellation().token_id {
                return LcmExpandServiceOutcome::Denied;
            }
            if cancellation.is_cancelled() {
                return LcmExpandServiceOutcome::Cancelled;
            }
            let binding =
                match admitted_lcm_session_binding(&self.root, self.configuration, context) {
                    Ok(binding) => binding,
                    Err(outcome) => return expand_binding_outcome(*outcome),
                };
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    hotpath::gauge!("daemon.session_retrieval.cancelled").inc(1.0);
                    LcmExpandServiceOutcome::Cancelled
                }
                outcome = self.execute_lcm_expand_admitted(context, &binding, command) => outcome,
            }
        })
    }
}

fn task_session_binding_outcome(
    outcome: SessionRetrievalServiceOutcome,
) -> TaskSessionRetrievalOutcomeV1 {
    match outcome {
        SessionRetrievalServiceOutcome::WrongScope => TaskSessionRetrievalOutcomeV1::WrongScope,
        SessionRetrievalServiceOutcome::Denied => TaskSessionRetrievalOutcomeV1::Denied,
        SessionRetrievalServiceOutcome::ResetRequired { .. } => {
            TaskSessionRetrievalOutcomeV1::ResetRequired
        }
        SessionRetrievalServiceOutcome::TimedOut => TaskSessionRetrievalOutcomeV1::TimedOut,
        SessionRetrievalServiceOutcome::CursorStale => TaskSessionRetrievalOutcomeV1::Unavailable,
        SessionRetrievalServiceOutcome::Cancelled => TaskSessionRetrievalOutcomeV1::Cancelled,
        SessionRetrievalServiceOutcome::BudgetExhausted { stage } => {
            TaskSessionRetrievalOutcomeV1::BudgetExhausted { stage }
        }
        SessionRetrievalServiceOutcome::CursorManifestLimitExceeded {
            kind,
            observed,
            maximum,
        } => TaskSessionRetrievalOutcomeV1::CursorManifestLimitExceeded {
            kind,
            observed,
            maximum,
        },
        _ => TaskSessionRetrievalOutcomeV1::Unavailable,
    }
}

fn describe_binding_outcome(outcome: SessionRetrievalServiceOutcome) -> LcmDescribeServiceOutcome {
    match outcome {
        SessionRetrievalServiceOutcome::WrongScope => LcmDescribeServiceOutcome::WrongScope,
        SessionRetrievalServiceOutcome::TimedOut => LcmDescribeServiceOutcome::TimedOut,
        SessionRetrievalServiceOutcome::Cancelled => LcmDescribeServiceOutcome::Cancelled,
        SessionRetrievalServiceOutcome::Denied => LcmDescribeServiceOutcome::Denied,
        SessionRetrievalServiceOutcome::ResetRequired { store_scope } => {
            LcmDescribeServiceOutcome::ResetRequired { store_scope }
        }
        SessionRetrievalServiceOutcome::CursorStale => LcmDescribeServiceOutcome::CursorStale,
        SessionRetrievalServiceOutcome::BudgetExhausted { .. } => {
            LcmDescribeServiceOutcome::BudgetExhausted
        }
        SessionRetrievalServiceOutcome::CursorManifestLimitExceeded {
            kind,
            observed,
            maximum,
        } => LcmDescribeServiceOutcome::CursorManifestLimitExceeded {
            kind,
            observed,
            maximum,
        },
        SessionRetrievalServiceOutcome::Unavailable(unavailable) => {
            LcmDescribeServiceOutcome::Unavailable(unavailable)
        }
        _ => LcmDescribeServiceOutcome::Unavailable(temporal_store_unavailable_value()),
    }
}

fn expand_binding_outcome(outcome: SessionRetrievalServiceOutcome) -> LcmExpandServiceOutcome {
    match outcome {
        SessionRetrievalServiceOutcome::WrongScope => LcmExpandServiceOutcome::WrongScope,
        SessionRetrievalServiceOutcome::TimedOut => LcmExpandServiceOutcome::TimedOut,
        SessionRetrievalServiceOutcome::Cancelled => LcmExpandServiceOutcome::Cancelled,
        SessionRetrievalServiceOutcome::Denied => LcmExpandServiceOutcome::Denied,
        SessionRetrievalServiceOutcome::ResetRequired { store_scope } => {
            LcmExpandServiceOutcome::ResetRequired { store_scope }
        }
        SessionRetrievalServiceOutcome::CursorStale => LcmExpandServiceOutcome::CursorStale,
        SessionRetrievalServiceOutcome::BudgetExhausted { .. } => {
            LcmExpandServiceOutcome::BudgetExhausted
        }
        SessionRetrievalServiceOutcome::CursorManifestLimitExceeded {
            kind,
            observed,
            maximum,
        } => LcmExpandServiceOutcome::CursorManifestLimitExceeded {
            kind,
            observed,
            maximum,
        },
        SessionRetrievalServiceOutcome::Unavailable(unavailable) => {
            LcmExpandServiceOutcome::Unavailable(unavailable)
        }
        _ => LcmExpandServiceOutcome::Unavailable(temporal_store_unavailable_value()),
    }
}

/// Admission decision counters over the one binding choke point every
/// admitted retrieval, task-session, describe, and expand request crosses.
/// The builder refuses with exactly two typed outcomes, so the reason set
/// stays static and bounded.
fn admitted_session_binding(
    root: &DaemonSessionRetrievalRoot,
    retrieval_configuration: SessionRetrievalConfiguration,
    context: &RequestContext,
) -> Result<SessionRequestBinding, Box<SessionRetrievalServiceOutcome>> {
    let budgets = RequestBudgets::new(
        APPLICATION_RETRIEVAL_MAX_RESULTS,
        APPLICATION_RETRIEVAL_MAX_BYTES,
        APPLICATION_RETRIEVAL_MAX_WORK_UNITS,
    )
    .map_err(|_| Box::new(temporal_store_unavailable()))?;
    counted_admitted_session_binding(root, retrieval_configuration, context, budgets)
}

/// As [`admitted_session_binding`], but carrying the canonical daemon LCM
/// budgets. LCM describe and expand verify content hashes over whole payloads
/// before slicing, so their read budget is the LCM authority's — the response
/// stays bounded by the query's context budget and the MCP response cap.
fn admitted_lcm_session_binding(
    root: &DaemonSessionRetrievalRoot,
    retrieval_configuration: SessionRetrievalConfiguration,
    context: &RequestContext,
) -> Result<SessionRequestBinding, Box<SessionRetrievalServiceOutcome>> {
    let budgets = RequestBudgets::new(
        crate::lcm_authority::LCM_MAX_RESULTS,
        crate::lcm_authority::LCM_MAX_BYTES,
        crate::lcm_authority::LCM_MAX_WORK_UNITS,
    )
    .map_err(|_| Box::new(temporal_store_unavailable()))?;
    counted_admitted_session_binding(root, retrieval_configuration, context, budgets)
}

fn counted_admitted_session_binding(
    root: &DaemonSessionRetrievalRoot,
    retrieval_configuration: SessionRetrievalConfiguration,
    context: &RequestContext,
    budgets: RequestBudgets,
) -> Result<SessionRequestBinding, Box<SessionRetrievalServiceOutcome>> {
    let binding = build_admitted_session_binding(root, retrieval_configuration, context, budgets);
    match &binding {
        Ok(_) => {
            hotpath::gauge!("daemon.session_retrieval.admitted").inc(1.0);
        }
        Err(outcome) => match outcome.as_ref() {
            SessionRetrievalServiceOutcome::WrongScope => {
                hotpath::gauge!("daemon.session_retrieval.refused.wrong_scope").inc(1.0);
            }
            _ => {
                hotpath::gauge!("daemon.session_retrieval.refused.unavailable").inc(1.0);
            }
        },
    }
    binding
}

fn build_admitted_session_binding(
    root: &DaemonSessionRetrievalRoot,
    retrieval_configuration: SessionRetrievalConfiguration,
    context: &RequestContext,
    budgets: RequestBudgets,
) -> Result<SessionRequestBinding, Box<SessionRetrievalServiceOutcome>> {
    if matches!(root.store_scope, SessionRetrievalStoreScope::Project)
        != root.identity.project_id().is_some()
    {
        return Err(Box::new(SessionRetrievalServiceOutcome::WrongScope));
    }
    let scope = root
        .identity
        .session_request_scope()
        .map_err(|_| Box::new(SessionRetrievalServiceOutcome::WrongScope))?;
    // Checkout identity, not the branch label: the mounted session identity
    // carries the branch its graph scope was registered under, the request
    // context carries the branch HEAD currently points at, and a session store
    // is partitioned by project/repository/worktree only. See
    // `ResolvedScope::identifies_same_checkout`.
    if !context.scope().identifies_same_checkout(&scope) {
        return Err(Box::new(SessionRetrievalServiceOutcome::WrongScope));
    }
    let cancellation = CancellationToken::for_admitted_application_request(
        context.cancellation().token_id.as_str(),
    )
    .ok_or_else(|| Box::new(temporal_store_unavailable()))?;
    if context.cancellation().is_cancelled() {
        cancellation.cancel();
    }
    let capability = CapabilityDigest::new(application_retrieval_digest(
        b"tracedecay.application.session-retrieval.capability.v1\0",
        &root.identity,
        context.grant().digest.as_str().as_bytes(),
        retrieval_configuration,
    ));
    let access_policy = tracedecay_store::observation_capture_access_policy_digest_v1()
        .map_err(|_| Box::new(temporal_store_unavailable()))?;
    let policy = PolicyDigest::from_access_policy_digest(&access_policy)
        .map_err(|_| Box::new(temporal_store_unavailable()))?;
    let configuration = ConfigurationDigest::new(application_retrieval_digest(
        b"tracedecay.application.session-retrieval.configuration.v1\0",
        &root.identity,
        &[],
        retrieval_configuration,
    ));
    Ok(SessionRequestBinding::for_admitted_context(
        root.identity.clone(),
        capability,
        policy,
        configuration,
        cancellation,
        budgets,
        context.grant().digest.clone(),
    ))
}

fn application_retrieval_digest(
    domain: &[u8],
    identity: &ResolvedSessionIdentity,
    admitted_authority: &[u8],
    configuration: SessionRetrievalConfiguration,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(message_search_digest(
        b"tracedecay.application.session-retrieval.root.v1\0",
        identity,
        None,
    ));
    digest.update(configuration.schema_version().to_be_bytes());
    digest.update(configuration.ranking_version().to_be_bytes());
    digest.update(admitted_authority);
    digest.finalize().into()
}

fn temporal_store_unavailable() -> SessionRetrievalServiceOutcome {
    SessionRetrievalServiceOutcome::Unavailable(temporal_store_unavailable_value())
}

const fn temporal_store_unavailable_value() -> SessionRetrievalUnavailable {
    SessionRetrievalUnavailable::without_worker(
        SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestContext, RequestId,
    };
    use tracedecay_domain::{
        ActorId, BrainId, ManifestDigest, ProjectId, RepositoryId, UserProfileId, UtcMicros,
        WorktreeId,
    };
    use tracedecay_session_memory::context::{
        BranchId, ProfileId, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId,
        SessionStoreId,
    };
    use tracedecay_store::StoreShardIdV1;
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::*;

    #[test]
    fn task_session_binding_preserves_cursor_manifest_refusal_details() {
        assert!(matches!(
            task_session_binding_outcome(
                SessionRetrievalServiceOutcome::CursorManifestLimitExceeded {
                    kind: tracedecay_domain::CursorManifestLimitKindV1::Participants,
                    observed: 257,
                    maximum: 256,
                }
            ),
            TaskSessionRetrievalOutcomeV1::CursorManifestLimitExceeded {
                kind: tracedecay_domain::CursorManifestLimitKindV1::Participants,
                observed: 257,
                maximum: 256,
            }
        ));
    }

    #[test]
    fn admitted_binding_preserves_outer_grant_scope_and_cancellation_identity() {
        let project_id = ProjectId::new("project.session-retrieval").expect("project identity");
        let expected_runtime_shard = test_project_shard(project_id.clone());
        let identity = ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.session-retrieval").expect("profile identity"),
            project_id.clone(),
            SessionStoreId::new("store.project.test").expect("store identity"),
            SessionRootId::new("root.project.test").expect("root identity"),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.project.test").expect("repository identity"),
                WorktreeId::new("worktree.project.test").expect("worktree identity"),
                BranchId::new("main").expect("branch identity"),
            ),
        );
        let root = DaemonSessionRetrievalRoot {
            store_scope: SessionRetrievalStoreScope::Project,
            identity,
            project_id: Some(project_id.as_str().to_owned()),
            authorized_root: None,
            expected_runtime_shard: Some(expected_runtime_shard),
        };
        let actor = ActorId::new("actor.work-evidence").expect("actor");
        let scope = root
            .identity
            .session_request_scope()
            .expect("application scope");
        let grant_digest =
            ManifestDigest::new(format!("sha256:{}", "7".repeat(64))).expect("grant digest");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.work-evidence").expect("grant id"),
            3,
            grant_digest.clone(),
            actor.clone(),
            UtcMicros(10),
            UtcMicros(1_000),
            scope.clone(),
            BTreeSet::from([
                CapabilityId::new("capability.work.evidence.read").expect("capability")
            ]),
            BTreeSet::from([UseCaseId::new("use-case.work.evidence.read").expect("use case")]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        let context = RequestContext::new(
            actor,
            scope,
            grant,
            RequestId::new("request.work-evidence").expect("request id"),
            Deadline::new(UtcMicros(900)).expect("deadline"),
            CancellationContext::cancelled("cancellation.work-evidence", UtcMicros(20))
                .expect("cancellation"),
        )
        .expect("request context");

        let binding = admitted_session_binding(
            &root,
            SessionRetrievalConfiguration::new(2, 5).expect("retrieval configuration"),
            &context,
        )
        .expect("admitted binding");

        assert_eq!(binding.identity(), &root.identity);
        assert_eq!(binding.admitted_grant_digest(), Some(&grant_digest));
        assert_eq!(
            binding.cancellation().application_token_id(),
            Some("cancellation.work-evidence")
        );
        assert!(binding.cancellation().is_cancelled());
        assert_eq!(binding.validate_context(&context), Ok(()));
    }

    fn retrieval_root(branch: &str) -> DaemonSessionRetrievalRoot {
        let project_id = ProjectId::new("project.session-retrieval").expect("project identity");
        let expected_runtime_shard = test_project_shard(project_id.clone());
        DaemonSessionRetrievalRoot {
            store_scope: SessionRetrievalStoreScope::Project,
            identity: ResolvedSessionIdentity::for_project(
                ProfileId::new("profile.session-retrieval").expect("profile identity"),
                project_id.clone(),
                SessionStoreId::new("store.project.test").expect("store identity"),
                SessionRootId::new("root.project.test").expect("root identity"),
                ResolvedGitRoute::new(
                    RepositoryId::new("repository.project.test").expect("repository identity"),
                    WorktreeId::new("worktree.project.test").expect("worktree identity"),
                    BranchId::new(branch).expect("branch identity"),
                ),
            ),
            project_id: Some(project_id.as_str().to_owned()),
            authorized_root: None,
            expected_runtime_shard: Some(expected_runtime_shard),
        }
    }

    fn test_project_shard(project_id: ProjectId) -> StoreShardIdV1 {
        StoreShardIdV1::project_sessions(
            BrainId::new("brain.session-retrieval").expect("brain identity"),
            UserProfileId::new("profile.session-retrieval").expect("profile identity"),
            project_id,
        )
    }

    fn request_context_for(scope: tracedecay_application::ResolvedScope) -> RequestContext {
        let actor = ActorId::new("actor.message-search").expect("actor");
        let grant_digest =
            ManifestDigest::new(format!("sha256:{}", "5".repeat(64))).expect("grant digest");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.message-search").expect("grant id"),
            1,
            grant_digest,
            actor.clone(),
            UtcMicros(10),
            UtcMicros(1_000),
            scope.clone(),
            BTreeSet::from([
                CapabilityId::new("capability.session.temporal-retrieval").expect("capability")
            ]),
            BTreeSet::from([
                UseCaseId::new("use-case.session.temporal-retrieval").expect("use case")
            ]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        RequestContext::new(
            actor,
            scope,
            grant,
            RequestId::new("request.message-search").expect("request id"),
            Deadline::new(UtcMicros(900)).expect("deadline"),
            CancellationContext::active("cancellation.message-search").expect("cancellation"),
        )
        .expect("request context")
    }

    /// The production failure this guards: the mounted session identity records
    /// the branch its *graph scope* was registered under (`master`), while the
    /// request context carries whatever branch HEAD points at. Comparing whole
    /// scopes made every `message_search` and `lcm_grep` on a feature branch
    /// answer `WrongScope`, which the retained surface renders as
    /// `not_found_or_not_authorized`.
    #[test]
    fn admitted_binding_serves_the_same_checkout_on_a_different_branch() {
        let root = retrieval_root("master");
        let serving_scope = root
            .identity
            .session_request_scope()
            .expect("application scope");
        let head_scope = tracedecay_application::ResolvedScope::new(
            serving_scope.project_id.clone(),
            serving_scope.repository_id.clone(),
            serving_scope.worktree_id.clone(),
            Some(
                tracedecay_domain::RefId::new("refs/heads/feature/hot-paths")
                    .expect("head reference"),
            ),
        )
        .expect("head scope");
        assert_ne!(head_scope, serving_scope);
        assert!(head_scope.identifies_same_checkout(&serving_scope));

        let context = request_context_for(head_scope);
        let binding = admitted_session_binding(
            &root,
            SessionRetrievalConfiguration::new(2, 5).expect("retrieval configuration"),
            &context,
        )
        .expect("branch label divergence must not refuse the mounted checkout");
        assert_eq!(binding.identity(), &root.identity);
    }

    #[test]
    fn admitted_binding_still_refuses_a_foreign_worktree() {
        let root = retrieval_root("master");
        let serving_scope = root
            .identity
            .session_request_scope()
            .expect("application scope");
        let foreign_scope = tracedecay_application::ResolvedScope::new(
            serving_scope.project_id.clone(),
            serving_scope.repository_id.clone(),
            WorktreeId::new("worktree.project.other").expect("worktree identity"),
            serving_scope.reference.clone(),
        )
        .expect("foreign scope");

        let context = request_context_for(foreign_scope);
        let outcome = admitted_session_binding(
            &root,
            SessionRetrievalConfiguration::new(2, 5).expect("retrieval configuration"),
            &context,
        )
        .expect_err("a different worktree is not the mounted checkout");
        assert!(matches!(
            *outcome,
            SessionRetrievalServiceOutcome::WrongScope
        ));
    }
}
