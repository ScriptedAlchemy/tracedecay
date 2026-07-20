//! Daemon-backed session retrieval (message search) service: retrieval-root
//! resolution, scope authorization, request-context construction, LCM
//! describe/expand execution, and result filtering for the
//! `SessionRetrievalServicePort` implementation.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    ActorId, PayloadReferenceV1, ProjectId, RepositoryId, RetrievalGrainV1, SessionId,
    TemporalCoverageCountsV1, TemporalModeV1, WorktreeId,
};

use crate::application::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, MonotonicDeadline,
    PolicyDigest, ProfileId, RequestBudgets, RequestContext, RequestId, ResolvedGitRoute,
    ResolvedSessionIdentity, SessionRootId, SessionStoreId,
};
use crate::application::session::{
    AuthorizationGrantId, SessionAccess, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionRetrievalConfiguration, SessionRetrievalOutcome, SessionRetrievalScope,
    SessionRetrievalService, SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
};
use crate::daemon::session_temporal_refresh_scheduler::{
    SessionTemporalRefreshBlocker, SessionTemporalRefreshRetryClass,
    SessionTemporalRefreshUnavailableReason, SessionTemporalRefreshWake,
    SessionTemporalRefreshWorkerStatus,
};
use crate::global_db::{
    AuthorizedSessionDescribeRequest, AuthorizedSessionExpandCursorBinding,
    AuthorizedSessionExpandRequest, CompatibilityReadError, CompatibilityTemporalMetadata,
    GlobalDb, GlobalDbSessionTemporalExecution, ProjectRegistryContext,
};
use crate::mcp::tools::{
    LcmDescribeServiceCommand, LcmDescribeServiceFuture, LcmDescribeServiceOutcome,
    LcmExpandServiceCommand, LcmExpandServiceFuture, LcmExpandServiceOutcome,
    SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalPageView,
    SessionRetrievalServiceFuture, SessionRetrievalServiceOutcome, SessionRetrievalServicePort,
    SessionRetrievalStoreScope, SessionRetrievalUnavailable, SessionRetrievalUnavailableReason,
    SessionRetrievalWorkerBlocker, SessionRetrievalWorkerRetryClass,
    SessionRetrievalWorkerStatusView, SessionTemporalMetadataView, SessionTemporalWatermarksView,
};
use crate::query::temporal::TemporalKernelResult;
use crate::query::temporal::context::{TokenPolicy, VersionedTokenEstimator};
use crate::query::temporal::ports::TemporalExecutionSnapshot;
use crate::sessions::lcm::LcmExpandTarget;
use crate::sessions::{SessionMessageSearchResult, SessionMessageType, SessionSearchScope};
use crate::tracedecay::TraceDecay;

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
}

impl DaemonSessionRetrievalRoot {
    pub(crate) async fn project(cg: &TraceDecay, registry: &GlobalDb) -> Option<Self> {
        let project_id = cg.store_layout().identity.project_id.as_deref()?;
        let context = registry.project_registry_context_by_id(project_id).await?;
        Self::from_project_context(cg, registry, context)
    }

    fn from_project_context(
        cg: &TraceDecay,
        registry: &GlobalDb,
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

        let project_key = ProjectId::new(context.project.canonical_root.clone()).ok()?;
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
        })
    }

    #[cfg(feature = "test-transport")]
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
        })
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
    fn version(&self) -> &str {
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
        request: &SessionScopeAuthorizationRequest,
    ) -> std::result::Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        if context.actor_id().as_str() != MESSAGE_SEARCH_ACTOR_ID
            || request.actor_id() != context.actor_id()
            || context.identity() != &self.identity
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

pub(crate) struct DaemonSessionRetrievalService {
    database: Arc<GlobalDb>,
    root: DaemonSessionRetrievalRoot,
    configuration: SessionRetrievalConfiguration,
    calls: Arc<AtomicU64>,
    refresh_status: Option<SessionTemporalRefreshWake>,
}

