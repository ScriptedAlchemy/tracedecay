//! Daemon-owned session retrieval service: retrieval-root
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
    ActorId, HydrationStateV1, PayloadReferenceV1, ProjectId, RetrievalAnchorId, RetrievalGrainV1,
    SessionId, TemporalCoverageCountsV1, TemporalModeV1, UtcMicros,
};
#[cfg(any(test, feature = "test-transport"))]
use tracedecay_domain::{RepositoryId, WorktreeId};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_sessions::lcm::contracts::{LcmDataFreshness, LcmRetrievalOutcome};
use tracedecay_store::StoreShardIdV1;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::context::{
    BranchId, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId, RequestBudgets,
    ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use tracedecay_usecases::session::{
    AuthorizationGrantId, SessionAccess, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionDataFreshness, SessionFreshnessPolicy, SessionProjectionServingStatusPort,
    SessionRequestBinding, SessionRetrievalConfiguration, SessionRetrievalOutcome,
    SessionRetrievalScope, SessionRetrievalService, SessionScopeAuthorizationRequest,
    SessionScopeAuthorizer, SessionTemporalExecutionError, SessionTemporalQuery,
};

use crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake;
use crate::global_db::session_temporal::RegisteredGlobalDbSessionTemporalExecution;
use crate::global_db::{ProjectRegistryContext, RegisteredGlobalDb};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use crate::tracedecay::TraceDecay;
use tracedecay_sessions::runtime::{SessionMessageSearchResult, SessionRecord};
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

mod serving_status;
const MESSAGE_SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const MESSAGE_SEARCH_MAX_RESULTS: u64 = 1_024;
const MESSAGE_SEARCH_MAX_BYTES: u64 = 16 * 1024 * 1024;
const MESSAGE_SEARCH_MAX_WORK_UNITS: u64 = 100_000;

mod admitted;
mod contract;
pub(crate) use admitted::{SessionApplicationRetrievalFutureV1, SessionApplicationRetrievalPortV1};
pub(crate) use contract::{
    LcmDescribeServiceCommand, LcmDescribeServiceFuture, LcmDescribeServiceOutcome,
    LcmExpandServiceCommand, LcmExpandServiceFuture, LcmExpandServiceOutcome,
    SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalFilters,
    SessionRetrievalNextActionView, SessionRetrievalOmissionView, SessionRetrievalPageView,
    SessionRetrievalProjectSelector, SessionRetrievalServiceFuture, SessionRetrievalServiceOutcome,
    SessionRetrievalServicePort, SessionRetrievalStoreScope, SessionRetrievalSweepFuture,
    SessionRetrievalSweepOutcome, SessionRetrievalSweepPort, SessionRetrievalSweepRootView,
    SessionRetrievalSweepSkipReason, SessionRetrievalSweepSkipView, SessionRetrievalUnavailable,
    SessionRetrievalUnavailableReason, SessionRetrievalWorkerBlocker,
    SessionRetrievalWorkerRetryClass, SessionRetrievalWorkerStatusView,
    SessionTemporalMetadataView, SessionTemporalWatermarksView,
};

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
    pub(crate) fn identity(&self) -> &ResolvedSessionIdentity {
        &self.identity
    }

    pub(crate) fn expected_runtime_shard(&self) -> Option<&StoreShardIdV1> {
        self.expected_runtime_shard.as_ref()
    }

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
                    selected = Some((
                        store.store.store_id.clone(),
                        scope.graph_scope_id.clone(),
                        scope.branch_name.clone(),
                    ));
                }
            }
        }
        let (store_id, graph_scope_id, branch_name) = selected?;

        let project_key = ProjectId::new(context.project.project_id.clone()).ok()?;
        let repository_id =
            crate::daemon::code_index_scheduler::identity::repository_id_for(cg.project_root())
                .ok()?;
        let worktree_id =
            crate::daemon::code_index_scheduler::identity::worktree_id_for(cg.project_root())
                .ok()?;
        let identity = ResolvedSessionIdentity::for_project(
            ProfileId::new(MESSAGE_SEARCH_PROFILE_ID).ok()?,
            project_key,
            SessionStoreId::new(store_id).ok()?,
            SessionRootId::new(graph_scope_id.clone()).ok()?,
            ResolvedGitRoute::new(repository_id, worktree_id, BranchId::new(branch_name).ok()?),
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
    actor: ActorId,
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
        if context.actor() != &self.actor
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

const fn requires_refresh_worker(freshness_policy: SessionFreshnessPolicy) -> bool {
    matches!(freshness_policy, SessionFreshnessPolicy::RequireFresh)
}

pub(crate) struct DaemonSessionRetrievalService {
    database: Arc<RegisteredGlobalDb>,
    root: DaemonSessionRetrievalRoot,
    configuration: SessionRetrievalConfiguration,
    refresh_status: Option<Arc<dyn SessionProjectionServingStatusPort>>,
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
            refresh_status: refresh_status
                .map(|status| Arc::new(status) as Arc<dyn SessionProjectionServingStatusPort>),
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
            refresh_status: refresh_status
                .map(|status| Arc::new(status) as Arc<dyn SessionProjectionServingStatusPort>),
        })
    }

    fn refresh_not_current(&self) -> Option<SessionRetrievalUnavailable> {
        serving_status::not_current_unavailable(self.refresh_status.as_deref()?)
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

    async fn execute_temporal_query_with_context(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        query: SessionTemporalQuery,
        grant_id: &'static str,
    ) -> SessionRetrievalOutcome<TemporalKernelResult> {
        let authorizer = DaemonSessionRetrievalAuthorizer {
            actor: context.actor().clone(),
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
        .retrieve(context, binding, query)
        .await
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
        self.execute_temporal_query_with_context(&context, &binding, query, grant_id)
            .await
    }

    async fn execute_command(
        &self,
        command: SessionRetrievalCommand,
    ) -> SessionRetrievalServiceOutcome {
        if requires_refresh_worker(command.query().freshness_policy())
            && let Some(unavailable) = self.refresh_not_current()
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
                message: tracedecay_sessions::runtime::SessionMessageRecord {
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
}

mod lcm;

#[cfg(test)]
use lcm::{describe_retrieval_outcome, expand_retrieval_outcome};

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

mod sweep;

pub(crate) use sweep::DaemonSessionRetrievalSweep;

#[cfg(test)]
mod tests;

