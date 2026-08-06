//! Daemon-backed session retrieval (message search) service: retrieval-root
//! resolution, scope authorization, request-context construction, LCM
//! describe/expand execution, and result filtering for the
//! `SessionRetrievalServicePort` implementation.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sha2::{Digest, Sha256};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{
    ActorId, HydrationStateV1, PayloadReferenceV1, ProjectId, RepositoryId, RetrievalAnchorId,
    RetrievalGrainV1, SessionId, TemporalCoverageCountsV1, TemporalModeV1, UtcMicros, WorktreeId,
};
use tracedecay_sessions::lcm::contracts::{LcmDataFreshness, LcmRetrievalOutcome};
use tracedecay_store::StoreShardIdV1;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::application::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use crate::application::session::{
    AuthorizationGrantId, SessionAccess, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionDataFreshness, SessionFreshnessPolicy, SessionRequestBinding,
    SessionRetrievalConfiguration, SessionRetrievalOutcome, SessionRetrievalScope,
    SessionRetrievalService, SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
    SessionTemporalExecutionError, SessionTemporalQuery,
};
use crate::daemon::session_temporal_refresh_scheduler::{
    SessionTemporalRefreshBlocker, SessionTemporalRefreshRetryClass,
    SessionTemporalRefreshUnavailableReason, SessionTemporalRefreshWake,
    SessionTemporalRefreshWorkerStatus,
};
use crate::global_db::session_temporal::RegisteredGlobalDbSessionTemporalExecution;
use crate::global_db::{ProjectRegistryContext, RegisteredGlobalDb};
use crate::mcp::tools::{
    LcmDescribeServiceCommand, LcmDescribeServiceFuture, LcmDescribeServiceOutcome,
    LcmExpandServiceCommand, LcmExpandServiceFuture, LcmExpandServiceOutcome,
    SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalOmissionView,
    SessionRetrievalPageView, SessionRetrievalServiceFuture, SessionRetrievalServiceOutcome,
    SessionRetrievalServicePort, SessionRetrievalStoreScope, SessionRetrievalUnavailable,
    SessionRetrievalUnavailableReason, SessionRetrievalWorkerBlocker,
    SessionRetrievalWorkerRetryClass, SessionRetrievalWorkerStatusView,
    SessionTemporalMetadataView, SessionTemporalWatermarksView,
};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use crate::sessions::lcm::{
    LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget, LcmExpandRequest, LcmExpandTarget,
};
use crate::sessions::{SessionMessageSearchResult, SessionRecord};
use crate::tracedecay::TraceDecay;
use tracedecay_temporal_query::context::{ContextBudget, TokenPolicy, VersionedTokenEstimator};
use tracedecay_temporal_query::ports::TemporalExecutionSnapshot;
use tracedecay_temporal_query::ranking::{DiversityLimits, RankedCandidate};
use tracedecay_temporal_query::{TemporalHydratedResult, TemporalKernelResult};

const MESSAGE_SEARCH_ACTOR_ID: &str = "mcp.message-search";
#[cfg(test)]
pub(crate) const MESSAGE_SEARCH_ROOT_SESSION_ID: &str = "session.message-search.root";
const MESSAGE_SEARCH_PROFILE_ID: &str = "profile.primary";
const MESSAGE_SEARCH_SCHEMA_VERSION: u32 = 1;
const MESSAGE_SEARCH_RANKING_VERSION: u32 = 1;
const MESSAGE_SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const MESSAGE_SEARCH_MAX_RESULTS: u64 = 1_024;
const MESSAGE_SEARCH_MAX_BYTES: u64 = 16 * 1024 * 1024;
const MESSAGE_SEARCH_MAX_WORK_UNITS: u64 = 100_000;

#[derive(Clone)]
pub(crate) struct DaemonSessionRetrievalRoot {
    store_scope: SessionRetrievalStoreScope,
    identity: ResolvedSessionIdentity,
    project_id: Option<String>,
    project_paths: HashSet<PathBuf>,
    authorized_root: Option<String>,
    expected_runtime_shard: Option<StoreShardIdV1>,
}

impl DaemonSessionRetrievalRoot {
    pub(crate) async fn project(cg: &TraceDecay, registry: &RegisteredGlobalDb) -> Option<Self> {
        let project_id = cg.store_layout().identity.project_id.as_deref()?;
        let context = registry
            .project_registry_context_by_id(project_id)
            .await
            .ok()??;
        Self::from_project_context(cg, registry, context)
    }

    fn from_project_context(
        cg: &TraceDecay,
        registry: &RegisteredGlobalDb,
        context: ProjectRegistryContext,
    ) -> Option<Self> {
        let profile_root = registry.db_path().parent()?;
        let serving_db = cg.db_path();
        let mut selected = None;
        for store in &context.stores {
            for scope in &store.graph_scopes {
                if scope.writable
                    && scope.project_id == context.project.project_id
                    && scope.store_id == store.store.store_id
                    && profile_root.join(&scope.db_relpath) == serving_db
                {
                    if selected.is_some() {
                        return None;
                    }
                    selected = Some((store.store.store_id.clone(), scope.graph_scope_id.clone()));
                }
            }
        }
        let (store_id, graph_scope_id) = selected?;

        let project_key = ProjectId::new(context.project.project_id.clone()).ok()?;
        let repository_id = context
            .project
            .git_common_dir
            .clone()
            .unwrap_or_else(|| format!("repository.project.{}", context.project.project_id));
        let identity = ResolvedSessionIdentity::for_project(
            ProfileId::new(MESSAGE_SEARCH_PROFILE_ID).ok()?,
            project_key,
            SessionStoreId::new(store_id).ok()?,
            SessionRootId::new(graph_scope_id.clone()).ok()?,
            ResolvedGitRoute::new(
                RepositoryId::new(repository_id).ok()?,
                WorktreeId::new(context.project.canonical_root.clone()).ok()?,
                BranchId::new(graph_scope_id).ok()?,
            ),
        );
        let mut project_paths = context
            .aliases
            .iter()
            .map(|alias| PathBuf::from(&alias.alias_path))
            .collect::<HashSet<_>>();
        project_paths.insert(PathBuf::from(&context.project.canonical_root));
        project_paths.insert(PathBuf::from(&context.project.display_root));
        Some(Self {
            store_scope: SessionRetrievalStoreScope::Project,
            identity,
            project_id: Some(context.project.project_id),
            project_paths,
            authorized_root: Some(context.project.display_root),
            expected_runtime_shard: None,
        })
    }