impl DaemonSessionRetrievalService {
    pub(crate) fn new(
        database: Arc<GlobalDb>,
        root: DaemonSessionRetrievalRoot,
        calls: Arc<AtomicU64>,
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
            calls,
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
                SessionTemporalRefreshUnavailableReason::Stopped => {
                    SessionRetrievalUnavailableReason::RefreshWorkerStopped
                }
            },
            worker: Some(session_retrieval_worker_status(status)),
        })
    }

    fn request_context(&self, command: &SessionRetrievalCommand) -> Option<RequestContext> {
        let provider = command.query().provider();
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
        Some(RequestContext::new(
            ActorId::new(MESSAGE_SEARCH_ACTOR_ID).ok()?,
            RequestId::new(MESSAGE_SEARCH_ACTOR_ID).ok()?,
            self.root.identity.clone(),
            CapabilityDigest::new(capability),
            PolicyDigest::new(policy),
            ConfigurationDigest::new(configuration),
            MonotonicDeadline::at(Instant::now() + MESSAGE_SEARCH_TIMEOUT),
            CancellationToken::new(),
            RequestBudgets::new(
                MESSAGE_SEARCH_MAX_RESULTS,
                MESSAGE_SEARCH_MAX_BYTES,
                MESSAGE_SEARCH_MAX_WORK_UNITS,
            )
            .ok()?,
        ))
    }

    async fn execute_command(
        &self,
        command: SessionRetrievalCommand,
    ) -> SessionRetrievalServiceOutcome {
        if let Some(unavailable) = self.refresh_unavailable() {
            return SessionRetrievalServiceOutcome::Unavailable(unavailable);
        }
        // Count commands the service answers past the fast-path gate,
        // including wrong-scope rejections: the counter proves the transport
        // selected this service for the answer.
        self.calls.fetch_add(1, Ordering::Relaxed);
        if !self.root.owns(&command) {
            return SessionRetrievalServiceOutcome::WrongScope;
        }
        if command.goals()
            || !command.filters().git_filter.is_empty()
            || command.filters().workflow_scope.is_some()
        {
            return SessionRetrievalServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::without_worker(
                    SessionRetrievalUnavailableReason::UnsupportedQuery,
                ),
            );
        }
        let Some(context) = self.request_context(&command) else {
            return SessionRetrievalServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::without_worker(
                    SessionRetrievalUnavailableReason::RequestContextInvalid,
                ),
            );
        };
        let grant_id = match self.root.store_scope {
            SessionRetrievalStoreScope::Project => "grant.mcp.message-search.project",
            SessionRetrievalStoreScope::Profile => "grant.mcp.message-search.profile",
        };
        let service = SessionRetrievalService::new(
            DaemonSessionRetrievalAuthorizer {
                identity: self.root.identity.clone(),
                session_id: command.query().session_id().clone(),
                retrieval_scope: command.query().retrieval_scope().clone(),
                temporal_mode: command.query().temporal_mode(),
                grain: command.query().grain(),
                provider: command.query().provider().map(str::to_owned),
                grant_id,
            },
            GlobalDbSessionTemporalExecution::new(self.database.as_ref()),
            MessageSearchWordEstimator,
            self.configuration,
        );
        let outcome = service.retrieve(&context, command.query().clone()).await;
        self.public_outcome(outcome, &command).await
    }

    async fn public_outcome(
        &self,
        outcome: SessionRetrievalOutcome<TemporalKernelResult>,
        command: &SessionRetrievalCommand,
    ) -> SessionRetrievalServiceOutcome {
        match outcome {
            SessionRetrievalOutcome::Complete { items, freshness } => {
                match self.page(items, command).await {
                    Some(page) => SessionRetrievalServiceOutcome::Complete { page, freshness },
                    None => SessionRetrievalServiceOutcome::Unavailable(
                        SessionRetrievalUnavailable::without_worker(
                            SessionRetrievalUnavailableReason::HydrationUnavailable,
                        ),
                    ),
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
            } => match self.page(items, command).await {
                Some(page) => SessionRetrievalServiceOutcome::Partial {
                    page,
                    freshness,
                    omitted,
                },
                None => SessionRetrievalServiceOutcome::Unavailable(
                    SessionRetrievalUnavailable::without_worker(
                        SessionRetrievalUnavailableReason::HydrationUnavailable,
                    ),
                ),
            },
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

    async fn page(
        &self,
        items: Vec<TemporalKernelResult>,
        command: &SessionRetrievalCommand,
    ) -> Option<SessionRetrievalPageView> {
        let mut results = Vec::new();
        let mut anchors = Vec::new();
        let mut explanations = Vec::new();
        let mut coverage = TemporalCoverageCountsV1::default();
        let mut watermarks = SessionTemporalWatermarksView::default();
        let mut cursor = None;
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
            if item.next_cursor.is_some() {
                cursor = item.next_cursor.clone();
            }
            for ranked in item.ranked {
                let result = self.hydrate_result(&item.snapshot, &ranked).await?;
                anchors.push(ranked.anchor_id.clone());
                explanations.push(SessionRetrievalExplanationView {
                    anchor: ranked.anchor_id,
                    summary: format!(
                        "temporal rank {} at {}",
                        ranked.normalized_score_micros, ranked.knowledge_at_micros
                    ),
                });
                if message_search_result_matches(&result, command) {
                    results.push(result);
                }
            }
        }
        Some(SessionRetrievalPageView {
            results,
            temporal: SessionTemporalMetadataView {
                anchors,
                watermarks,
                coverage,
                cursor,
                explanations,
                authorized_root: self.root.authorized_root.clone(),
            },
        })
    }

    async fn hydrate_result(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        ranked: &crate::query::temporal::ranking::RankedCandidate,
    ) -> Option<SessionMessageSearchResult> {
        let message = GlobalDbSessionTemporalExecution::new(self.database.as_ref())
            .hydrate_authorized_occurrence(snapshot, &ranked.anchor_id)
            .await
            .ok()?;
        let session = self
            .database
            .get_session(&message.provider, &message.session_id)
            .await?;
        if message.session_id != session.session_id {
            return None;
        }
        Some(SessionMessageSearchResult {
            session,
            message,
            score: ranked.normalized_score_micros as f64 / 1_000_000.0,
        })
    }

    fn lcm_authorization_digest(&self, provider: &str) -> String {
        format!(
            "sha256:{}",
            hex::encode(message_search_digest(
                b"tracedecay.mcp.lcm.authorization.v1\0",
                &self.root.identity,
                Some(provider),
            ))
        )
    }

    fn lcm_temporal_view(
        &self,
        temporal: CompatibilityTemporalMetadata,
    ) -> SessionTemporalMetadataView {
        SessionTemporalMetadataView {
            anchors: temporal.anchors,
            watermarks: SessionTemporalWatermarksView {
                generation: temporal.watermarks.generation,
                source: temporal.watermarks.source,
                projection: temporal.watermarks.projection,
                index: temporal.watermarks.index,
                summary: temporal.watermarks.summary,
            },
            coverage: temporal.coverage,
            cursor: None,
            explanations: Vec::new(),
            authorized_root: self.root.authorized_root.clone(),
        }
    }

    async fn execute_lcm_describe(
        &self,
        command: LcmDescribeServiceCommand,
    ) -> LcmDescribeServiceOutcome {
        if command.store_scope() != self.root.store_scope {
            return LcmDescribeServiceOutcome::WrongScope;
        }
        if let Some(unavailable) = self.refresh_unavailable() {
            return LcmDescribeServiceOutcome::Unavailable(unavailable);
        }
        self.calls.fetch_add(1, Ordering::Relaxed);
        let request = match AuthorizedSessionDescribeRequest::new(
            command.provider(),
            command.session_id().as_str(),
            command.target().clone(),
            command.grain(),
            self.lcm_authorization_digest(command.provider()),
        ) {
            Ok(request) => request,
            Err(error) => return describe_compatibility_error(error),
        };
        let result = match GlobalDbSessionTemporalExecution::new(self.database.as_ref())
            .describe_compatible(request)
            .await
        {
            Ok(result) => result,
            Err(error) => return describe_compatibility_error(error),
        };
        LcmDescribeServiceOutcome::Complete {
            description: result.description,
            temporal: self.lcm_temporal_view(result.temporal),
            grain: result.grain,
            state: result.state,
            lineage: result.lineage,
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

    fn lcm_expand_cursor_binding(
        &self,
        command: &LcmExpandServiceCommand,
    ) -> Option<AuthorizedSessionExpandCursorBinding> {
        AuthorizedSessionExpandCursorBinding::new(
            command.provider(),
            command.session_id().as_str(),
            Self::lcm_expand_target_key(command.target()),
            command.grain(),
            command.content_slice().offset,
            command.content_slice().limit,
            command.source_limit(),
            self.lcm_authorization_digest(command.provider()),
        )
        .ok()
    }

    async fn encode_lcm_expand_cursor(
        &self,
        command: &LcmExpandServiceCommand,
        source_offset: usize,
    ) -> Option<String> {
        let binding = self.lcm_expand_cursor_binding(command)?;
        GlobalDbSessionTemporalExecution::new(self.database.as_ref())
            .encode_expand_cursor(binding, source_offset)
            .await
            .ok()
    }

    async fn decode_lcm_expand_cursor(&self, command: &LcmExpandServiceCommand) -> Option<usize> {
        let binding = self.lcm_expand_cursor_binding(command)?;
        GlobalDbSessionTemporalExecution::new(self.database.as_ref())
            .decode_expand_cursor(&binding, command.cursor()?)
            .await
            .ok()
    }

    async fn execute_lcm_expand(
        &self,
        command: LcmExpandServiceCommand,
    ) -> LcmExpandServiceOutcome {
        if command.store_scope() != self.root.store_scope {
            return LcmExpandServiceOutcome::WrongScope;
        }
        if let Some(unavailable) = self.refresh_unavailable() {
            return LcmExpandServiceOutcome::Unavailable(unavailable);
        }
        self.calls.fetch_add(1, Ordering::Relaxed);
        let source_offset = if command.cursor().is_some() {
            let Some(offset) = self.decode_lcm_expand_cursor(&command).await else {
                return LcmExpandServiceOutcome::Denied;
            };
            offset
        } else {
            command.source_offset()
        };
        let request = match AuthorizedSessionExpandRequest::new(
            command.provider(),
            command.session_id().as_str(),
            command.target().clone(),
            command.grain(),
            command.content_slice(),
            source_offset,
            command.source_limit(),
            self.lcm_authorization_digest(command.provider()),
        ) {
            Ok(request) => request,
            Err(error) => return expand_compatibility_error(error),
        };
        let result = match GlobalDbSessionTemporalExecution::new(self.database.as_ref())
            .expand_compatible(request)
            .await
        {
            Ok(result) => result,
            Err(error) => return expand_compatibility_error(error),
        };
        let mut temporal = self.lcm_temporal_view(result.temporal);
        if let Some(offset) = result
            .expansion
            .source_pagination
            .as_ref()
            .and_then(|pagination| pagination.next_source_offset)
        {
            let Some(cursor) = self.encode_lcm_expand_cursor(&command, offset).await else {
                return LcmExpandServiceOutcome::Unavailable(
                    SessionRetrievalUnavailable::without_worker(
                        SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
                    ),
                );
            };
            temporal.cursor = Some(cursor);
        }
        LcmExpandServiceOutcome::Complete {
            expansion: result.expansion,
            temporal,
            grain: result.grain,
            state: result.state,
        }
    }
}

impl SessionRetrievalServicePort for DaemonSessionRetrievalService {
    fn execute<'a>(
        &'a self,
        command: SessionRetrievalCommand,
    ) -> SessionRetrievalServiceFuture<'a> {
        Box::pin(async move { self.execute_command(command).await })
    }

    fn describe_lcm<'a>(
        &'a self,
        command: LcmDescribeServiceCommand,
    ) -> LcmDescribeServiceFuture<'a> {
        Box::pin(async move { self.execute_lcm_describe(command).await })
    }

    fn expand_lcm<'a>(&'a self, command: LcmExpandServiceCommand) -> LcmExpandServiceFuture<'a> {
        Box::pin(async move { self.execute_lcm_expand(command).await })
    }
}

