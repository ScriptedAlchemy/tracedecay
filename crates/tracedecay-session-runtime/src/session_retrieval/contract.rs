//! Transport-neutral daemon session-retrieval commands and outcomes.

use std::future::Future;
use std::pin::Pin;

use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;
use tracedecay_domain::{
    CompactContextLineageEdgeV1, CursorManifestLimitKindV1, HydrationStateV1, RetrievalAnchorId,
    RetrievalGrainV1, SessionId, SessionSourceCoverageV1, TemporalCoverageCountsV1,
};
use tracedecay_lcm::contracts::LcmRetrievalOutcome;
use tracedecay_temporal_query::ports::{
    TemporalCandidateFilterV1, TemporalMessageTypeFilterV1, TemporalSessionScopeFilterV1,
};

use tracedecay_global_db::WorkflowScopeFilter;
use tracedecay_lcm::{
    LcmContentSlice, LcmDescribeResponse, LcmDescribeTarget, LcmExpandResponse, LcmExpandTarget,
};
use tracedecay_session_memory::session::{SessionDataFreshness, SessionTemporalQuery};
use tracedecay_sessions::runtime::git_correlation::GitScopeFilter;
use tracedecay_sessions::runtime::{
    SessionMessageSearchResult, SessionMessageType, SessionSearchScope, SessionSearchTimeRange,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRetrievalStoreScope {
    Project,
    Profile,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRetrievalFilters {
    pub project_key: Option<String>,
    pub parent_session_id: Option<String>,
    pub source: Option<String>,
    pub include_summaries: bool,
    pub scope: SessionSearchScope,
    pub message_type: SessionMessageType,
    pub roles: Vec<String>,
    pub time_range: SessionSearchTimeRange,
    pub git_filter: GitScopeFilter,
    pub workflow_scope: Option<WorkflowScopeFilter>,
}

#[derive(Clone, Debug)]
pub struct SessionRetrievalCommand {
    query: SessionTemporalQuery,
}

impl SessionRetrievalCommand {
    pub fn new(query: SessionTemporalQuery, filters: SessionRetrievalFilters, goals: bool) -> Self {
        let query = query
            .with_compatibility_filter_digest(compatibility_filter_digest(&filters, goals))
            .with_semantic_filter(temporal_candidate_filter(&filters, goals));
        Self { query }
    }

    pub fn into_query(self) -> SessionTemporalQuery {
        self.query
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
    encode_tagged_lowercase_hex("sha256:", &Sha256::digest(encoded.as_bytes()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LcmDescribeServiceCommand {
    provider: String,
    session_id: SessionId,
    target: LcmDescribeTarget,
    grain: RetrievalGrainV1,
    store_scope: SessionRetrievalStoreScope,
}

impl LcmDescribeServiceCommand {
    pub fn new(
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

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn target(&self) -> &LcmDescribeTarget {
        &self.target
    }

    #[hotpath::skip]
    pub const fn grain(&self) -> RetrievalGrainV1 {
        self.grain
    }

    #[hotpath::skip]
    pub const fn store_scope(&self) -> SessionRetrievalStoreScope {
        self.store_scope
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LcmExpandServiceCommand {
    provider: String,
    session_id: SessionId,
    target: LcmExpandTarget,
    grain: RetrievalGrainV1,
    content_slice: LcmContentSlice,
    source_limit: Option<usize>,
    cursor: Option<String>,
    store_scope: SessionRetrievalStoreScope,
}

impl LcmExpandServiceCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: impl Into<String>,
        session_id: SessionId,
        target: LcmExpandTarget,
        grain: RetrievalGrainV1,
        content_slice: LcmContentSlice,
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
            source_limit,
            cursor,
            store_scope,
        }
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn target(&self) -> &LcmExpandTarget {
        &self.target
    }

    #[hotpath::skip]
    pub const fn grain(&self) -> RetrievalGrainV1 {
        self.grain
    }

    #[hotpath::skip]
    pub const fn content_slice(&self) -> LcmContentSlice {
        self.content_slice
    }

    #[hotpath::skip]
    pub const fn source_limit(&self) -> Option<usize> {
        self.source_limit
    }

    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    #[hotpath::skip]
    pub const fn store_scope(&self) -> SessionRetrievalStoreScope {
        self.store_scope
    }
}

pub type LcmDescribeServiceFuture<'a> =
    Pin<Box<dyn Future<Output = LcmDescribeServiceOutcome> + Send + 'a>>;
pub type LcmExpandServiceFuture<'a> =
    Pin<Box<dyn Future<Output = LcmExpandServiceOutcome> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionRetrievalExplanationView {
    pub anchor: RetrievalAnchorId,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionRetrievalOmissionView {
    pub rank: u32,
    pub anchor: RetrievalAnchorId,
    pub reason: HydrationStateV1,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SessionTemporalWatermarksView {
    pub generation: u64,
    pub source: u64,
    pub projection: u64,
    pub index: u64,
    pub summary: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SessionTemporalMetadataView {
    pub anchors: Vec<RetrievalAnchorId>,
    pub watermarks: SessionTemporalWatermarksView,
    pub coverage: TemporalCoverageCountsV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_coverage: Vec<SessionSourceCoverageV1>,
    pub cursor: Option<String>,
    pub explanations: Vec<SessionRetrievalExplanationView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omissions: Vec<SessionRetrievalOmissionView>,
    pub authorized_root: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionRetrievalUnavailableReason {
    ServiceNotConfigured,
    RefreshWorkerMissing,
    RefreshWorkerRecovering,
    RefreshWorkerStalled,
    RefreshWorkerStopped,
    HistoricalConvergence,
    HistoricalRetry,
    HistoricalBlocked,
    TemporalStoreUnavailable,
    HydrationUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRetrievalWorkerBlocker {
    WorkerMissing,
    WorkerPanicked,
    WorkerStopped,
    Storage,
    Projector,
    Deadline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRetrievalWorkerRetryClass {
    Storage,
    Projector,
    Deadline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SessionRetrievalWorkerStatusView {
    pub last_progress_at_unix_micros: Option<i64>,
    pub backlog: usize,
    pub blocker: Option<SessionRetrievalWorkerBlocker>,
    pub retry_class: Option<SessionRetrievalWorkerRetryClass>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionRetrievalUnavailable {
    pub reason: SessionRetrievalUnavailableReason,
    pub worker: Option<SessionRetrievalWorkerStatusView>,
}

impl SessionRetrievalUnavailable {
    #[hotpath::skip]
    pub const fn service_not_configured() -> Self {
        Self {
            reason: SessionRetrievalUnavailableReason::ServiceNotConfigured,
            worker: None,
        }
    }

    #[hotpath::skip]
    pub const fn without_worker(reason: SessionRetrievalUnavailableReason) -> Self {
        Self {
            reason,
            worker: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum LcmDescribeServiceOutcome {
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
    CursorStale,
    WrongScope,
    Locked,
    Redacted,
    Deleted,
    Denied,
    ResetRequired {
        store_scope: SessionRetrievalStoreScope,
    },
    Unavailable(SessionRetrievalUnavailable),
    CursorManifestLimitExceeded {
        kind: CursorManifestLimitKindV1,
        observed: usize,
        maximum: usize,
    },
    BudgetExhausted,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum LcmExpandServiceOutcome {
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
    CursorStale,
    WrongScope,
    Locked,
    Redacted,
    Deleted,
    Denied,
    ResetRequired {
        store_scope: SessionRetrievalStoreScope,
    },
    Unavailable(SessionRetrievalUnavailable),
    CursorManifestLimitExceeded {
        kind: CursorManifestLimitKindV1,
        observed: usize,
        maximum: usize,
    },
    BudgetExhausted,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionRetrievalPageView {
    pub results: Vec<SessionMessageSearchResult>,
    pub temporal: SessionTemporalMetadataView,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionRetrievalServiceOutcome {
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
    CursorStale,
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
    ResetRequired {
        store_scope: SessionRetrievalStoreScope,
    },
    Unavailable(SessionRetrievalUnavailable),
    CursorManifestLimitExceeded {
        kind: CursorManifestLimitKindV1,
        observed: usize,
        maximum: usize,
    },
    BudgetExhausted {
        stage: tracedecay_session_memory::session::SessionRetrievalBudgetStageV1,
    },
    TimedOut,
    Cancelled,
}