    #[cfg(any(test, feature = "test-transport"))]
    pub(crate) fn project_for_test(cg: &TraceDecay) -> Self {
        let project_root = cg.project_root().to_path_buf();
        let project_id = cg.store_layout().identity.project_id.clone();
        let project_key_value = project_id
            .clone()
            .unwrap_or_else(|| project_root.display().to_string());
        let project_key = ProjectId::new(project_key_value.clone())
            .unwrap_or_else(|error| panic!("test project identity: {error}"));
        let identity = ResolvedSessionIdentity::for_project(
            ProfileId::new(MESSAGE_SEARCH_PROFILE_ID)
                .unwrap_or_else(|error| panic!("test profile identity: {error}")),
            project_key,
            SessionStoreId::new("store.project.test")
                .unwrap_or_else(|error| panic!("test store identity: {error}")),
            SessionRootId::new("root.project.test")
                .unwrap_or_else(|error| panic!("test root identity: {error}")),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.project.test")
                    .unwrap_or_else(|error| panic!("test repository identity: {error}")),
                WorktreeId::new(project_root.display().to_string())
                    .unwrap_or_else(|error| panic!("test worktree identity: {error}")),
                BranchId::new("branch.project.test")
                    .unwrap_or_else(|error| panic!("test branch identity: {error}")),
            ),
        );
        Self {
            store_scope: SessionRetrievalStoreScope::Project,
            identity,
            project_id,
            project_paths: HashSet::from([project_root.clone()]),
            authorized_root: Some(project_key_value),
            expected_runtime_shard: None,
        }
    }

    pub(crate) fn profile() -> Option<Self> {
        Some(Self {
            store_scope: SessionRetrievalStoreScope::Profile,
            identity: ResolvedSessionIdentity::for_profile(
                ProfileId::new(MESSAGE_SEARCH_PROFILE_ID).ok()?,
                SessionStoreId::new("store.profile.primary").ok()?,
                SessionRootId::new("root.profile.primary").ok()?,
            ),
            project_id: None,
            project_paths: HashSet::new(),
            authorized_root: None,
            expected_runtime_shard: None,
        })
    }

    pub(crate) fn with_project_runtime_shard(
        self,
        profile_identity: &crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    ) -> Option<Self> {
        self.with_project_runtime_identity(
            profile_identity.brain_id().clone(),
            profile_identity.profile_id().clone(),
        )
    }

    fn with_project_runtime_identity(
        mut self,
        brain_id: tracedecay_domain::BrainId,
        profile_id: tracedecay_domain::UserProfileId,
    ) -> Option<Self> {
        let runtime_project_id = ProjectId::new(self.project_id.as_deref()?).ok()?;
        let request_project_id = self.identity.project_id()?.clone();
        let store_id = self.identity.store_id().clone();
        let root_id = self.identity.root_id().clone();
        let git_route = self.identity.git_route()?.clone();
        self.identity = ResolvedSessionIdentity::for_project(
            ProfileId::new(profile_id.as_str().to_owned()).ok()?,
            request_project_id,
            store_id,
            root_id,
            git_route,
        );
        self.expected_runtime_shard = Some(StoreShardIdV1::project_sessions(
            brain_id,
            profile_id,
            runtime_project_id,
        ));
        Some(self)
    }

    pub(crate) fn with_profile_runtime_shard(
        self,
        profile_identity: &crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    ) -> Option<Self> {
        self.with_profile_runtime_identity(
            profile_identity.brain_id().clone(),
            profile_identity.profile_id().clone(),
        )
    }

    fn with_profile_runtime_identity(
        mut self,
        brain_id: tracedecay_domain::BrainId,
        profile_id: tracedecay_domain::UserProfileId,
    ) -> Option<Self> {
        let store_id = self.identity.store_id().clone();
        let root_id = self.identity.root_id().clone();
        self.identity = ResolvedSessionIdentity::for_profile(
            ProfileId::new(profile_id.as_str().to_owned()).ok()?,
            store_id,
            root_id,
        );
        self.expected_runtime_shard = Some(StoreShardIdV1::profile_sessions(brain_id, profile_id));
        Some(self)
    }

    fn owns(&self, command: &SessionRetrievalCommand) -> bool {
        if command.store_scope() != self.store_scope {
            return false;
        }
        let Some(selector) = command.project_selector() else {
            return true;
        };
        if self.store_scope != SessionRetrievalStoreScope::Project {
            return false;
        }
        selector
            .project_id
            .as_deref()
            .is_none_or(|id| self.project_id.as_deref() == Some(id))
            && selector
                .project_path
                .as_deref()
                .is_none_or(|path| self.project_paths.contains(Path::new(path)))
    }
}

#[derive(Clone, Copy)]
struct MessageSearchWordEstimator;

impl VersionedTokenEstimator for MessageSearchWordEstimator {
    fn version(&self) -> &'static str {
        "words-v1"
    }

    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
    }
}

struct DaemonSessionRetrievalAuthorizer {
    identity: ResolvedSessionIdentity,
    session_id: SessionId,
    retrieval_scope: SessionRetrievalScope,
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
    provider: Option<String>,
    grant_id: &'static str,
}

impl SessionScopeAuthorizer for DaemonSessionRetrievalAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> std::result::Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        if context.actor().as_str() != MESSAGE_SEARCH_ACTOR_ID
            || request.actor_id() != context.actor()
            || binding.identity() != &self.identity
            || request.identity() != &self.identity
        {
            return Err(SessionAuthorizationError::WrongContext);
        }
        if request.session_id() != &self.session_id
            || request.retrieval_scope() != &self.retrieval_scope
            || request.provider_scope() != self.provider.as_deref()
            || request.temporal_mode() != self.temporal_mode
            || request.grain() != self.grain
            || request.access() != SessionAccess::Hydrate
        {
            return Err(SessionAuthorizationError::WrongScope);
        }
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new(self.grant_id)?,
            1,
            context,
            binding,
            request,
        )
    }
}

fn session_retrieval_worker_status(
    status: SessionTemporalRefreshWorkerStatus,
) -> SessionRetrievalWorkerStatusView {
    SessionRetrievalWorkerStatusView {
        last_progress_at_unix_micros: status.last_progress_at_unix_micros,
        backlog: status.backlog,
        blocker: status.blocker.map(|blocker| match blocker {
            SessionTemporalRefreshBlocker::WorkerMissing => {
                SessionRetrievalWorkerBlocker::WorkerMissing
            }
            SessionTemporalRefreshBlocker::WorkerPanicked => {
                SessionRetrievalWorkerBlocker::WorkerPanicked
            }
            SessionTemporalRefreshBlocker::WorkerStopped => {
                SessionRetrievalWorkerBlocker::WorkerStopped
            }
            SessionTemporalRefreshBlocker::Storage => SessionRetrievalWorkerBlocker::Storage,
            SessionTemporalRefreshBlocker::Projector => SessionRetrievalWorkerBlocker::Projector,
            SessionTemporalRefreshBlocker::Deadline => SessionRetrievalWorkerBlocker::Deadline,
        }),
        retry_class: status.retry_class.map(|retry_class| match retry_class {
            SessionTemporalRefreshRetryClass::Storage => SessionRetrievalWorkerRetryClass::Storage,
            SessionTemporalRefreshRetryClass::Projector => {
                SessionRetrievalWorkerRetryClass::Projector
            }
            SessionTemporalRefreshRetryClass::Deadline => {
                SessionRetrievalWorkerRetryClass::Deadline
            }
        }),
    }
}

const fn requires_refresh_worker(freshness_policy: SessionFreshnessPolicy) -> bool {
    matches!(freshness_policy, SessionFreshnessPolicy::RequireFresh)
}

pub(crate) struct DaemonSessionRetrievalService {
    database: Arc<RegisteredGlobalDb>,
    root: DaemonSessionRetrievalRoot,
    configuration: SessionRetrievalConfiguration,
    refresh_status: Option<SessionTemporalRefreshWake>,
}

impl DaemonSessionRetrievalService {
    pub(crate) fn new(
        database: Arc<RegisteredGlobalDb>,
        root: DaemonSessionRetrievalRoot,
        refresh_status: Option<SessionTemporalRefreshWake>,
    ) -> Option<Self> {
        Some(Self {
            database,
            root,
            configuration: SessionRetrievalConfiguration::new(
                MESSAGE_SEARCH_SCHEMA_VERSION,
                MESSAGE_SEARCH_RANKING_VERSION,
            )
            .ok()?,
            refresh_status,
        })
    }

    pub(crate) fn new_registered(
        database: Arc<RegisteredGlobalDb>,
        registered_database: Arc<RegisteredGlobalDb>,
        root: DaemonSessionRetrievalRoot,
        refresh_status: Option<SessionTemporalRefreshWake>,
    ) -> Option<Self> {
        let expected = root.expected_runtime_shard.as_ref()?;
        if &registered_database.binding().shard_id != expected {
            return None;
        }
        if database.binding() != registered_database.binding() {
            return None;
        }
        Some(Self {
            database: registered_database,
            root,
            configuration: SessionRetrievalConfiguration::new(
                MESSAGE_SEARCH_SCHEMA_VERSION,
                MESSAGE_SEARCH_RANKING_VERSION,
            )
            .ok()?,
            refresh_status,
        })
    }

    fn refresh_unavailable(&self) -> Option<SessionRetrievalUnavailable> {
        let status = self.refresh_status.as_ref()?.status();
        let unavailable = status.unavailable_reason?;
        Some(SessionRetrievalUnavailable {
            reason: match unavailable {
                SessionTemporalRefreshUnavailableReason::Missing => {
                    SessionRetrievalUnavailableReason::RefreshWorkerMissing
                }
                SessionTemporalRefreshUnavailableReason::Recovering => {
                    SessionRetrievalUnavailableReason::RefreshWorkerRecovering
                }
                SessionTemporalRefreshUnavailableReason::Stalled => {
                    SessionRetrievalUnavailableReason::RefreshWorkerStalled
                }
                SessionTemporalRefreshUnavailableReason::Stopped => {
                    SessionRetrievalUnavailableReason::RefreshWorkerStopped
                }
            },
            worker: Some(session_retrieval_worker_status(status)),
        })
    }

