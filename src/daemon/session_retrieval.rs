//! Daemon-owned session retrieval service: retrieval-root
//! resolution, scope authorization, request-context construction, LCM
//! describe/expand execution, and result filtering for application-admitted
//! retrieval.

use std::sync::Arc;

use serde_json::json;
use sha2::{Digest, Sha256};
use tracedecay_application::RequestContext;
use tracedecay_domain::{
    ActorId, HydrationStateV1, ProjectId, RetrievalGrainV1, SessionId, TemporalCoverageCountsV1,
    TemporalModeV1,
};
#[cfg(test)]
use tracedecay_domain::{RepositoryId, WorktreeId};
use tracedecay_store::StoreShardIdV1;
use tracedecay_usecases::context::{
    BranchId, ProfileId, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
};
use tracedecay_usecases::session::{
    AuthorizationGrantId, SessionAccess, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionDataFreshness, SessionFreshnessPolicy, SessionProjectionServingStatusPort,
    SessionRequestBinding, SessionRetrievalConfiguration, SessionRetrievalOutcome,
    SessionRetrievalScope, SessionRetrievalService, SessionScopeAuthorizationRequest,
    SessionScopeAuthorizer, SessionTemporalExecutionError, SessionTemporalQuery,
};

use crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake;
use crate::global_db::session_temporal::{
    RegisteredGlobalDbSessionTemporalExecution, SessionPageReconstruction,
    SessionPageReconstructionRequest,
};
use crate::global_db::{ProjectRegistryContext, RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use crate::tracedecay::TraceDecay;
use tracedecay_sessions::runtime::SessionMessageSearchResult;
use tracedecay_temporal_query::context::{TokenPolicy, VersionedTokenEstimator};
use tracedecay_temporal_query::ports::{ExecutionLimits, TemporalExecutionSnapshot};
use tracedecay_temporal_query::ranking::RankedCandidate;
use tracedecay_temporal_query::{TemporalHydratedResult, TemporalKernelResult};

const MESSAGE_SEARCH_PROFILE_ID: &str = "profile.primary";
const MESSAGE_SEARCH_SCHEMA_VERSION: u32 = 1;
const MESSAGE_SEARCH_RANKING_VERSION: u32 = 1;

mod serving_status;
const MESSAGE_SEARCH_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Byte ceiling every admitted application retrieval request is bound by.
///
/// `SessionRetrievalService::retrieve` refuses — terminally, as
/// `BudgetExhausted` — any query whose context budget or execution limits
/// exceed the binding's budgets, so every query built for the admitted path
/// must be sized against this constant rather than the multi-MiB
/// `ExecutionLimits::default()` or [`MESSAGE_SEARCH_MAX_BYTES`].
pub(crate) const APPLICATION_RETRIEVAL_MAX_BYTES: u64 = 64 * 1024;

/// Execution limits an admitted application retrieval of `limit` items may ask
/// for.
///
/// The admitted binding checks exactly four things: the three *total* byte
/// limits against [`APPLICATION_RETRIEVAL_MAX_BYTES`], and that the hydration
/// item count covers the requested page. The defaults are multi-MiB and are
/// rejected outright, which reads to the caller as a terminal `Saturated`
/// rather than a smaller answer, so those four are the ones this sizes.
///
/// Nothing else is narrowed. Candidate and record item counts are the pool the
/// ranker draws from, not the page it returns: clamping them to `limit` would
/// leave a ten-result search ranking ten candidates instead of the default
/// 256, silently trading recall for a budget the binding never asked about.
pub(crate) fn admitted_execution_limits(limit: usize) -> ExecutionLimits {
    let bytes = usize::try_from(APPLICATION_RETRIEVAL_MAX_BYTES).unwrap_or(usize::MAX);
    let defaults = ExecutionLimits::default();
    ExecutionLimits {
        candidate_total_bytes: bytes.min(defaults.candidate_total_bytes),
        record_total_bytes: bytes.min(defaults.record_total_bytes),
        hydration_total_bytes: bytes.min(defaults.hydration_total_bytes),
        hydration_limit: defaults.hydration_limit.max(limit),
        ..defaults
    }
}

mod admitted;
mod contract;
mod primitive;
pub(crate) use admitted::{
    SessionApplicationRetrievalFutureV1, SessionApplicationRetrievalPortV1,
    UnavailableSessionApplicationRetrievalV1,
};
pub(crate) use contract::{
    LcmDescribeServiceCommand, LcmDescribeServiceFuture, LcmDescribeServiceOutcome,
    LcmExpandServiceCommand, LcmExpandServiceFuture, LcmExpandServiceOutcome,
    SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalFilters,
    SessionRetrievalOmissionView, SessionRetrievalPageView, SessionRetrievalServiceOutcome,
    SessionRetrievalStoreScope, SessionRetrievalUnavailable, SessionRetrievalUnavailableReason,
    SessionTemporalMetadataView, SessionTemporalWatermarksView,
};
pub(crate) use primitive::DaemonSessionLookupPrimitiveV1;

#[derive(Clone)]
pub(crate) struct DaemonSessionRetrievalRoot {
    store_scope: SessionRetrievalStoreScope,
    identity: ResolvedSessionIdentity,
    project_id: Option<String>,
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
        Some(Self {
            store_scope: SessionRetrievalStoreScope::Project,
            identity,
            project_id: Some(context.project.project_id),
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
                crate::daemon::code_index_scheduler::identity::repository_id_for(&project_root)
                    .unwrap_or_else(|error| panic!("test repository identity: {error}")),
                crate::daemon::code_index_scheduler::identity::worktree_id_for(&project_root)
                    .unwrap_or_else(|error| panic!("test worktree identity: {error}")),
                BranchId::new(
                    crate::branch::current_branch(&project_root)
                        .unwrap_or_else(|| "detached".to_owned()),
                )
                .unwrap_or_else(|error| panic!("test branch identity: {error}")),
            ),
        );
        Self {
            store_scope: SessionRetrievalStoreScope::Project,
            identity,
            project_id,
            authorized_root: Some(project_key_value),
            expected_runtime_shard: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn project_identity_for_test(
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        authorized_root: String,
    ) -> Self {
        let identity = ResolvedSessionIdentity::for_project(
            ProfileId::new(MESSAGE_SEARCH_PROFILE_ID)
                .unwrap_or_else(|error| panic!("test profile identity: {error}")),
            project_id.clone(),
            SessionStoreId::new("store.project.test")
                .unwrap_or_else(|error| panic!("test store identity: {error}")),
            SessionRootId::new("root.project.test")
                .unwrap_or_else(|error| panic!("test root identity: {error}")),
            ResolvedGitRoute::new(
                repository_id,
                worktree_id,
                BranchId::new("branch.project.test")
                    .unwrap_or_else(|error| panic!("test branch identity: {error}")),
            ),
        );
        Self {
            store_scope: SessionRetrievalStoreScope::Project,
            identity,
            project_id: Some(project_id.as_str().to_owned()),
            authorized_root: Some(authorized_root),
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
    database: RegisteredGlobalDbLeaseV1,
    root: DaemonSessionRetrievalRoot,
    configuration: SessionRetrievalConfiguration,
    refresh_status: Option<Arc<dyn SessionProjectionServingStatusPort>>,
}

impl DaemonSessionRetrievalService {
    pub(crate) fn new(
        database: RegisteredGlobalDbLeaseV1,
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
        database: RegisteredGlobalDbLeaseV1,
        registered_database: RegisteredGlobalDbLeaseV1,
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

    async fn public_outcome(
        &self,
        outcome: SessionRetrievalOutcome<TemporalKernelResult>,
    ) -> SessionRetrievalServiceOutcome {
        match outcome {
            SessionRetrievalOutcome::Complete { items, freshness } => {
                match self.page(items).await {
                    Ok((page, skipped, _)) => complete_page_outcome(page, freshness, skipped),
                    Err(error) => self.rendering_error(error),
                }
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
            } => match self.page(items).await {
                Ok((page, _, rendering_omitted)) => SessionRetrievalServiceOutcome::Partial {
                    page,
                    freshness,
                    omitted: omitted.saturating_add(rendering_omitted),
                },
                Err(error) => self.rendering_error(error),
            },
            SessionRetrievalOutcome::WrongScope => SessionRetrievalServiceOutcome::WrongScope,
            SessionRetrievalOutcome::Locked => SessionRetrievalServiceOutcome::Locked,
            SessionRetrievalOutcome::Redacted => SessionRetrievalServiceOutcome::Redacted,
            SessionRetrievalOutcome::Deleted => SessionRetrievalServiceOutcome::Deleted,
            SessionRetrievalOutcome::Denied => SessionRetrievalServiceOutcome::Denied,
            SessionRetrievalOutcome::ResetRequired => {
                SessionRetrievalServiceOutcome::ResetRequired {
                    store_scope: self.root.store_scope,
                }
            }
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

    fn rendering_error(
        &self,
        error: SessionTemporalExecutionError,
    ) -> SessionRetrievalServiceOutcome {
        match error {
            SessionTemporalExecutionError::WrongScope => SessionRetrievalServiceOutcome::WrongScope,
            SessionTemporalExecutionError::Locked => SessionRetrievalServiceOutcome::Locked,
            SessionTemporalExecutionError::Redacted => SessionRetrievalServiceOutcome::Redacted,
            SessionTemporalExecutionError::Deleted => SessionRetrievalServiceOutcome::Deleted,
            SessionTemporalExecutionError::Denied => SessionRetrievalServiceOutcome::Denied,
            SessionTemporalExecutionError::ResetRequired => {
                SessionRetrievalServiceOutcome::ResetRequired {
                    store_scope: self.root.store_scope,
                }
            }
            SessionTemporalExecutionError::BudgetExhausted => {
                SessionRetrievalServiceOutcome::BudgetExhausted
            }
            SessionTemporalExecutionError::Cancelled => SessionRetrievalServiceOutcome::Cancelled,
            SessionTemporalExecutionError::Stale { generation_lag } => {
                SessionRetrievalServiceOutcome::Stale {
                    temporal: self.empty_temporal(),
                    freshness: SessionDataFreshness::Stored { generation_lag },
                }
            }
            SessionTemporalExecutionError::Empty { freshness } => {
                SessionRetrievalServiceOutcome::CompleteZero {
                    temporal: self.empty_temporal(),
                    freshness,
                }
            }
            SessionTemporalExecutionError::Unavailable
            | SessionTemporalExecutionError::Kernel(_) => {
                SessionRetrievalServiceOutcome::Unavailable(
                    SessionRetrievalUnavailable::without_worker(
                        SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
                    ),
                )
            }
        }
    }

    async fn page(
        &self,
        items: Vec<TemporalKernelResult>,
    ) -> Result<(SessionRetrievalPageView, u64, u64), SessionTemporalExecutionError> {
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
        let mut batch = SessionPageReconstructionInputs::default();
        let mut queued_reconstruction = Vec::with_capacity(items.len());
        for item in &items {
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
            let mut queued = Vec::with_capacity(item.ranked.len());
            for (rank, ranked) in item.ranked.iter().enumerate() {
                let queued_for_batch = page_hydration_slot(rank, ranked, &item.hydrated)
                    .ok()
                    .is_some_and(|hydrated| batch.push(&item.snapshot, ranked, hydrated));
                queued.push(queued_for_batch);
            }
            queued_reconstruction.push(queued);
        }
        let reconstructed = if batch.is_empty() {
            Vec::new()
        } else {
            let execution = self.registered_execution()?;
            execution
                .reconstruct_session_page(batch.into_requests())
                .await?
        };
        let mut reconstructed = reconstructed.into_iter();
        for (item, queued) in items.iter().zip(queued_reconstruction) {
            for (rank, ranked) in item.ranked.iter().enumerate() {
                let hydrated = match page_hydration_slot(rank, ranked, &item.hydrated) {
                    Ok(hydrated) => hydrated,
                    Err(omission) => {
                        skipped = skipped.saturating_add(1);
                        omissions.push(omission);
                        continue;
                    }
                };
                let reconstruction = if queued.get(rank).copied().unwrap_or(false) {
                    Some(
                        reconstructed
                            .next()
                            .ok_or(SessionTemporalExecutionError::Unavailable)?,
                    )
                } else {
                    None
                };
                let rendering = if ranked.evidence_role.as_deref() == Some("summary") {
                    self.hydrate_summary_result(ranked, hydrated, reconstruction)?
                } else if let Some(reconstruction) = reconstruction {
                    self.hydrate_non_summary_result(ranked, reconstruction)?
                } else {
                    PageRenderingResult::Omitted(HydrationStateV1::RetainedButUnavailable)
                };
                let result = match rendering {
                    PageRenderingResult::Rendered(result) => *result,
                    PageRenderingResult::Omitted(reason) => {
                        skipped = skipped.saturating_add(1);
                        rendering_omitted = rendering_omitted.saturating_add(1);
                        coverage.unknown = coverage.unknown.saturating_add(1);
                        omissions.push(SessionRetrievalOmissionView {
                            rank: hydrated.rank(),
                            anchor: ranked.anchor_id.clone(),
                            reason,
                        });
                        continue;
                    }
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
        if reconstructed.next().is_some() {
            return Err(SessionTemporalExecutionError::Unavailable);
        }
        source_coverage.sort_by(|left, right| left.source_id().cmp(right.source_id()));
        source_coverage.dedup_by(|left, right| left.source_id() == right.source_id());
        Ok((
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
        ))
    }

    fn hydrate_summary_result(
        &self,
        ranked: &tracedecay_temporal_query::ranking::RankedCandidate,
        hydrated: &TemporalHydratedResult,
        reconstruction: Option<Result<SessionPageReconstruction, SessionTemporalExecutionError>>,
    ) -> Result<PageRenderingResult, SessionTemporalExecutionError> {
        let Some(content) = hydrated.content() else {
            return Ok(PageRenderingResult::Omitted(
                HydrationStateV1::RetainedButUnavailable,
            ));
        };
        let Some(reconstruction) = reconstruction else {
            return Ok(PageRenderingResult::Omitted(
                HydrationStateV1::RetainedButUnavailable,
            ));
        };
        let reconstruction = match reconstruction_or_omission(reconstruction)? {
            Ok(reconstruction) => reconstruction,
            Err(reason) => return Ok(PageRenderingResult::Omitted(reason)),
        };
        let SessionPageReconstruction::Summary { session } = reconstruction else {
            return Err(SessionTemporalExecutionError::Unavailable);
        };
        let Some(summary_id) = ranked
            .contributions
            .iter()
            .find(|contribution| {
                contribution.channel
                    == tracedecay_temporal_query::candidates::CandidateChannel::Summary
            })
            .map(|contribution| contribution.retriever_record_id.clone())
        else {
            return Ok(PageRenderingResult::Omitted(
                HydrationStateV1::RetainedButUnavailable,
            ));
        };
        let Ok(text) = std::str::from_utf8(content).map(str::to_owned) else {
            return Ok(PageRenderingResult::Omitted(
                HydrationStateV1::RetainedButUnavailable,
            ));
        };
        let Some(provider) = ranked.source.clone() else {
            return Ok(PageRenderingResult::Omitted(
                HydrationStateV1::RetainedButUnavailable,
            ));
        };
        let Some(session_id) = ranked.session.clone() else {
            return Ok(PageRenderingResult::Omitted(
                HydrationStateV1::RetainedButUnavailable,
            ));
        };
        Ok(PageRenderingResult::Rendered(Box::new(
            SessionMessageSearchResult {
                session,
                message: tracedecay_sessions::runtime::SessionMessageRecord {
                    provider,
                    message_id: summary_id,
                    session_id,
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
            },
        )))
    }

    fn hydrate_non_summary_result(
        &self,
        ranked: &tracedecay_temporal_query::ranking::RankedCandidate,
        reconstruction: Result<SessionPageReconstruction, SessionTemporalExecutionError>,
    ) -> Result<PageRenderingResult, SessionTemporalExecutionError> {
        let reconstruction = match reconstruction_or_omission(reconstruction)? {
            Ok(reconstruction) => reconstruction,
            Err(reason) => return Ok(PageRenderingResult::Omitted(reason)),
        };
        let SessionPageReconstruction::Occurrence { session, message } = reconstruction else {
            return Err(SessionTemporalExecutionError::Unavailable);
        };
        Ok(PageRenderingResult::Rendered(Box::new(
            SessionMessageSearchResult {
                session,
                message: *message,
                score: ranked.normalized_score_micros as f64 / 1_000_000.0,
            },
        )))
    }
}

enum PageRenderingResult {
    Rendered(Box<SessionMessageSearchResult>),
    Omitted(HydrationStateV1),
}

#[derive(Default)]
struct SessionPageReconstructionInputs<'a> {
    requests: Vec<SessionPageReconstructionRequest<'a>>,
}

impl<'a> SessionPageReconstructionInputs<'a> {
    fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    fn push(
        &mut self,
        snapshot: &'a TemporalExecutionSnapshot,
        ranked: &'a RankedCandidate,
        hydrated: &'a TemporalHydratedResult,
    ) -> bool {
        let Some(content) = hydrated.content() else {
            return false;
        };
        let Some(provider) = ranked.source.as_deref() else {
            return false;
        };
        let Some(session_id) = ranked.session.as_deref() else {
            return false;
        };
        let request = if ranked.evidence_role.as_deref() == Some("summary") {
            SessionPageReconstructionRequest::summary(snapshot, provider, session_id)
        } else {
            SessionPageReconstructionRequest::occurrence(
                snapshot,
                &ranked.anchor_id,
                provider,
                session_id,
                content,
            )
        };
        self.requests.push(request);
        true
    }

    fn into_requests(self) -> Vec<SessionPageReconstructionRequest<'a>> {
        self.requests
    }
}

fn reconstruction_or_omission(
    reconstruction: Result<SessionPageReconstruction, SessionTemporalExecutionError>,
) -> Result<Result<SessionPageReconstruction, HydrationStateV1>, SessionTemporalExecutionError> {
    match reconstruction {
        Ok(reconstruction) => Ok(Ok(reconstruction)),
        Err(SessionTemporalExecutionError::Unavailable) => {
            Ok(Err(HydrationStateV1::RetainedButUnavailable))
        }
        Err(SessionTemporalExecutionError::Locked) => Ok(Err(HydrationStateV1::Locked)),
        Err(SessionTemporalExecutionError::Redacted) => Ok(Err(HydrationStateV1::Redacted)),
        Err(SessionTemporalExecutionError::Deleted) => Ok(Err(HydrationStateV1::Deleted)),
        Err(SessionTemporalExecutionError::Denied) => Ok(Err(HydrationStateV1::Unauthorized)),
        Err(error) => Err(error),
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

#[cfg(test)]
mod tests;
