//! Application-admitted retrieval over one exact mounted session root.

use std::future::Future;
use std::pin::Pin;

use sha2::{Digest, Sha256};
use tracedecay_application::{CancellationSignal, RequestContext};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_store::StoreShardScopeV1;
use tracedecay_usecases::context::{
    CapabilityDigest, ConfigurationDigest, PolicyDigest, RequestBudgets, ResolvedSessionIdentity,
};
use tracedecay_usecases::session::{
    SessionRequestBinding, SessionRetrievalConfiguration, SessionTemporalQuery,
};

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

const APPLICATION_RETRIEVAL_MAX_RESULTS: u64 = 100;
const APPLICATION_RETRIEVAL_MAX_BYTES: u64 = 64 * 1024;
const APPLICATION_RETRIEVAL_MAX_WORK_UNITS: u64 = 100_000;

impl DaemonSessionRetrievalService {
    /// Mount the canonical profile-session retrieval service over the exact
    /// registered profile shard and retained session identity supplied by the
    /// daemon composition root.
    pub(crate) fn new_admitted_profile(
        database: std::sync::Arc<crate::global_db::RegisteredGlobalDb>,
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
        Self::new(
            database,
            DaemonSessionRetrievalRoot {
                store_scope: SessionRetrievalStoreScope::Profile,
                identity,
                project_id: None,
                project_paths: std::collections::HashSet::new(),
                authorized_root: None,
                expected_runtime_shard: None,
            },
            None,
        )
    }
}

pub(crate) type SessionApplicationRetrievalFutureV1<'a> =
    Pin<Box<dyn Future<Output = SessionRetrievalServiceOutcome> + Send + 'a>>;

/// Application-level Plan 23 retrieval over one already-mounted session root.
///
/// The caller has already crossed its application admission boundary. This
/// port therefore accepts the original immutable context and never routes
/// through MCP command parsing or mints a replacement application identity.
pub(crate) trait SessionApplicationRetrievalPortV1: Send + Sync {
    fn retrieve_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        query: SessionTemporalQuery,
    ) -> SessionApplicationRetrievalFutureV1<'a>;

    fn retrieve_admitted_with_cancellation<'a>(
        &'a self,
        context: &'a RequestContext,
        cancellation: &'a CancellationSignal,
        query: SessionTemporalQuery,
    ) -> SessionApplicationRetrievalFutureV1<'a> {
        Box::pin(async move {
            if cancellation.context().token_id != context.cancellation().token_id {
                return SessionRetrievalServiceOutcome::Denied;
            }
            if cancellation.is_cancelled() {
                return SessionRetrievalServiceOutcome::Cancelled;
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => SessionRetrievalServiceOutcome::Cancelled,
                outcome = self.retrieve_admitted(context, query) => outcome,
            }
        })
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

impl SessionApplicationRetrievalPortV1 for DaemonSessionRetrievalService {
    fn retrieve_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        query: SessionTemporalQuery,
    ) -> SessionApplicationRetrievalFutureV1<'a> {
        Box::pin(async move {
            if requires_refresh_worker(query.freshness_policy())
                && let Some(unavailable) = self.refresh_not_current()
            {
                return SessionRetrievalServiceOutcome::Unavailable(unavailable);
            }
            let binding = match admitted_session_binding(&self.root, self.configuration, context) {
                Ok(binding) => binding,
                Err(outcome) => return outcome,
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

    fn describe_lcm_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        cancellation: &'a CancellationSignal,
        command: LcmDescribeServiceCommand,
    ) -> LcmDescribeServiceFuture<'a> {
        Box::pin(async move {
            if cancellation.context().token_id != context.cancellation().token_id {
                return LcmDescribeServiceOutcome::Denied;
            }
            if cancellation.is_cancelled() {
                return LcmDescribeServiceOutcome::Cancelled;
            }
            let binding = match admitted_session_binding(&self.root, self.configuration, context) {
                Ok(binding) => binding,
                Err(outcome) => return describe_binding_outcome(outcome),
            };
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => LcmDescribeServiceOutcome::Cancelled,
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
            if cancellation.context().token_id != context.cancellation().token_id {
                return LcmExpandServiceOutcome::Denied;
            }
            if cancellation.is_cancelled() {
                return LcmExpandServiceOutcome::Cancelled;
            }
            let binding = match admitted_session_binding(&self.root, self.configuration, context) {
                Ok(binding) => binding,
                Err(outcome) => return expand_binding_outcome(outcome),
            };
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => LcmExpandServiceOutcome::Cancelled,
                outcome = self.execute_lcm_expand_admitted(context, &binding, command) => outcome,
            }
        })
    }
}