    fn registered_execution(
        &self,
    ) -> Result<RegisteredGlobalDbSessionTemporalExecution<'_>, SessionTemporalExecutionError> {
        Ok(RegisteredGlobalDbSessionTemporalExecution::new(
            self.database.as_ref(),
        ))
    }

    fn request_context(
        &self,
        provider: Option<&str>,
    ) -> Option<(RequestContext, SessionRequestBinding)> {
        let request_id = mint_global_request_id(GlobalRequestSurface::McpSessionRetrieval).ok()?;
        let request_id = RequestId::new(request_id.as_str()).ok()?;
        let actor = ActorId::new(MESSAGE_SEARCH_ACTOR_ID).ok()?;
        let scope = self.root.identity.session_request_scope().ok()?;
        let capability = message_search_digest(
            b"tracedecay.mcp.message-search.capability.v1\0",
            &self.root.identity,
            provider,
        );
        let policy = message_search_policy_digest()?;
        let configuration = message_search_digest(
            b"tracedecay.mcp.message-search.configuration.v1\0",
            &self.root.identity,
            None,
        );
        let capability = CapabilityDigest::new(capability);
        let policy = PolicyDigest::new(policy);
        let configuration = ConfigurationDigest::new(configuration);
        let cancellation = CancellationToken::for_application_request(request_id.as_str());
        let budgets = RequestBudgets::new(
            MESSAGE_SEARCH_MAX_RESULTS,
            MESSAGE_SEARCH_MAX_BYTES,
            MESSAGE_SEARCH_MAX_WORK_UNITS,
        )
        .ok()?;
        let observed_at = application_observed_at();
        let timeout_micros = i64::try_from(MESSAGE_SEARCH_TIMEOUT.as_micros()).unwrap_or(i64::MAX);
        let expires_at = UtcMicros(observed_at.0.saturating_add(timeout_micros));
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.mcp.message-search").ok()?,
            1,
            session_application_grant_digest(
                capability,
                policy,
                configuration,
                &cancellation,
                budgets,
            )
            .ok()?,
            actor.clone(),
            observed_at,
            expires_at,
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.session.temporal-retrieval").ok()?]),
            BTreeSet::from([UseCaseId::new("use-case.mcp.message-search").ok()?]),
            DisclosureClass::Evidence,
        )
        .ok()?;
        let context = RequestContext::new(
            actor,
            scope,
            grant,
            request_id,
            Deadline::new(expires_at).ok()?,
            CancellationContext::active(cancellation.application_token_id()?).ok()?,
        )
        .ok()?;
        let binding = SessionRequestBinding::new(
            self.root.identity.clone(),
            capability,
            policy,
            configuration,
            cancellation,
            budgets,
        );
        Some((context, binding))
    }

    async fn execute_temporal_query(
        &self,
        query: SessionTemporalQuery,
    ) -> SessionRetrievalOutcome<TemporalKernelResult> {
        let Some((context, binding)) = self.request_context(query.provider()) else {
            return SessionRetrievalOutcome::Unavailable;
        };
        let grant_id = match self.root.store_scope {
            SessionRetrievalStoreScope::Project => "grant.mcp.message-search.project",
            SessionRetrievalStoreScope::Profile => "grant.mcp.message-search.profile",
        };
        let authorizer = DaemonSessionRetrievalAuthorizer {
            identity: self.root.identity.clone(),
            session_id: query.session_id().clone(),
            retrieval_scope: query.retrieval_scope().clone(),
            temporal_mode: query.temporal_mode(),
            grain: query.grain(),
            provider: query.provider().map(str::to_owned),
            grant_id,
        };
        let Ok(execution) = self.registered_execution() else {
            return SessionRetrievalOutcome::Unavailable;
        };
        SessionRetrievalService::new(
            authorizer,
            execution,
            MessageSearchWordEstimator,
            self.configuration,
        )
        .retrieve(&context, &binding, query)
        .await
    }

    async fn execute_command(
        &self,
        command: SessionRetrievalCommand,
    ) -> SessionRetrievalServiceOutcome {
        if requires_refresh_worker(command.query().freshness_policy())
            && let Some(unavailable) = self.refresh_unavailable()
        {
            return SessionRetrievalServiceOutcome::Unavailable(unavailable);
        }
        if !self.root.owns(&command) {
            return SessionRetrievalServiceOutcome::WrongScope;
        }
        let outcome = self.execute_temporal_query(command.query().clone()).await;
        self.public_outcome(outcome).await
    }

    async fn public_outcome(
        &self,
        outcome: SessionRetrievalOutcome<TemporalKernelResult>,
    ) -> SessionRetrievalServiceOutcome {
        match outcome {
            SessionRetrievalOutcome::Complete { items, freshness } => {
                let (page, skipped, _) = self.page(items).await;
                complete_page_outcome(page, freshness, skipped)
            }
            SessionRetrievalOutcome::CompleteZero { freshness } => {
                SessionRetrievalServiceOutcome::CompleteZero {
                    temporal: self.empty_temporal(),
                    freshness,
                }
            }
            SessionRetrievalOutcome::Stale { freshness } => SessionRetrievalServiceOutcome::Stale {
                temporal: self.empty_temporal(),
                freshness,
            },
            SessionRetrievalOutcome::Partial {
                items,
                freshness,
                omitted,
            } => {
                let (page, _, rendering_omitted) = self.page(items).await;
                SessionRetrievalServiceOutcome::Partial {
                    page,
                    freshness,
                    omitted: omitted.saturating_add(rendering_omitted),
                }
            }
            SessionRetrievalOutcome::WrongScope => SessionRetrievalServiceOutcome::WrongScope,
            SessionRetrievalOutcome::Locked => SessionRetrievalServiceOutcome::Locked,
            SessionRetrievalOutcome::Redacted => SessionRetrievalServiceOutcome::Redacted,
            SessionRetrievalOutcome::Deleted => SessionRetrievalServiceOutcome::Deleted,
            SessionRetrievalOutcome::Denied => SessionRetrievalServiceOutcome::Denied,
            SessionRetrievalOutcome::Unavailable => SessionRetrievalServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::without_worker(
                    SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
                ),
            ),
            SessionRetrievalOutcome::CursorManifestLimitExceeded {
                kind,
                observed,
                maximum,
            } => SessionRetrievalServiceOutcome::CursorManifestLimitExceeded {
                kind,
                observed,
                maximum,
            },
            SessionRetrievalOutcome::BudgetExhausted => {
                SessionRetrievalServiceOutcome::BudgetExhausted
            }
            SessionRetrievalOutcome::Cancelled => SessionRetrievalServiceOutcome::Cancelled,
        }
    }

    fn empty_temporal(&self) -> SessionTemporalMetadataView {
        SessionTemporalMetadataView {
            authorized_root: self.root.authorized_root.clone(),
            ..SessionTemporalMetadataView::default()
        }
    }

    async fn page(&self, items: Vec<TemporalKernelResult>) -> (SessionRetrievalPageView, u64, u64) {
        let mut results = Vec::new();
        let mut anchors = Vec::new();
        let mut explanations = Vec::new();
        let mut omissions = Vec::new();
        let mut coverage = TemporalCoverageCountsV1::default();
        let mut source_coverage = Vec::new();
        let mut watermarks = SessionTemporalWatermarksView::default();
        let mut cursor = None;
        let mut skipped = 0u64;
        let mut rendering_omitted = 0u64;
        let mut sessions = PageSessionCache::default();
        for item in items {
            let item_watermarks = item.snapshot.watermarks();
            watermarks.generation = watermarks.generation.max(item_watermarks.generation);
            watermarks.source = watermarks.source.max(item_watermarks.source);
            watermarks.projection = watermarks.projection.max(item_watermarks.projection);
            watermarks.index = watermarks.index.max(item_watermarks.index);
            watermarks.summary = watermarks.summary.max(item_watermarks.summary);
            coverage.visible = coverage.visible.saturating_add(item.coverage.visible);
            coverage.hidden = coverage.hidden.saturating_add(item.coverage.hidden);
            coverage.unknown = coverage.unknown.saturating_add(item.coverage.unknown);
            coverage.redacted = coverage.redacted.saturating_add(item.coverage.redacted);
            if let Ok(receipt) = item.snapshot.source_coverage() {
                source_coverage.extend(receipt.sources().iter().cloned());
            }
            if item.next_cursor.is_some() {
                cursor = item.next_cursor.clone();
            }
            for (rank, ranked) in item.ranked.iter().enumerate() {
                let hydrated = match page_hydration_slot(rank, ranked, &item.hydrated) {
                    Ok(hydrated) => hydrated,
                    Err(omission) => {
                        skipped = skipped.saturating_add(1);
                        omissions.push(omission);
                        continue;
                    }
                };
                let Some(result) = self
                    .hydrate_result(&item.snapshot, ranked, hydrated, &mut sessions)
                    .await
                else {
                    skipped = skipped.saturating_add(1);
                    rendering_omitted = rendering_omitted.saturating_add(1);
                    coverage.unknown = coverage.unknown.saturating_add(1);
                    omissions.push(SessionRetrievalOmissionView {
                        rank: hydrated.rank(),
                        anchor: ranked.anchor_id.clone(),
                        reason: HydrationStateV1::RetainedButUnavailable,
                    });
                    continue;
                };
                anchors.push(ranked.anchor_id.clone());
                explanations.push(SessionRetrievalExplanationView {
                    anchor: ranked.anchor_id.clone(),
                    summary: format!(
                        "temporal rank {} at {}",
                        ranked.normalized_score_micros, ranked.knowledge_at_micros
                    ),
                });
                results.push(result);
            }
        }
        source_coverage.sort_by(|left, right| left.source_id().cmp(right.source_id()));
        source_coverage.dedup_by(|left, right| left.source_id() == right.source_id());
        (
            SessionRetrievalPageView {
                results,
                temporal: SessionTemporalMetadataView {
                    anchors,
                    watermarks,
                    coverage,
                    source_coverage,
                    cursor,
                    explanations,
                    omissions,
                    authorized_root: self.root.authorized_root.clone(),
                },
            },
            skipped,
            rendering_omitted,
        )
    }

    async fn hydrate_result(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        ranked: &tracedecay_temporal_query::ranking::RankedCandidate,
        hydrated: &TemporalHydratedResult,
        sessions: &mut PageSessionCache,
    ) -> Option<SessionMessageSearchResult> {
        let content = hydrated.content()?;
        let authorized_project_key = snapshot.request().authorized_root()?.project_key();
        if ranked.evidence_role.as_deref() == Some("summary") {
            let provider = ranked.source.as_deref()?;
            let session_id = ranked.session.as_deref()?;
            let summary_id = ranked
                .contributions
                .iter()
                .find(|contribution| {
                    contribution.channel
                        == tracedecay_temporal_query::candidates::CandidateChannel::Summary
                })?
                .retriever_record_id
                .clone();
            let text = std::str::from_utf8(content).ok()?.to_string();
            let session = sessions
                .resolve(
                    self.database.as_ref(),
                    authorized_project_key,
                    provider,
                    session_id,
                )
                .await?;
            return Some(SessionMessageSearchResult {
                session,
                message: crate::sessions::SessionMessageRecord {
                    provider: provider.to_string(),
                    message_id: summary_id,
                    session_id: session_id.to_string(),
                    role: "summary".to_string(),
                    timestamp: Some(ranked.knowledge_at_micros),
                    ordinal: 0,
                    text,
                    kind: Some("summary".to_string()),
                    model: None,
                    tool_names: None,
                    source_path: None,
                    source_offset: None,
                    metadata_json: Some(
                        json!({
                            "retrieval_anchor_id": ranked.anchor_id,
                            "retrieval_kind": "summary_node",
                        })
                        .to_string(),
                    ),
                },
                score: ranked.normalized_score_micros as f64 / 1_000_000.0,
            });
        }
        let provider = ranked.source.as_deref()?;
        let session_id = ranked.session.as_deref()?;
        let message = self
            .registered_execution()
            .ok()?
            .session_message_from_hydrated_occurrence(
                snapshot,
                &ranked.anchor_id,
                provider,
                session_id,
                content,
            )
            .await
            .ok()?;
        let session = sessions
            .resolve(
                self.database.as_ref(),
                authorized_project_key,
                provider,
                session_id,
            )
            .await?;
        if message.provider != provider
            || message.session_id != session_id
            || session.project_key != authorized_project_key
        {
            return None;
        }
        Some(SessionMessageSearchResult {
            session,
            message,
            score: ranked.normalized_score_micros as f64 / 1_000_000.0,
        })
    }

    fn lcm_authorization_binding(&self, provider: &str) -> String {
        format!(
            "sha256:{}",
            hex::encode(message_search_digest(
                b"tracedecay.mcp.lcm.authorization.v1\0",
                &self.root.identity,
                Some(provider),
            ))
        )
    }

    fn lcm_binding(
        &self,
        kind: &str,
        provider: &str,
        session_id: &SessionId,
        target: &str,
        grain: RetrievalGrainV1,
        content_slice: Option<LcmContentSlice>,
        source_limit: Option<usize>,
    ) -> String {
        let encoded = json!({
            "version": 1,
            "kind": kind,
            "provider": provider,
            "session_id": session_id.as_str(),
            "target": target,
            "grain": grain.as_str(),
            "content_offset": content_slice.map(|slice| slice.offset),
            "content_limit": content_slice.map(|slice| slice.limit),
            "source_limit": source_limit,
            "authorization": self.lcm_authorization_binding(provider),
        })
        .to_string();
        format!("sha256:{}", hex::encode(Sha256::digest(encoded.as_bytes())))
    }

    fn lcm_temporal_view(&self, result: &TemporalKernelResult) -> SessionTemporalMetadataView {
        let watermarks = result.snapshot.watermarks();
        SessionTemporalMetadataView {
            anchors: result
                .ranked
                .iter()
                .map(|ranked| ranked.anchor_id.clone())
                .collect(),
            watermarks: SessionTemporalWatermarksView {
                generation: watermarks.generation,
                source: watermarks.source,
                projection: watermarks.projection,
                index: watermarks.index,
                summary: watermarks.summary,
            },
            coverage: result.coverage,
            source_coverage: result
                .snapshot
                .source_coverage()
                .map(|receipt| receipt.sources().to_vec())
                .unwrap_or_default(),
            cursor: result.next_cursor.clone(),
            explanations: result
                .ranked
                .iter()
                .map(|ranked| SessionRetrievalExplanationView {
                    anchor: ranked.anchor_id.clone(),
                    summary: format!(
                        "temporal rank {} at {}",
                        ranked.normalized_score_micros, ranked.knowledge_at_micros
                    ),
                })
                .collect(),
            omissions: result
                .hydrated
                .iter()
                .filter(|hydrated| hydrated.state() != HydrationStateV1::Available)
                .map(|hydrated| SessionRetrievalOmissionView {
                    rank: hydrated.rank(),
                    anchor: hydrated.anchor_id().clone(),
                    reason: hydrated.state(),
                })
                .collect(),
            authorized_root: self.root.authorized_root.clone(),
        }
    }

    fn lcm_direct_query(
        &self,
        session_id: SessionId,
        provider: &str,
        grain: RetrievalGrainV1,
        temporal_mode: TemporalModeV1,
        retrieval_scope: SessionRetrievalScope,
        direct_anchor: Option<RetrievalAnchorId>,
        binding: String,
    ) -> Option<SessionTemporalQuery> {
        let query = SessionTemporalQuery::new(
            session_id,
            Some(provider.to_string()),
            "",
            None,
            temporal_mode,
            grain,
            1,
            DiversityLimits::unbounded(),
            ContextBudget {
                max_bytes: MESSAGE_SEARCH_MAX_BYTES,
                max_tokens: MESSAGE_SEARCH_MAX_BYTES / 4,
                estimator_version: "words-v1".to_string(),
            },
        )
        .ok()?
        .with_retrieval_scope(retrieval_scope)
        .with_compatibility_filter_digest(binding);
        Some(match direct_anchor {
            Some(anchor_id) => query.with_direct_anchor(anchor_id),
            None => query,
        })
    }

    async fn execute_lcm_describe(
        &self,
        command: LcmDescribeServiceCommand,
    ) -> LcmDescribeServiceOutcome {
        if command.store_scope() != self.root.store_scope {
            return LcmDescribeServiceOutcome::WrongScope;
        }
        let executor = match self.registered_execution() {
            Ok(executor) => executor,
            Err(error) => return describe_execution_error(error, self.empty_temporal()),
        };
        let target = command.target().clone();
        let direct_result = executor
            .resolve_lcm_describe_target(command.provider(), command.session_id(), &target)
            .await;
        let direct = match direct_result {
            Ok(direct) => direct,
            Err(error) => return describe_execution_error(error, self.empty_temporal()),
        };
        let binding = self.lcm_binding(
            "describe",
            command.provider(),
            command.session_id(),
            &lcm_describe_target_key(&target),
            command.grain(),
            None,
            None,
        );
        let temporal_mode = if direct.is_some() {
            TemporalModeV1::Current
        } else {
            TemporalModeV1::Forensic
        };
        let Some(query) = self.lcm_direct_query(
            command.session_id().clone(),
            command.provider(),
            command.grain(),
            temporal_mode,
            SessionRetrievalScope::Session(command.session_id().clone()),
            direct.as_ref().map(|direct| direct.anchor_id.clone()),
            binding,
        ) else {
            return LcmDescribeServiceOutcome::Denied;
        };
        let outcome = self.execute_temporal_query(query).await;
        let (result, retrieval) = match outcome {
            SessionRetrievalOutcome::Complete {
                mut items,
                freshness,
            } => (
                items.pop(),
                LcmRetrievalOutcome::complete(lcm_data_freshness(freshness)),
            ),
            SessionRetrievalOutcome::Partial {
                mut items,
                freshness,
                omitted,
            } => {
                let retrieval =
                    LcmRetrievalOutcome::partial(lcm_data_freshness(freshness), omitted);
                let Some(result) = items.pop() else {
                    return LcmDescribeServiceOutcome::Partial {
                        description: None,
                        temporal: self.empty_temporal(),
                        grain: command.grain(),
                        state: None,
                        lineage: Vec::new(),
                        retrieval,
                    };
                };
                (Some(result), retrieval)
            }
            SessionRetrievalOutcome::CompleteZero { freshness } if direct.is_none() => (
                None,
                LcmRetrievalOutcome::complete(lcm_data_freshness(freshness)),
            ),
            SessionRetrievalOutcome::CompleteZero { .. } => {
                return LcmDescribeServiceOutcome::Deleted;
            }
            terminal => {
                return describe_retrieval_outcome(
                    terminal,
                    command.grain(),
                    self.empty_temporal(),
                );
            }
        };
        let state = match (direct.as_ref(), result.as_ref()) {
            (Some(direct), Some(result)) => match hydration_state(result, &direct.anchor_id) {
                Some(HydrationStateV1::Available) => HydrationStateV1::Available,
                Some(state) => return describe_hydration_state(state),
                None => {
                    return LcmDescribeServiceOutcome::Unavailable(
                        SessionRetrievalUnavailable::without_worker(
                            SessionRetrievalUnavailableReason::HydrationUnavailable,
                        ),
                    );
                }
            },
            (Some(_), None) => return LcmDescribeServiceOutcome::Deleted,
            (None, _) => HydrationStateV1::Available,
        };
        let request = LcmDescribeRequest {
            provider: command.provider().to_string(),
            session_id: command.session_id().as_str().to_string(),
            target,
        };
        let rendered = executor.render_lcm_describe(request).await;
        let description = match rendered {
            Ok(description) => description,
            Err(error) => return describe_execution_error(error, self.empty_temporal()),
        };
        let temporal = result.as_ref().map_or_else(
            || self.empty_temporal(),
            |result| self.lcm_temporal_view(result),
        );
        let lineage = result.map_or_else(Vec::new, |result| result.lineage);
        match retrieval {
            LcmRetrievalOutcome::Complete { .. } => LcmDescribeServiceOutcome::Complete {
                description,
                temporal,
                grain: command.grain(),
                state,
                lineage,
                retrieval,
            },
            LcmRetrievalOutcome::Partial { .. } => LcmDescribeServiceOutcome::Partial {
                description: Some(description),
                temporal,
                grain: command.grain(),
                state: Some(state),
                lineage,
                retrieval,
            },
            LcmRetrievalOutcome::Stale { .. } => LcmDescribeServiceOutcome::Stale {
                temporal,
                retrieval,
            },
        }
    }

    fn lcm_expand_target_key(target: &LcmExpandTarget) -> String {
        match target {
            LcmExpandTarget::RawMessage { store_id } => format!("raw:{store_id}"),
            LcmExpandTarget::SummaryNode { node_id } => format!("summary:{node_id}"),
            LcmExpandTarget::ExternalPayload { payload_ref } => {
                format!("payload:{payload_ref}")
            }
        }
    }

    async fn execute_lcm_expand(
        &self,
        command: LcmExpandServiceCommand,
    ) -> LcmExpandServiceOutcome {
        if command.store_scope() != self.root.store_scope {
            return LcmExpandServiceOutcome::WrongScope;
        }
        let executor = match self.registered_execution() {
            Ok(executor) => executor,
            Err(error) => return expand_execution_error(error, self.empty_temporal()),
        };
        let target = command.target().clone();
        let direct_result = executor
            .resolve_lcm_expand_target(command.provider(), command.session_id(), &target)
            .await;
        let direct = match direct_result {
            Ok(direct) => direct,
            Err(SessionTemporalExecutionError::Deleted) if command.cursor().is_some() => {
                return LcmExpandServiceOutcome::Denied;
            }
            Err(error) => return expand_execution_error(error, self.empty_temporal()),
        };
        let binding = self.lcm_binding(
            "expand",
            command.provider(),
            command.session_id(),
            &Self::lcm_expand_target_key(&target),
            command.grain(),
            Some(command.content_slice()),
            command.source_limit(),
        );
        let retrieval_scope = if matches!(&target, LcmExpandTarget::RawMessage { .. })
            && direct.owner_session_id.as_str() != command.session_id().as_str()
        {
            SessionRetrievalScope::AllSessionsInAuthorizedRoot
        } else {
            SessionRetrievalScope::Session(command.session_id().clone())
        };
        let Some(query) = self.lcm_direct_query(
            command.session_id().clone(),
            command.provider(),
            command.grain(),
            TemporalModeV1::Current,
            retrieval_scope,
            Some(direct.anchor_id.clone()),
            binding.clone(),
        ) else {
            return LcmExpandServiceOutcome::Denied;
        };
        let outcome = self.execute_temporal_query(query).await;
        let (result, retrieval) = match outcome {
            SessionRetrievalOutcome::Complete {
                mut items,
                freshness,
            } => match items.pop() {
                Some(result) => (
                    result,
                    LcmRetrievalOutcome::complete(lcm_data_freshness(freshness)),
                ),
                None => return LcmExpandServiceOutcome::Deleted,
            },
            SessionRetrievalOutcome::Partial {
                mut items,
                freshness,
                omitted,
            } => {
                let retrieval =
                    LcmRetrievalOutcome::partial(lcm_data_freshness(freshness), omitted);
                let Some(result) = items.pop() else {
                    return LcmExpandServiceOutcome::Partial {
                        expansion: None,
                        temporal: self.empty_temporal(),
                        grain: command.grain(),
                        state: None,
                        retrieval,
                    };
                };
                (result, retrieval)
            }
            SessionRetrievalOutcome::CompleteZero { .. } => {
                return LcmExpandServiceOutcome::Deleted;
            }
            terminal => {
                return expand_retrieval_outcome(terminal, command.grain(), self.empty_temporal());
            }
        };
        let canonical_content = match hydration_state(&result, &direct.anchor_id) {
            Some(HydrationStateV1::Available) => result
                .hydrated
                .iter()
                .find(|hydrated| hydrated.anchor_id() == &direct.anchor_id)
                .and_then(|hydrated| hydrated.content())
                .and_then(|content| std::str::from_utf8(content).ok()),
            Some(state) => return expand_hydration_state(state),
            None => None,
        };
        let Some(canonical_content) = canonical_content else {
            return LcmExpandServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::without_worker(
                    SessionRetrievalUnavailableReason::HydrationUnavailable,
                ),
            );
        };
        let source_offset = match command.cursor() {
            Some(cursor) => match executor
                .decode_lcm_source_cursor(&result.snapshot, &binding, cursor)
                .await
            {
                Ok(offset) => offset,
                Err(error) => return expand_execution_error(error, self.empty_temporal()),
            },
            None => command.source_offset(),
        };
        let request = LcmExpandRequest {
            provider: command.provider().to_string(),
            session_id: command.session_id().as_str().to_string(),
            target,
            content_slice: Some(command.content_slice()),
            source_offset,
            source_limit: command.source_limit(),
        };
        let rendered = executor.render_lcm_expand(request, canonical_content).await;
        let mut expansion = match rendered {
            Ok(expansion) => expansion,
            Err(error) => return expand_execution_error(error, self.empty_temporal()),
        };
        if let Err(error) = executor
            .hydrate_lcm_summary_sources(
                &result.snapshot,
                command.provider(),
                command.session_id(),
                command.content_slice(),
                &mut expansion,
            )
            .await
        {
            return expand_execution_error(error, self.empty_temporal());
        }
        let mut temporal = self.lcm_temporal_view(&result);
        if let Some(offset) = expansion
            .source_pagination
            .as_ref()
            .and_then(|pagination| pagination.next_source_offset)
        {
            match executor
                .encode_lcm_source_cursor(&result.snapshot, &binding, offset)
                .await
            {
                Ok(cursor) => temporal.cursor = Some(cursor),
                Err(error) => return expand_execution_error(error, self.empty_temporal()),
            }
        }
        let summary_source_omitted = u64::try_from(
            expansion
                .summary_sources
                .iter()
                .filter(|source| source.state != HydrationStateV1::Available)
                .count(),
        )
        .unwrap_or(u64::MAX);
        let retrieval = match retrieval {
            LcmRetrievalOutcome::Complete { freshness } if summary_source_omitted > 0 => {
                LcmRetrievalOutcome::partial(freshness, summary_source_omitted)
            }
            LcmRetrievalOutcome::Partial { freshness, omitted } => LcmRetrievalOutcome::partial(
                freshness,
                omitted.saturating_add(summary_source_omitted),
            ),
            retrieval => retrieval,
        };
        match retrieval {
            LcmRetrievalOutcome::Complete { .. } => LcmExpandServiceOutcome::Complete {
                expansion,
                temporal,
                grain: command.grain(),
                state: HydrationStateV1::Available,
                retrieval,
            },
            LcmRetrievalOutcome::Partial { .. } => LcmExpandServiceOutcome::Partial {
                expansion: Some(expansion),
                temporal,
                grain: command.grain(),
                state: Some(HydrationStateV1::Available),
                retrieval,
            },
            LcmRetrievalOutcome::Stale { .. } => LcmExpandServiceOutcome::Stale {
                temporal,
                retrieval,
            },
        }
    }
}

