use std::future::Future;
use std::pin::Pin;

use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CompactContextLineageEdgeV1, CursorManifestLimitKindV1, HydrationStateV1, RetrievalAnchorId,
    RetrievalGrainV1, SessionId, SessionSourceCoverageV1, TemporalCoverageCountsV1,
};
use tracedecay_sessions::lcm::contracts::LcmRetrievalOutcome;
use tracedecay_temporal_query::ports::{
    TemporalCandidateFilterV1, TemporalMessageTypeFilterV1, TemporalSessionScopeFilterV1,
};

use crate::application::session::{SessionDataFreshness, SessionTemporalQuery};
use crate::global_db::WorkflowScopeFilter;
use crate::sessions::git_correlation::GitScopeFilter;
use crate::sessions::lcm::{
    LcmContentSlice, LcmDescribeResponse, LcmDescribeTarget, LcmExpandResponse, LcmExpandTarget,
};
use crate::sessions::{
    SessionMessageSearchResult, SessionMessageType, SessionSearchScope, SessionSearchTimeRange,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionRetrievalStoreScope {
    Project,
    Profile,
}

impl SessionRetrievalStoreScope {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Profile => "profile",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionRetrievalProjectSelector {
    pub(crate) project_id: Option<String>,
    pub(crate) project_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionRetrievalFilters {
    pub(crate) project_key: Option<String>,
    pub(crate) parent_session_id: Option<String>,
    pub(crate) source: Option<String>,
    pub(crate) include_summaries: bool,
    pub(crate) scope: SessionSearchScope,
    pub(crate) message_type: SessionMessageType,
    pub(crate) roles: Vec<String>,
    pub(crate) time_range: SessionSearchTimeRange,
    pub(crate) git_filter: GitScopeFilter,
    pub(crate) workflow_scope: Option<WorkflowScopeFilter>,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionRetrievalCommand {
    query: SessionTemporalQuery,
    #[cfg(test)]
    filters: SessionRetrievalFilters,
    #[cfg(test)]
    goals: bool,
    store_scope: SessionRetrievalStoreScope,
    project_selector: Option<SessionRetrievalProjectSelector>,
}

impl SessionRetrievalCommand {
    pub(crate) fn new(
        query: SessionTemporalQuery,
        filters: SessionRetrievalFilters,
        goals: bool,
        store_scope: SessionRetrievalStoreScope,
    ) -> Self {
        #[cfg(test)]
        let test_filters = filters.clone();
        let query = query
            .with_compatibility_filter_digest(compatibility_filter_digest(&filters, goals))
            .with_semantic_filter(temporal_candidate_filter(&filters, goals));
        Self {
            query,
            #[cfg(test)]
            filters: test_filters,
            #[cfg(test)]
            goals,
            store_scope,
            project_selector: None,
        }
    }

    pub(crate) fn query(&self) -> &SessionTemporalQuery {
        &self.query
    }

    #[cfg(test)]
    pub(crate) fn filters(&self) -> &SessionRetrievalFilters {
        &self.filters
    }

    #[cfg(test)]
    pub(crate) const fn goals(&self) -> bool {
        self.goals
    }

    pub(crate) const fn store_scope(&self) -> SessionRetrievalStoreScope {
        self.store_scope
    }

    pub(crate) fn project_selector(&self) -> Option<&SessionRetrievalProjectSelector> {
        self.project_selector.as_ref()
    }

    pub(crate) fn with_project_selector(
        mut self,
        project_selector: Option<SessionRetrievalProjectSelector>,
    ) -> Self {
        self.project_selector = project_selector;
        self
    }
}

fn temporal_candidate_filter(
    filters: &SessionRetrievalFilters,
    goals: bool,
) -> TemporalCandidateFilterV1 {
    let mut roles = filters.roles.clone();
    roles.sort();
    roles.dedup();
    TemporalCandidateFilterV1 {
        project_key: filters.project_key.clone(),
        parent_session_id: filters.parent_session_id.clone(),
        source: filters.source.clone(),
        include_summaries: filters.include_summaries,
        session_scope: match filters.scope {
            SessionSearchScope::All => TemporalSessionScopeFilterV1::All,
            SessionSearchScope::ParentsOnly => TemporalSessionScopeFilterV1::ParentsOnly,
            SessionSearchScope::SubagentsOnly => TemporalSessionScopeFilterV1::SubagentsOnly,
        },
        message_type: match filters.message_type {
            SessionMessageType::All => TemporalMessageTypeFilterV1::All,
            SessionMessageType::DirectUser => TemporalMessageTypeFilterV1::DirectUser,
            SessionMessageType::ToolResult => TemporalMessageTypeFilterV1::ToolResult,
        },
        roles,
        start_time: filters.time_range.start_time,
        end_time: filters.time_range.end_time,
        git_branch: filters.git_filter.branch.clone(),
        git_worktree: filters.git_filter.worktree.clone(),
        git_commit: filters.git_filter.commit.clone(),
        workflow_run: filters
            .workflow_scope
            .as_ref()
            .map(|scope| scope.run_id.clone()),
        workflow_agent: filters
            .workflow_scope
            .as_ref()
            .and_then(|scope| scope.agent_label.clone()),
        goals,
    }
}

fn compatibility_filter_digest(filters: &SessionRetrievalFilters, goals: bool) -> String {
    let mut roles = filters.roles.clone();
    roles.sort();
    roles.dedup();
    let encoded = json!({
        "version": 2,
        "project_key": filters.project_key,
        "parent_session_id": filters.parent_session_id,
        "source": filters.source,
        "include_summaries": filters.include_summaries,
        "scope": filters.scope.as_str(),
        "message_type": filters.message_type.as_str(),
        "roles": roles,
        "start_time": filters.time_range.start_time,
        "end_time": filters.time_range.end_time,
        "git": filters.git_filter,
        "workflow": filters.workflow_scope,
        "goals": goals,
    })
    .to_string();
    format!("sha256:{}", hex::encode(Sha256::digest(encoded.as_bytes())))
}

pub(crate) type SessionRetrievalServiceFuture<'a> =
    Pin<Box<dyn Future<Output = SessionRetrievalServiceOutcome> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LcmDescribeServiceCommand {
    provider: String,
    session_id: SessionId,
    target: LcmDescribeTarget,
    grain: RetrievalGrainV1,
    store_scope: SessionRetrievalStoreScope,
}

impl LcmDescribeServiceCommand {
    pub(crate) fn new(
        provider: impl Into<String>,
        session_id: SessionId,
        target: LcmDescribeTarget,
        grain: RetrievalGrainV1,
        store_scope: SessionRetrievalStoreScope,
    ) -> Self {
        Self {
            provider: provider.into(),
            session_id,
            target,
            grain,
            store_scope,
        }
    }

    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(crate) fn target(&self) -> &LcmDescribeTarget {
        &self.target
    }

    pub(crate) const fn grain(&self) -> RetrievalGrainV1 {
        self.grain
    }

    pub(crate) const fn store_scope(&self) -> SessionRetrievalStoreScope {
        self.store_scope
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LcmExpandServiceCommand {
    provider: String,
    session_id: SessionId,
    target: LcmExpandTarget,
    grain: RetrievalGrainV1,
    content_slice: LcmContentSlice,
    source_offset: usize,
    source_limit: Option<usize>,
    cursor: Option<String>,
    store_scope: SessionRetrievalStoreScope,
}

impl LcmExpandServiceCommand {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        provider: impl Into<String>,
        session_id: SessionId,
        target: LcmExpandTarget,
        grain: RetrievalGrainV1,
        content_slice: LcmContentSlice,
        source_offset: usize,
        source_limit: Option<usize>,
        cursor: Option<String>,
        store_scope: SessionRetrievalStoreScope,
    ) -> Self {
        Self {
            provider: provider.into(),
            session_id,
            target,
            grain,
            content_slice,
            source_offset,
            source_limit,
            cursor,
            store_scope,
        }
    }

    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(crate) fn target(&self) -> &LcmExpandTarget {
        &self.target
    }

    pub(crate) const fn grain(&self) -> RetrievalGrainV1 {
        self.grain
    }

    pub(crate) const fn content_slice(&self) -> LcmContentSlice {
        self.content_slice
    }

    pub(crate) const fn source_offset(&self) -> usize {
        self.source_offset
    }

    pub(crate) const fn source_limit(&self) -> Option<usize> {
        self.source_limit
    }

    pub(crate) fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    pub(crate) const fn store_scope(&self) -> SessionRetrievalStoreScope {
        self.store_scope
    }
}

pub(crate) type LcmDescribeServiceFuture<'a> =
    Pin<Box<dyn Future<Output = LcmDescribeServiceOutcome> + Send + 'a>>;
pub(crate) type LcmExpandServiceFuture<'a> =
    Pin<Box<dyn Future<Output = LcmExpandServiceOutcome> + Send + 'a>>;

pub(crate) trait SessionRetrievalServicePort: Send + Sync {
    #[allow(clippy::elidable_lifetime_names)]
    fn execute<'a>(&'a self, command: SessionRetrievalCommand)
    -> SessionRetrievalServiceFuture<'a>;

    #[allow(clippy::elidable_lifetime_names)]
    fn describe_lcm<'a>(
        &'a self,
        _command: LcmDescribeServiceCommand,
    ) -> LcmDescribeServiceFuture<'a> {
        Box::pin(async {
            LcmDescribeServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::service_not_configured(),
            )
        })
    }

    #[allow(clippy::elidable_lifetime_names)]
    fn expand_lcm<'a>(&'a self, _command: LcmExpandServiceCommand) -> LcmExpandServiceFuture<'a> {
        Box::pin(async {
            LcmExpandServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::service_not_configured(),
            )
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionRetrievalExplanationView {
    pub(crate) anchor: RetrievalAnchorId,
    pub(crate) summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionRetrievalOmissionView {
    pub(crate) rank: u32,
    pub(crate) anchor: RetrievalAnchorId,
    pub(crate) reason: HydrationStateV1,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct SessionTemporalWatermarksView {
    pub(crate) generation: u64,
    pub(crate) source: u64,
    pub(crate) projection: u64,
    pub(crate) index: u64,
    pub(crate) summary: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct SessionTemporalMetadataView {
    pub(crate) anchors: Vec<RetrievalAnchorId>,
    pub(crate) watermarks: SessionTemporalWatermarksView,
    pub(crate) coverage: TemporalCoverageCountsV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) source_coverage: Vec<SessionSourceCoverageV1>,
    pub(crate) cursor: Option<String>,
    pub(crate) explanations: Vec<SessionRetrievalExplanationView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) omissions: Vec<SessionRetrievalOmissionView>,
    pub(crate) authorized_root: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionRetrievalUnavailableReason {
    ServiceNotConfigured,
    RefreshWorkerMissing,
    RefreshWorkerRecovering,
    RefreshWorkerStalled,
    RefreshWorkerStopped,
    TemporalStoreUnavailable,
    HydrationUnavailable,
}

impl SessionRetrievalUnavailableReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ServiceNotConfigured => "service_not_configured",
            Self::RefreshWorkerMissing => "refresh_worker_missing",
            Self::RefreshWorkerRecovering => "refresh_worker_recovering",
            Self::RefreshWorkerStalled => "refresh_worker_stalled",
            Self::RefreshWorkerStopped => "refresh_worker_stopped",
            Self::TemporalStoreUnavailable => "temporal_store_unavailable",
            Self::HydrationUnavailable => "hydration_unavailable",
        }
    }

    pub(crate) const fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::RefreshWorkerMissing
                | Self::RefreshWorkerRecovering
                | Self::RefreshWorkerStalled
                | Self::RefreshWorkerStopped
                | Self::TemporalStoreUnavailable
                | Self::HydrationUnavailable
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionRetrievalWorkerBlocker {
    WorkerMissing,
    WorkerPanicked,
    WorkerStopped,
    Storage,
    Projector,
    Deadline,
}

impl SessionRetrievalWorkerBlocker {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::WorkerMissing => "worker_missing",
            Self::WorkerPanicked => "worker_panicked",
            Self::WorkerStopped => "worker_stopped",
            Self::Storage => "storage",
            Self::Projector => "projector",
            Self::Deadline => "deadline",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionRetrievalWorkerRetryClass {
    Storage,
    Projector,
    Deadline,
}

impl SessionRetrievalWorkerRetryClass {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Projector => "projector",
            Self::Deadline => "deadline",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SessionRetrievalWorkerStatusView {
    pub(crate) last_progress_at_unix_micros: Option<i64>,
    pub(crate) backlog: usize,
    pub(crate) blocker: Option<SessionRetrievalWorkerBlocker>,
    pub(crate) retry_class: Option<SessionRetrievalWorkerRetryClass>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SessionRetrievalUnavailable {
    pub(crate) reason: SessionRetrievalUnavailableReason,
    pub(crate) worker: Option<SessionRetrievalWorkerStatusView>,
}

impl SessionRetrievalUnavailable {
    pub(crate) const fn service_not_configured() -> Self {
        Self {
            reason: SessionRetrievalUnavailableReason::ServiceNotConfigured,
            worker: None,
        }
    }

    pub(crate) const fn without_worker(reason: SessionRetrievalUnavailableReason) -> Self {
        Self {
            reason,
            worker: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum LcmDescribeServiceOutcome {
    Complete {
        description: LcmDescribeResponse,
        temporal: SessionTemporalMetadataView,
        grain: RetrievalGrainV1,
        state: HydrationStateV1,
        lineage: Vec<CompactContextLineageEdgeV1>,
        retrieval: LcmRetrievalOutcome,
    },
    Partial {
        description: Option<LcmDescribeResponse>,
        temporal: SessionTemporalMetadataView,
        grain: RetrievalGrainV1,
        state: Option<HydrationStateV1>,
        lineage: Vec<CompactContextLineageEdgeV1>,
        retrieval: LcmRetrievalOutcome,
    },
    Stale {
        temporal: SessionTemporalMetadataView,
        retrieval: LcmRetrievalOutcome,
    },
    WrongScope,
    Locked,
    Redacted,
    Deleted,
    Denied,
    Unavailable(SessionRetrievalUnavailable),
    BudgetExhausted,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum LcmExpandServiceOutcome {
    Complete {
        expansion: LcmExpandResponse,
        temporal: SessionTemporalMetadataView,
        grain: RetrievalGrainV1,
        state: HydrationStateV1,
        retrieval: LcmRetrievalOutcome,
    },
    Partial {
        expansion: Option<LcmExpandResponse>,
        temporal: SessionTemporalMetadataView,
        grain: RetrievalGrainV1,
        state: Option<HydrationStateV1>,
        retrieval: LcmRetrievalOutcome,
    },
    Stale {
        temporal: SessionTemporalMetadataView,
        retrieval: LcmRetrievalOutcome,
    },
    WrongScope,
    Locked,
    Redacted,
    Deleted,
    Denied,
    Unavailable(SessionRetrievalUnavailable),
    BudgetExhausted,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct SessionRetrievalPageView {
    pub(crate) results: Vec<SessionMessageSearchResult>,
    pub(crate) temporal: SessionTemporalMetadataView,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SessionRetrievalServiceOutcome {
    Complete {
        page: SessionRetrievalPageView,
        freshness: SessionDataFreshness,
    },
    CompleteZero {
        temporal: SessionTemporalMetadataView,
        freshness: SessionDataFreshness,
    },
    Stale {
        temporal: SessionTemporalMetadataView,
        freshness: SessionDataFreshness,
    },
    Partial {
        page: SessionRetrievalPageView,
        freshness: SessionDataFreshness,
        omitted: u64,
    },
    WrongScope,
    Locked,
    Redacted,
    Deleted,
    Denied,
    Unavailable(SessionRetrievalUnavailable),
    CursorManifestLimitExceeded {
        kind: CursorManifestLimitKindV1,
        observed: usize,
        maximum: usize,
    },
    BudgetExhausted,
    Cancelled,
}