fn describe_compatibility_error(error: CompatibilityReadError) -> LcmDescribeServiceOutcome {
    match error {
        CompatibilityReadError::Locked => LcmDescribeServiceOutcome::Locked,
        CompatibilityReadError::Redacted => LcmDescribeServiceOutcome::Redacted,
        CompatibilityReadError::Deleted => LcmDescribeServiceOutcome::Deleted,
        CompatibilityReadError::Denied => LcmDescribeServiceOutcome::Denied,
        CompatibilityReadError::Unavailable => {
            LcmDescribeServiceOutcome::Unavailable(SessionRetrievalUnavailable::without_worker(
                SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
            ))
        }
    }
}

fn expand_compatibility_error(error: CompatibilityReadError) -> LcmExpandServiceOutcome {
    match error {
        CompatibilityReadError::Locked => LcmExpandServiceOutcome::Locked,
        CompatibilityReadError::Redacted => LcmExpandServiceOutcome::Redacted,
        CompatibilityReadError::Deleted => LcmExpandServiceOutcome::Deleted,
        CompatibilityReadError::Denied => LcmExpandServiceOutcome::Denied,
        CompatibilityReadError::Unavailable => {
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

fn message_search_policy_digest() -> Option<[u8; 32]> {
    let encoded = PayloadReferenceV1::for_payload(&json!({
        "domain": "tracedecay.observation-anchor.authorization.v1",
        "authority": "observation-capture.v1",
    }))
    .ok()?;
    let digest = encoded.digest().as_str().strip_prefix("sha256:")?;
    hex::decode(digest).ok()?.try_into().ok()
}

fn message_search_result_matches(
    result: &SessionMessageSearchResult,
    command: &SessionRetrievalCommand,
) -> bool {
    let filters = command.filters();
    if filters
        .project_key
        .as_deref()
        .is_some_and(|project_key| result.session.project_key != project_key)
        || filters
            .parent_session_id
            .as_deref()
            .is_some_and(|parent| result.session.parent_session_id.as_deref() != Some(parent))
    {
        return false;
    }
    match filters.scope {
        SessionSearchScope::All => {}
        SessionSearchScope::ParentsOnly if result.session.is_subagent => return false,
        SessionSearchScope::SubagentsOnly if !result.session.is_subagent => return false,
        SessionSearchScope::ParentsOnly | SessionSearchScope::SubagentsOnly => {}
    }
    match filters.message_type {
        SessionMessageType::All => {}
        SessionMessageType::DirectUser
            if result.message.role != "user"
                || result.message.kind.as_deref() == Some("tool_result") =>
        {
            return false;
        }
        SessionMessageType::ToolResult
            if result.message.kind.as_deref() != Some("tool_result")
                && result.message.role != "tool" =>
        {
            return false;
        }
        SessionMessageType::DirectUser | SessionMessageType::ToolResult => {}
    }
    if !filters.roles.is_empty()
        && !filters
            .roles
            .iter()
            .any(|role| role == &result.message.role)
    {
        return false;
    }
    let timestamp = result.message.timestamp;
    filters
        .time_range
        .start_time
        .is_none_or(|start| timestamp.is_some_and(|value| value >= start))
        && filters
            .time_range
            .end_time
            .is_none_or(|end| timestamp.is_some_and(|value| value <= end))
}