impl SessionRetrievalServicePort for DaemonSessionRetrievalService {
    fn execute(&self, command: SessionRetrievalCommand) -> SessionRetrievalServiceFuture<'_> {
        Box::pin(async move { self.execute_command(command).await })
    }

    fn describe_lcm(&self, command: LcmDescribeServiceCommand) -> LcmDescribeServiceFuture<'_> {
        Box::pin(async move { self.execute_lcm_describe(command).await })
    }

    fn expand_lcm(&self, command: LcmExpandServiceCommand) -> LcmExpandServiceFuture<'_> {
        Box::pin(async move { self.execute_lcm_expand(command).await })
    }
}

fn lcm_describe_target_key(target: &LcmDescribeTarget) -> String {
    match target {
        LcmDescribeTarget::Session => "session".to_string(),
        LcmDescribeTarget::SummaryNode { node_id } => format!("summary:{node_id}"),
        LcmDescribeTarget::ExternalPayload { payload_ref } => format!("payload:{payload_ref}"),
    }
}

const fn lcm_data_freshness(freshness: SessionDataFreshness) -> LcmDataFreshness {
    match freshness {
        SessionDataFreshness::Fresh => LcmDataFreshness::Fresh,
        SessionDataFreshness::Stored { generation_lag } => {
            LcmDataFreshness::Stored { generation_lag }
        }
        SessionDataFreshness::Partial { generation_lag } => {
            LcmDataFreshness::Partial { generation_lag }
        }
    }
}