fn describe_binding_outcome(outcome: SessionRetrievalServiceOutcome) -> LcmDescribeServiceOutcome {
    match outcome {
        SessionRetrievalServiceOutcome::WrongScope => LcmDescribeServiceOutcome::WrongScope,
        SessionRetrievalServiceOutcome::Cancelled => LcmDescribeServiceOutcome::Cancelled,
        SessionRetrievalServiceOutcome::Denied => LcmDescribeServiceOutcome::Denied,
        SessionRetrievalServiceOutcome::BudgetExhausted
        | SessionRetrievalServiceOutcome::CursorManifestLimitExceeded { .. } => {
            LcmDescribeServiceOutcome::BudgetExhausted
        }
        SessionRetrievalServiceOutcome::Unavailable(unavailable) => {
            LcmDescribeServiceOutcome::Unavailable(unavailable)
        }
        _ => LcmDescribeServiceOutcome::Unavailable(temporal_store_unavailable_value()),
    }
}

fn expand_binding_outcome(outcome: SessionRetrievalServiceOutcome) -> LcmExpandServiceOutcome {
    match outcome {
        SessionRetrievalServiceOutcome::WrongScope => LcmExpandServiceOutcome::WrongScope,
        SessionRetrievalServiceOutcome::Cancelled => LcmExpandServiceOutcome::Cancelled,
        SessionRetrievalServiceOutcome::Denied => LcmExpandServiceOutcome::Denied,
        SessionRetrievalServiceOutcome::BudgetExhausted
        | SessionRetrievalServiceOutcome::CursorManifestLimitExceeded { .. } => {
            LcmExpandServiceOutcome::BudgetExhausted
        }
        SessionRetrievalServiceOutcome::Unavailable(unavailable) => {
            LcmExpandServiceOutcome::Unavailable(unavailable)
        }
        _ => LcmExpandServiceOutcome::Unavailable(temporal_store_unavailable_value()),
    }
}

fn admitted_session_binding(
    root: &DaemonSessionRetrievalRoot,
    retrieval_configuration: SessionRetrievalConfiguration,
    context: &RequestContext,
) -> Result<SessionRequestBinding, SessionRetrievalServiceOutcome> {
    if matches!(root.store_scope, SessionRetrievalStoreScope::Project)
        != root.identity.project_id().is_some()
    {
        return Err(SessionRetrievalServiceOutcome::WrongScope);
    }
    let scope = root
        .identity
        .session_request_scope()
        .map_err(|_| SessionRetrievalServiceOutcome::WrongScope)?;
    if context.scope() != &scope {
        return Err(SessionRetrievalServiceOutcome::WrongScope);
    }
    let cancellation = CancellationToken::for_admitted_application_request(
        context.cancellation().token_id.as_str(),
    )
    .ok_or_else(temporal_store_unavailable)?;
    if context.cancellation().is_cancelled() {
        cancellation.cancel();
    }
    let budgets = RequestBudgets::new(
        APPLICATION_RETRIEVAL_MAX_RESULTS,
        APPLICATION_RETRIEVAL_MAX_BYTES,
        APPLICATION_RETRIEVAL_MAX_WORK_UNITS,
    )
    .map_err(|_| temporal_store_unavailable())?;
    let capability = CapabilityDigest::new(application_retrieval_digest(
        b"tracedecay.application.session-retrieval.capability.v1\0",
        &root.identity,
        context.grant().digest.as_str().as_bytes(),
        retrieval_configuration,
    ));
    let policy = PolicyDigest::new(application_retrieval_digest(
        b"tracedecay.application.session-retrieval.policy.v1\0",
        &root.identity,
        context.grant().digest.as_str().as_bytes(),
        retrieval_configuration,
    ));
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
    use std::collections::{BTreeSet, HashSet};

    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestContext, RequestId,
    };
    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
    use tracedecay_usecases::context::{
        BranchId, ProfileId, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId,
        SessionStoreId,
    };

    use super::*;

    #[test]
    fn admitted_binding_preserves_outer_grant_scope_and_cancellation_identity() {
        let project_id = ProjectId::new("project.session-retrieval").expect("project identity");
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
            project_paths: HashSet::new(),
            authorized_root: None,
            expected_runtime_shard: None,
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
}