fn hydration_state(
    result: &TemporalKernelResult,
    anchor_id: &RetrievalAnchorId,
) -> Option<HydrationStateV1> {
    result
        .hydrated
        .iter()
        .find(|hydrated| hydrated.anchor_id() == anchor_id)
        .map(tracedecay_temporal_query::TemporalHydratedResult::state)
}

fn describe_hydration_state(state: HydrationStateV1) -> LcmDescribeServiceOutcome {
    match state {
        HydrationStateV1::Locked => LcmDescribeServiceOutcome::Locked,
        HydrationStateV1::Redacted => LcmDescribeServiceOutcome::Redacted,
        HydrationStateV1::Deleted | HydrationStateV1::RetentionExpired => {
            LcmDescribeServiceOutcome::Deleted
        }
        HydrationStateV1::Unauthorized => LcmDescribeServiceOutcome::Denied,
        HydrationStateV1::Available
        | HydrationStateV1::RetainedButUnavailable
        | HydrationStateV1::UnverifiableLegacy => {
            LcmDescribeServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::HydrationUnavailable,
            ))
        }
    }
}

fn expand_hydration_state(state: HydrationStateV1) -> LcmExpandServiceOutcome {
    match state {
        HydrationStateV1::Locked => LcmExpandServiceOutcome::Locked,
        HydrationStateV1::Redacted => LcmExpandServiceOutcome::Redacted,
        HydrationStateV1::Deleted | HydrationStateV1::RetentionExpired => {
            LcmExpandServiceOutcome::Deleted
        }
        HydrationStateV1::Unauthorized => LcmExpandServiceOutcome::Denied,
        HydrationStateV1::Available
        | HydrationStateV1::RetainedButUnavailable
        | HydrationStateV1::UnverifiableLegacy => {
            LcmExpandServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::HydrationUnavailable,
            ))
        }
    }
}

fn describe_execution_error(
    error: SessionTemporalExecutionError,
    temporal: SessionTemporalMetadataView,
) -> LcmDescribeServiceOutcome {
    match error {
        SessionTemporalExecutionError::Locked => LcmDescribeServiceOutcome::Locked,
        SessionTemporalExecutionError::Redacted => LcmDescribeServiceOutcome::Redacted,
        SessionTemporalExecutionError::Deleted => LcmDescribeServiceOutcome::Deleted,
        SessionTemporalExecutionError::WrongScope => LcmDescribeServiceOutcome::WrongScope,
        SessionTemporalExecutionError::Denied => LcmDescribeServiceOutcome::Denied,
        SessionTemporalExecutionError::BudgetExhausted => {
            LcmDescribeServiceOutcome::BudgetExhausted
        }
        SessionTemporalExecutionError::Cancelled => LcmDescribeServiceOutcome::Cancelled,
        SessionTemporalExecutionError::Stale { generation_lag } => {
            LcmDescribeServiceOutcome::Stale {
                temporal,
                retrieval: LcmRetrievalOutcome::stale(LcmDataFreshness::Stored { generation_lag }),
            }
        }
        SessionTemporalExecutionError::Unavailable
        | SessionTemporalExecutionError::Empty { .. }
        | SessionTemporalExecutionError::Kernel(_) => {
            LcmDescribeServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
            ))
        }
    }
}

fn expand_execution_error(
    error: SessionTemporalExecutionError,
    temporal: SessionTemporalMetadataView,
) -> LcmExpandServiceOutcome {
    match error {
        SessionTemporalExecutionError::Locked => LcmExpandServiceOutcome::Locked,
        SessionTemporalExecutionError::Redacted => LcmExpandServiceOutcome::Redacted,
        SessionTemporalExecutionError::Deleted => LcmExpandServiceOutcome::Deleted,
        SessionTemporalExecutionError::WrongScope => LcmExpandServiceOutcome::WrongScope,
        SessionTemporalExecutionError::Denied => LcmExpandServiceOutcome::Denied,
        SessionTemporalExecutionError::BudgetExhausted => LcmExpandServiceOutcome::BudgetExhausted,
        SessionTemporalExecutionError::Cancelled => LcmExpandServiceOutcome::Cancelled,
        SessionTemporalExecutionError::Stale { generation_lag } => LcmExpandServiceOutcome::Stale {
            temporal,
            retrieval: LcmRetrievalOutcome::stale(LcmDataFreshness::Stored { generation_lag }),
        },
        SessionTemporalExecutionError::Unavailable
        | SessionTemporalExecutionError::Empty { .. }
        | SessionTemporalExecutionError::Kernel(_) => {
            LcmExpandServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
            ))
        }
    }
}

fn describe_retrieval_outcome(
    outcome: SessionRetrievalOutcome<TemporalKernelResult>,
    grain: RetrievalGrainV1,
    temporal: SessionTemporalMetadataView,
) -> LcmDescribeServiceOutcome {
    match outcome {
        SessionRetrievalOutcome::WrongScope => LcmDescribeServiceOutcome::WrongScope,
        SessionRetrievalOutcome::Locked => LcmDescribeServiceOutcome::Locked,
        SessionRetrievalOutcome::Redacted => LcmDescribeServiceOutcome::Redacted,
        SessionRetrievalOutcome::Deleted => LcmDescribeServiceOutcome::Deleted,
        SessionRetrievalOutcome::Denied => LcmDescribeServiceOutcome::Denied,
        SessionRetrievalOutcome::BudgetExhausted => LcmDescribeServiceOutcome::BudgetExhausted,
        SessionRetrievalOutcome::CursorManifestLimitExceeded { .. } => {
            LcmDescribeServiceOutcome::BudgetExhausted
        }
        SessionRetrievalOutcome::Cancelled => LcmDescribeServiceOutcome::Cancelled,
        SessionRetrievalOutcome::Stale { freshness } => LcmDescribeServiceOutcome::Stale {
            temporal,
            retrieval: LcmRetrievalOutcome::stale(lcm_data_freshness(freshness)),
        },
        SessionRetrievalOutcome::Partial {
            freshness, omitted, ..
        } => LcmDescribeServiceOutcome::Partial {
            description: None,
            temporal,
            grain,
            state: None,
            lineage: Vec::new(),
            retrieval: LcmRetrievalOutcome::partial(lcm_data_freshness(freshness), omitted),
        },
        SessionRetrievalOutcome::Unavailable
        | SessionRetrievalOutcome::Complete { .. }
        | SessionRetrievalOutcome::CompleteZero { .. } => {
            LcmDescribeServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
            ))
        }
    }
}

fn expand_retrieval_outcome(
    outcome: SessionRetrievalOutcome<TemporalKernelResult>,
    grain: RetrievalGrainV1,
    temporal: SessionTemporalMetadataView,
) -> LcmExpandServiceOutcome {
    match outcome {
        SessionRetrievalOutcome::WrongScope => LcmExpandServiceOutcome::WrongScope,
        SessionRetrievalOutcome::Locked => LcmExpandServiceOutcome::Locked,
        SessionRetrievalOutcome::Redacted => LcmExpandServiceOutcome::Redacted,
        SessionRetrievalOutcome::Deleted => LcmExpandServiceOutcome::Deleted,
        SessionRetrievalOutcome::Denied => LcmExpandServiceOutcome::Denied,
        SessionRetrievalOutcome::BudgetExhausted => LcmExpandServiceOutcome::BudgetExhausted,
        SessionRetrievalOutcome::CursorManifestLimitExceeded { .. } => {
            LcmExpandServiceOutcome::BudgetExhausted
        }
        SessionRetrievalOutcome::Cancelled => LcmExpandServiceOutcome::Cancelled,
        SessionRetrievalOutcome::Stale { freshness } => LcmExpandServiceOutcome::Stale {
            temporal,
            retrieval: LcmRetrievalOutcome::stale(lcm_data_freshness(freshness)),
        },
        SessionRetrievalOutcome::Partial {
            freshness, omitted, ..
        } => LcmExpandServiceOutcome::Partial {
            expansion: None,
            temporal,
            grain,
            state: None,
            retrieval: LcmRetrievalOutcome::partial(lcm_data_freshness(freshness), omitted),
        },
        SessionRetrievalOutcome::Unavailable
        | SessionRetrievalOutcome::Complete { .. }
        | SessionRetrievalOutcome::CompleteZero { .. } => {
            LcmExpandServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
            ))
        }
    }
}

fn message_search_digest(
    domain: &[u8],
    identity: &ResolvedSessionIdentity,
    provider: Option<&str>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(identity.profile_id().as_str().as_bytes());
    digest.update([0]);
    if let Some(project_id) = identity.project_id() {
        digest.update(project_id.as_str().as_bytes());
    }
    digest.update([0]);
    digest.update(identity.store_id().as_str().as_bytes());
    digest.update([0]);
    digest.update(identity.root_id().as_str().as_bytes());
    if let Some(route) = identity.git_route() {
        digest.update([0]);
        digest.update(route.repository_id().as_str().as_bytes());
        digest.update([0]);
        digest.update(route.worktree_id().as_str().as_bytes());
        digest.update([0]);
        digest.update(route.branch_id().as_str().as_bytes());
    }
    if let Some(provider) = provider {
        digest.update([0]);
        digest.update(provider.as_bytes());
    }
    digest.finalize().into()
}

/// Message-search policy digest.
///
/// The digested value is a pair of compile-time constants, so it is the same
/// bytes for every request. It is derived once and reused instead of running a
/// canonical encode plus SHA-256 on each message-search request.
fn message_search_policy_digest() -> Option<[u8; 32]> {
    static POLICY_DIGEST: std::sync::OnceLock<Option<[u8; 32]>> = std::sync::OnceLock::new();
    *POLICY_DIGEST.get_or_init(|| {
        let encoded = PayloadReferenceV1::for_payload(&json!({
            "domain": "tracedecay.observation-anchor.authorization.v1",
            "authority": "observation-capture.v1",
        }))
        .ok()?;
        let digest = encoded.digest().as_str().strip_prefix("sha256:")?;
        hex::decode(digest).ok()?.try_into().ok()
    })
}

fn complete_page_outcome(
    page: SessionRetrievalPageView,
    freshness: SessionDataFreshness,
    omitted: u64,
) -> SessionRetrievalServiceOutcome {
    if omitted == 0 {
        SessionRetrievalServiceOutcome::Complete { page, freshness }
    } else {
        SessionRetrievalServiceOutcome::Partial {
            page,
            freshness,
            omitted,
        }
    }
}

fn page_hydration_slot<'a>(
    rank: usize,
    ranked: &RankedCandidate,
    hydrated: &'a [TemporalHydratedResult],
) -> Result<&'a TemporalHydratedResult, SessionRetrievalOmissionView> {
    let Ok(rank) = u32::try_from(rank) else {
        return Err(SessionRetrievalOmissionView {
            rank: u32::MAX,
            anchor: ranked.anchor_id.clone(),
            reason: HydrationStateV1::RetainedButUnavailable,
        });
    };
    let Some(hydrated) = hydrated.get(rank as usize).filter(|hydrated| {
        hydrated.rank() == rank
            && hydrated.stable_id() == ranked.stable_id
            && hydrated.anchor_id() == &ranked.anchor_id
    }) else {
        return Err(SessionRetrievalOmissionView {
            rank,
            anchor: ranked.anchor_id.clone(),
            reason: HydrationStateV1::RetainedButUnavailable,
        });
    };
    if hydrated.state() != HydrationStateV1::Available {
        return Err(SessionRetrievalOmissionView {
            rank,
            anchor: ranked.anchor_id.clone(),
            reason: hydrated.state(),
        });
    }
    Ok(hydrated)
}

/// Session records already read while rendering the current page.
///
/// One page routinely ranks many results out of the same session, and every
/// unique lookup costs its own read snapshot, so the record is read once per
/// distinct authorized identity instead of once per rendered result. A lookup
/// that finds nothing is remembered too: repeating it cannot turn an absent
/// session into a present one, and re-reading would only let one page render
/// the same session inconsistently.
#[derive(Default)]
struct PageSessionCache {
    sessions: HashMap<(String, String, String), Option<SessionRecord>>,
}

impl PageSessionCache {
    async fn resolve(
        &mut self,
        database: &RegisteredGlobalDb,
        project_key: &str,
        provider: &str,
        session_id: &str,
    ) -> Option<SessionRecord> {
        let key = (
            project_key.to_string(),
            provider.to_string(),
            session_id.to_string(),
        );
        if let Some(cached) = self.sessions.get(&key) {
            return cached.clone();
        }
        let session = registered_session(database, project_key, provider, session_id).await;
        self.sessions.insert(key, session.clone());
        session
    }
}

async fn registered_session(
    database: &RegisteredGlobalDb,
    project_key: &str,
    provider: &str,
    session_id: &str,
) -> Option<SessionRecord> {
    let snapshot = database.read_snapshot().await.ok()?;
    let mut rows = snapshot
        .query(
            "SELECT provider, session_id, project_key, project_path, title, started_at,
                    ended_at, transcript_path, metadata_json, parent_session_id,
                    is_subagent, agent_id, parent_tool_use_id
             FROM sessions
             WHERE project_key = ?1 AND provider = ?2 AND session_id = ?3",
            crate::db::engine::params![project_key, provider, session_id],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    Some(SessionRecord {
        provider: row.get(0).ok()?,
        session_id: row.get(1).ok()?,
        project_key: row.get(2).ok()?,
        project_path: row.get(3).ok()?,
        title: row.get(4).ok(),
        started_at: row.get(5).ok(),
        ended_at: row.get(6).ok(),
        transcript_path: row.get(7).ok(),
        metadata_json: row.get(8).ok(),
        parent_session_id: row.get(9).ok(),
        is_subagent: row.get::<i64>(10).unwrap_or_default() != 0,
        agent_id: row.get(11).ok(),
        parent_tool_use_id: row.get(12).ok(),
    })
}

#[cfg(test)]
mod stored_refresh_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_retrieval_does_not_require_refresh_worker() {
        assert!(!requires_refresh_worker(
            SessionFreshnessPolicy::AllowStored
        ));
        assert!(requires_refresh_worker(
            SessionFreshnessPolicy::RequireFresh
        ));
    }

    fn typed<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("typed test identity")
    }

    #[test]
    fn registered_profile_binding_replaces_the_legacy_request_profile_identity() {
        let brain_id = typed::<tracedecay_domain::BrainId>("brain.session-retrieval");
        let profile_id =
            typed::<tracedecay_domain::UserProfileId>("profile.durable-session-retrieval");
        let root = DaemonSessionRetrievalRoot::profile().expect("profile root");
        assert_eq!(
            root.identity.profile_id().as_str(),
            MESSAGE_SEARCH_PROFILE_ID
        );

        let root = root
            .with_profile_runtime_identity(brain_id.clone(), profile_id.clone())
            .expect("durable profile binding");

        assert_eq!(root.identity.profile_id().as_str(), profile_id.as_str());
        assert_eq!(
            root.expected_runtime_shard,
            Some(StoreShardIdV1::profile_sessions(brain_id, profile_id))
        );
    }

    #[test]
    fn registered_project_binding_uses_one_durable_profile_and_typed_project() {
        let brain_id = typed::<tracedecay_domain::BrainId>("brain.session-retrieval");
        let profile_id =
            typed::<tracedecay_domain::UserProfileId>("profile.durable-session-retrieval");
        let project_id = ProjectId::new("project.session-retrieval").expect("project identity");
        let identity = ResolvedSessionIdentity::for_project(
            ProfileId::new(MESSAGE_SEARCH_PROFILE_ID).expect("legacy profile"),
            project_id.clone(),
            SessionStoreId::new("store.project.test").expect("store identity"),
            SessionRootId::new("root.project.test").expect("root identity"),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.project.test").expect("repository identity"),
                WorktreeId::new("/project/test").expect("worktree identity"),
                BranchId::new("branch.project.test").expect("branch identity"),
            ),
        );
        let root = DaemonSessionRetrievalRoot {
            store_scope: SessionRetrievalStoreScope::Project,
            identity,
            project_id: Some(project_id.as_str().to_owned()),
            project_paths: HashSet::new(),
            authorized_root: None,
            expected_runtime_shard: None,
        }
        .with_project_runtime_identity(brain_id.clone(), profile_id.clone())
        .expect("durable project binding");

        assert_eq!(root.identity.profile_id().as_str(), profile_id.as_str());
        assert_eq!(root.identity.project_id(), Some(&project_id));
        assert_eq!(
            root.expected_runtime_shard,
            Some(StoreShardIdV1::project_sessions(
                brain_id, profile_id, project_id,
            ))
        );
    }

    #[test]
    fn denied_shared_anchor_stays_at_its_rank_without_promoting_lower_candidate() {
        fn ranked(stable_id: &str, anchor: &RetrievalAnchorId) -> RankedCandidate {
            RankedCandidate {
                stable_id: stable_id.to_string(),
                anchor_id: anchor.clone(),
                normalized_score_micros: 1,
                knowledge_at_micros: 1,
                logical_message: None,
                turn: None,
                session: Some(format!("session.{stable_id}")),
                source: Some("cursor".to_string()),
                evidence_role: Some("assistant".to_string()),
                contributions: Vec::new(),
            }
        }

        let anchor = RetrievalAnchorId::new("anchor.shared").unwrap();
        let selected = [ranked("denied", &anchor), ranked("lower", &anchor)];
        let hydrated = [
            TemporalHydratedResult::unavailable_for_test(
                0,
                "denied",
                anchor.clone(),
                HydrationStateV1::Unauthorized,
            ),
            TemporalHydratedResult::available_for_test(
                1,
                "lower",
                anchor.clone(),
                b"lower candidate".to_vec(),
            ),
        ];

        let omission = page_hydration_slot(0, &selected[0], &hydrated).unwrap_err();
        assert_eq!(omission.rank, 0);
        assert_eq!(omission.anchor, anchor);
        assert_eq!(omission.reason, HydrationStateV1::Unauthorized);

        let lower = page_hydration_slot(1, &selected[1], &hydrated).unwrap();
        assert_eq!(lower.rank(), 1);
        assert_eq!(lower.stable_id(), "lower");
    }

    #[test]
    fn complete_page_with_typed_omission_becomes_partial_and_keeps_coverage() {
        let anchor = RetrievalAnchorId::new("anchor.omitted").unwrap();
        let page = SessionRetrievalPageView {
            results: Vec::new(),
            temporal: SessionTemporalMetadataView {
                coverage: TemporalCoverageCountsV1 {
                    visible: 0,
                    hidden: 0,
                    unknown: 1,
                    redacted: 0,
                },
                omissions: vec![SessionRetrievalOmissionView {
                    rank: 0,
                    anchor: anchor.clone(),
                    reason: HydrationStateV1::Unauthorized,
                }],
                ..SessionTemporalMetadataView::default()
            },
        };

        let SessionRetrievalServiceOutcome::Partial {
            page,
            freshness,
            omitted,
        } = complete_page_outcome(page, SessionDataFreshness::Fresh, 1)
        else {
            panic!("complete page with an omission must become partial");
        };
        assert_eq!(freshness, SessionDataFreshness::Fresh);
        assert_eq!(omitted, 1);
        assert_eq!(page.temporal.coverage.unknown, 1);
        assert_eq!(page.temporal.omissions[0].rank, 0);
        assert_eq!(page.temporal.omissions[0].anchor, anchor);
        assert_eq!(
            page.temporal.omissions[0].reason,
            HydrationStateV1::Unauthorized
        );
    }

    #[test]
    fn stale_lcm_retrieval_remains_typed_instead_of_generic_unavailable() {
        let freshness = SessionDataFreshness::Stored { generation_lag: 7 };

        let describe = describe_retrieval_outcome(
            SessionRetrievalOutcome::Stale { freshness },
            RetrievalGrainV1::Summary,
            SessionTemporalMetadataView::default(),
        );
        let expand = expand_retrieval_outcome(
            SessionRetrievalOutcome::Stale { freshness },
            RetrievalGrainV1::Summary,
            SessionTemporalMetadataView::default(),
        );

        assert!(matches!(
            describe,
            LcmDescribeServiceOutcome::Stale {
                retrieval: LcmRetrievalOutcome::Stale {
                    freshness: LcmDataFreshness::Stored { generation_lag: 7 }
                },
                ..
            }
        ));
        assert!(matches!(
            expand,
            LcmExpandServiceOutcome::Stale {
                retrieval: LcmRetrievalOutcome::Stale {
                    freshness: LcmDataFreshness::Stored { generation_lag: 7 }
                },
                ..
            }
        ));
    }

    #[test]
    fn zero_item_partial_lcm_retrieval_remains_partial_instead_of_deleted() {
        let freshness = SessionDataFreshness::Partial { generation_lag: 3 };

        let describe = describe_retrieval_outcome(
            SessionRetrievalOutcome::Partial {
                items: Vec::new(),
                freshness,
                omitted: 5,
            },
            RetrievalGrainV1::Summary,
            SessionTemporalMetadataView::default(),
        );
        let expand = expand_retrieval_outcome(
            SessionRetrievalOutcome::Partial {
                items: Vec::new(),
                freshness,
                omitted: 5,
            },
            RetrievalGrainV1::Summary,
            SessionTemporalMetadataView::default(),
        );

        assert!(matches!(
            describe,
            LcmDescribeServiceOutcome::Partial {
                description: None,
                retrieval: LcmRetrievalOutcome::Partial {
                    freshness: LcmDataFreshness::Partial { generation_lag: 3 },
                    omitted: 5,
                },
                ..
            }
        ));
        assert!(matches!(
            expand,
            LcmExpandServiceOutcome::Partial {
                expansion: None,
                retrieval: LcmRetrievalOutcome::Partial {
                    freshness: LcmDataFreshness::Partial { generation_lag: 3 },
                    omitted: 5,
                },
                ..
            }
        ));
    }
}
