use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CompactContextLineageEdgeV1, CursorManifestLimitKindV1, HydrationStateV1, RetrievalAnchorId,
    RetrievalGrainV1, SessionId, SessionSourceCoverageV1, TemporalCoverageCountsV1, TemporalModeV1,
};

use super::lcm_args::{
    message_search_time_range, parse_git_scope_filter, parse_message_search_provider_scope,
    parse_message_search_scope, parse_session_message_type,
};
use super::sessions_for::render_message_search_md;
use super::*;
use crate::application::session::{
    SessionDataFreshness, SessionFreshnessPolicy, SessionRetrievalScope, SessionTemporalQuery,
};
use crate::sessions::lcm::{
    LcmContentSlice, LcmDescribeResponse, LcmDescribeTarget, LcmExpandResponse, LcmExpandTarget,
};
use crate::query::temporal::context::ContextBudget;
use crate::query::temporal::ports::{
    TemporalCandidateFilterV1, TemporalMessageTypeFilterV1, TemporalSessionScopeFilterV1,
};
use crate::query::temporal::ranking::DiversityLimits;

pub(super) struct MessageSearchRequest<'a> {
    pub(super) query: &'a str,
    pub(super) provider_scope: ProviderScope,
    pub(super) requested_provider: Option<&'static str>,
    pub(super) project_key: Option<&'a str>,
    pub(super) parent_session_id: Option<&'a str>,
    pub(super) workflow_run: Option<&'a str>,
    pub(super) workflow_agent: Option<&'a str>,
    pub(super) include_subagents: bool,
    pub(super) catch_up: bool,
    pub(super) cursor: Option<&'a str>,
    pub(super) scope: SessionSearchScope,
    pub(super) message_type: SessionMessageType,
    pub(super) limit: usize,
    pub(super) git_filter: GitScopeFilter,
    pub(super) time_range: SessionSearchTimeRange,
    pub(super) workflow_scope: Option<WorkflowScopeFilter>,
    /// When true, ignore FTS and list each session's latest goal
    /// (`kind = 'goal'`) instead. `query` is optional in this mode.
    pub(super) goals: bool,
}

fn optional_message_search_string<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(Some)
        .ok_or_else(|| argument_error(format!("{name} must be a non-empty string")))
}

pub(super) fn parse_message_search_request(args: &Value) -> Result<MessageSearchRequest<'_>> {
    let goals = super::lcm_args::bool_arg(args, "goals")?.unwrap_or(false);
    let query = match optional_message_search_string(args, "query")? {
        Some(query) => query,
        // In goals-listing mode the query is optional: the listing is not an
        // FTS search, so an absent query simply lists the most recent goals.
        None if goals => "",
        None => {
            return Err(TraceDecayError::Config {
                message: "missing required parameter: query".to_string(),
            });
        }
    };
    let provider_scope = parse_message_search_provider_scope(args)?;
    let workflow_run = optional_message_search_string(args, "workflow_run")?;
    let workflow_agent = optional_message_search_string(args, "workflow_agent")?;
    if workflow_agent.is_some() && workflow_run.is_none() {
        return Err(argument_error(
            "workflow_agent requires workflow_run to avoid broadening retrieval",
        ));
    }
    let include_subagents = super::lcm_args::bool_arg(args, "include_subagents")?.unwrap_or(true);
    let limit = match args.get("limit") {
        None => 10,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| argument_error("limit must be a non-negative integer"))?
            .clamp(1, 50) as usize,
    };
    let mut scope = parse_message_search_scope(args)?;
    if !include_subagents && matches!(scope, SessionSearchScope::SubagentsOnly) {
        return Err(argument_error(
            "include_subagents=false cannot be combined with scope=subagents_only",
        ));
    }
    if !include_subagents && matches!(scope, SessionSearchScope::All) {
        scope = SessionSearchScope::ParentsOnly;
    }
    Ok(MessageSearchRequest {
        query,
        provider_scope,
        requested_provider: provider_scope.provider_id(),
        project_key: optional_message_search_string(args, "project_key")?,
        parent_session_id: optional_message_search_string(args, "parent_session_id")?,
        workflow_run,
        workflow_agent,
        include_subagents,
        catch_up: super::lcm_args::bool_arg(args, "catch_up")?.unwrap_or(false),
        cursor: optional_message_search_string(args, "cursor")?,
        scope,
        message_type: parse_session_message_type(args)?,
        limit,
        git_filter: parse_git_scope_filter(args)?,
        time_range: message_search_time_range(args)?,
        workflow_scope: workflow_run.map(|run_id| WorkflowScopeFilter {
            run_id: run_id.to_string(),
            agent_label: workflow_agent.map(str::to_string),
        }),
        goals,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionRetrievalStoreScope {
    Project,
    Profile,
}

impl SessionRetrievalStoreScope {
    const fn as_str(self) -> &'static str {
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
#[allow(dead_code)] // Slice 1 exposes this contract before server injection is wired.
pub(crate) struct SessionRetrievalCommand {
    query: SessionTemporalQuery,
    filters: SessionRetrievalFilters,
    goals: bool,
    store_scope: SessionRetrievalStoreScope,
    project_selector: Option<SessionRetrievalProjectSelector>,
}

#[allow(dead_code)] // Consumed by the injected service in the follow-up wiring slice.
impl SessionRetrievalCommand {
    pub(crate) fn new(
        query: SessionTemporalQuery,
        filters: SessionRetrievalFilters,
        goals: bool,
        store_scope: SessionRetrievalStoreScope,
    ) -> Self {
        let query = query
            .with_compatibility_filter_digest(compatibility_filter_digest(&filters, goals))
            .with_semantic_filter(temporal_candidate_filter(&filters, goals));
        Self {
            query,
            filters,
            goals,
            store_scope,
            project_selector: None,
        }
    }

    pub(crate) fn query(&self) -> &SessionTemporalQuery {
        &self.query
    }

    pub(crate) fn filters(&self) -> &SessionRetrievalFilters {
        &self.filters
    }

    pub(crate) const fn goals(&self) -> bool {
        self.goals
    }

    pub(crate) const fn store_scope(&self) -> SessionRetrievalStoreScope {
        self.store_scope
    }

    pub(crate) fn project_selector(&self) -> Option<&SessionRetrievalProjectSelector> {
        self.project_selector.as_ref()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionRetrievalNextActionView {
    pub(crate) kind: &'static str,
    pub(crate) tool: &'static str,
    pub(crate) action: &'static str,
    pub(crate) reason: &'static str,
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
    #[allow(dead_code)]
    // Complete terminal-reason contract; semantic eligibility runs before temporal ranking, so the
    // retrieval adapter must never terminate a query as unsupported.
    UnsupportedQuery,
    #[allow(dead_code)]
    // Complete terminal-reason contract; produced by the injected retrieval service in the follow-up wiring slice.
    RequestContextInvalid,
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
            Self::UnsupportedQuery => "unsupported_query",
            Self::RequestContextInvalid => "request_context_invalid",
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
#[allow(dead_code)] // The injected compatibility boundary preserves every typed terminal state.
#[allow(clippy::large_enum_variant)]
pub(crate) enum LcmDescribeServiceOutcome {
    Complete {
        description: LcmDescribeResponse,
        temporal: SessionTemporalMetadataView,
        grain: RetrievalGrainV1,
        state: HydrationStateV1,
        lineage: Vec<CompactContextLineageEdgeV1>,
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
#[allow(dead_code)] // The injected compatibility boundary preserves every typed terminal state.
#[allow(clippy::large_enum_variant)]
pub(crate) enum LcmExpandServiceOutcome {
    Complete {
        expansion: LcmExpandResponse,
        temporal: SessionTemporalMetadataView,
        grain: RetrievalGrainV1,
        state: HydrationStateV1,
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
#[allow(dead_code)] // The adapter outcome surface is intentionally complete before injection.
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

fn base_message_search_payload(request: &MessageSearchRequest<'_>) -> Value {
    let mut payload = json!({
        "status": "ok",
        "outcome": "complete_zero",
        "provider": request.requested_provider.unwrap_or("all"),
        "requested_provider": request.requested_provider,
        "project_key": request.project_key,
        "parent_session_id": request.parent_session_id,
        "include_subagents": request.include_subagents,
        "catch_up": request.catch_up,
        "catch_up_performed": false,
        "catch_up_failures": [],
        "catch_up_provider": request.provider_scope.response_label(),
        "scope": request.scope.as_str(),
        "message_type": request.message_type.as_str(),
        "since": request.time_range.start_time,
        "until": request.time_range.end_time,
        "query": request.query,
        "goals": request.goals,
        "count": 0,
        "results": [],
        "refresh_required": false,
        "next_action": Value::Null,
    });
    if !request.git_filter.is_empty()
        && let Some(map) = payload.as_object_mut()
    {
        map.insert(
            "git_filter".to_string(),
            serde_json::to_value(&request.git_filter).unwrap_or(Value::Null),
        );
        map.insert("git_filter_applied".to_string(), Value::Bool(true));
    }
    if request.workflow_scope.is_some()
        && let Some(map) = payload.as_object_mut()
    {
        map.insert(
            "workflow_run".to_string(),
            request
                .workflow_run
                .map_or(Value::Null, |run| Value::String(run.to_string())),
        );
        if let Some(label) = request.workflow_agent {
            map.insert(
                "workflow_agent".to_string(),
                Value::String(label.to_string()),
            );
        }
        map.insert("workflow_filter_applied".to_string(), Value::Bool(true));
        map.insert("workflow_run_parent_session".to_string(), Value::Null);
    }
    payload
}

fn temporal_value(
    temporal: &SessionTemporalMetadataView,
    freshness: SessionDataFreshness,
) -> Value {
    let freshness = match freshness {
        SessionDataFreshness::Fresh => json!({ "state": "fresh" }),
        SessionDataFreshness::Stored { generation_lag } => {
            json!({ "state": "stored", "generation_lag": generation_lag })
        }
        SessionDataFreshness::Partial { generation_lag } => {
            json!({ "state": "partial", "generation_lag": generation_lag })
        }
    };
    let mut value = json!({
        "anchors": temporal.anchors,
        "watermarks": temporal.watermarks,
        "coverage": temporal.coverage,
        "cursor": temporal.cursor,
        "explanations": temporal.explanations,
        "freshness": freshness,
    });
    if !temporal.source_coverage.is_empty() {
        value["source_coverage"] = json!(temporal.source_coverage);
    }
    if !temporal.omissions.is_empty() {
        value["omissions"] = json!(temporal.omissions);
    }
    value
}

const fn refresh_next_action() -> SessionRetrievalNextActionView {
    SessionRetrievalNextActionView {
        kind: "session_refresh",
        tool: "tracedecay_session_refresh",
        action: "begin",
        reason: "the authorized session-temporal store does not satisfy the requested freshness precondition",
    }
}

fn apply_page(
    payload: &mut Value,
    page: SessionRetrievalPageView,
    freshness: SessionDataFreshness,
) {
    let SessionRetrievalPageView { results, temporal } = page;
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    if let Some(root) = &temporal.authorized_root {
        map.insert(
            "selected_project_root".to_string(),
            Value::String(root.clone()),
        );
    }
    map.insert("count".to_string(), json!(results.len()));
    map.insert(
        "results".to_string(),
        serde_json::to_value(results).unwrap_or_else(|_| json!([])),
    );
    map.insert("temporal".to_string(), temporal_value(&temporal, freshness));
}

fn apply_temporal(
    payload: &mut Value,
    temporal: &SessionTemporalMetadataView,
    freshness: SessionDataFreshness,
) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    if let Some(root) = &temporal.authorized_root {
        map.insert(
            "selected_project_root".to_string(),
            Value::String(root.clone()),
        );
    }
    map.insert("temporal".to_string(), temporal_value(temporal, freshness));
}

fn apply_refresh_guidance(payload: &mut Value, required: bool) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    map.insert("refresh_required".to_string(), Value::Bool(required));
    map.insert(
        "next_action".to_string(),
        if required {
            serde_json::to_value(refresh_next_action()).unwrap_or(Value::Null)
        } else {
            Value::Null
        },
    );
}

fn apply_typed_error(payload: &mut Value, status: &str, code: &str, message: &str) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    map.insert("status".to_string(), Value::String(status.to_string()));
    map.insert("outcome".to_string(), Value::String(status.to_string()));
    map.insert("message".to_string(), Value::String(message.to_string()));
    map.insert(
        "error".to_string(),
        json!({
            "code": code,
            "message": message,
            "retryable": false
        }),
    );
}

fn apply_unavailable(payload: &mut Value, unavailable: SessionRetrievalUnavailable) {
    apply_typed_error(
        payload,
        "unavailable",
        "session_retrieval_service_unavailable",
        "the authorized session retrieval service is unavailable",
    );
    payload["error"]["reason"] = json!(unavailable.reason.as_str());
    payload["error"]["retryable"] = json!(unavailable.reason.is_retryable());
    if let Some(worker) = unavailable.worker {
        payload["service_status"] = json!({
            "last_progress_at_unix_micros": worker.last_progress_at_unix_micros,
            "backlog": worker.backlog,
            "blocker": worker.blocker.map(SessionRetrievalWorkerBlocker::as_str),
            "retry_class": worker.retry_class.map(SessionRetrievalWorkerRetryClass::as_str),
        });
    }
}

fn render_service_outcome(
    request: &MessageSearchRequest<'_>,
    outcome: SessionRetrievalServiceOutcome,
) -> Value {
    let mut payload = base_message_search_payload(request);
    match outcome {
        SessionRetrievalServiceOutcome::Complete { page, freshness } => {
            payload["outcome"] = json!("complete");
            apply_page(&mut payload, page, freshness);
        }
        SessionRetrievalServiceOutcome::CompleteZero {
            temporal,
            freshness,
        } => {
            apply_temporal(&mut payload, &temporal, freshness);
        }
        SessionRetrievalServiceOutcome::Stale {
            temporal,
            freshness,
        } => {
            payload["status"] = json!("stale");
            payload["outcome"] = json!("stale");
            apply_temporal(&mut payload, &temporal, freshness);
            apply_refresh_guidance(&mut payload, request.catch_up);
        }
        SessionRetrievalServiceOutcome::Partial {
            page,
            freshness,
            omitted,
        } => {
            payload["status"] = json!("partial");
            payload["outcome"] = json!("partial");
            payload["omitted"] = json!(omitted);
            let refresh_required = request.catch_up
                && matches!(
                    freshness,
                    SessionDataFreshness::Stored { .. } | SessionDataFreshness::Partial { .. }
                );
            apply_page(&mut payload, page, freshness);
            apply_refresh_guidance(&mut payload, refresh_required);
        }
        SessionRetrievalServiceOutcome::WrongScope => apply_typed_error(
            &mut payload,
            "wrong_scope",
            "session_retrieval_wrong_scope",
            "the injected retrieval service does not own the requested root",
        ),
        SessionRetrievalServiceOutcome::Locked => apply_typed_error(
            &mut payload,
            "locked",
            "session_retrieval_locked",
            "the authorized session-temporal store is locked",
        ),
        SessionRetrievalServiceOutcome::Redacted => apply_typed_error(
            &mut payload,
            "redacted",
            "session_retrieval_redacted",
            "the requested session evidence is redacted",
        ),
        SessionRetrievalServiceOutcome::Deleted => apply_typed_error(
            &mut payload,
            "deleted",
            "session_retrieval_deleted",
            "the requested session evidence was deleted",
        ),
        SessionRetrievalServiceOutcome::Denied => apply_typed_error(
            &mut payload,
            "denied",
            "session_retrieval_denied",
            "session retrieval was denied",
        ),
        SessionRetrievalServiceOutcome::Unavailable(unavailable) => {
            apply_unavailable(&mut payload, unavailable);
        }
        SessionRetrievalServiceOutcome::CursorManifestLimitExceeded {
            kind,
            observed,
            maximum,
        } => {
            apply_typed_error(
                &mut payload,
                "cursor_manifest_limit_exceeded",
                "session_cursor_manifest_limit_exceeded",
                "session retrieval cursor manifest exceeded its canonical bound",
            );
            payload["error"]["kind"] = json!(kind);
            payload["error"]["observed"] = json!(observed);
            payload["error"]["maximum"] = json!(maximum);
        }
        SessionRetrievalServiceOutcome::BudgetExhausted => apply_typed_error(
            &mut payload,
            "budget_exhausted",
            "session_retrieval_budget_exhausted",
            "session retrieval exhausted its bounded work budget",
        ),
        SessionRetrievalServiceOutcome::Cancelled => apply_typed_error(
            &mut payload,
            "cancelled",
            "session_retrieval_cancelled",
            "session retrieval was cancelled",
        ),
    }
    payload
}

fn optional_owned_string(value: Option<&Value>, name: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .map(Some)
        .ok_or_else(|| argument_error(format!("{name} must be a non-empty string")))
}

fn project_selector(args: &Value) -> Result<Option<SessionRetrievalProjectSelector>> {
    let nested = args.get("project_selector");
    let nested = nested
        .map(|value| {
            value
                .as_object()
                .ok_or_else(|| argument_error("project_selector must be an object"))
        })
        .transpose()?;
    let project_id = nested
        .and_then(|selector| selector.get("project_id"))
        .or_else(|| args.get("project_id"));
    let project_path = nested
        .and_then(|selector| {
            selector
                .get("path")
                .or_else(|| selector.get("project_path"))
        })
        .or_else(|| args.get("project_path"));
    let selector = SessionRetrievalProjectSelector {
        project_id: optional_owned_string(project_id, "project_id")?,
        project_path: optional_owned_string(project_path, "project_path")?,
    };
    if selector.project_id.is_none() && selector.project_path.is_none() {
        if nested.is_some()
            || args.get("project_id").is_some()
            || args.get("project_path").is_some()
        {
            return Err(argument_error(
                "project selector must include project_id or project_path",
            ));
        }
        return Ok(None);
    }
    Ok(Some(selector))
}

fn retrieval_command(
    request: &MessageSearchRequest<'_>,
    store_scope: SessionRetrievalStoreScope,
    project_selector: Option<SessionRetrievalProjectSelector>,
) -> Result<SessionRetrievalCommand> {
    let query = SessionTemporalQuery::new(
        SessionId::new("session.message-search.root").map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?,
        request.requested_provider.map(str::to_string),
        request.query,
        request.cursor.map(str::to_string),
        TemporalModeV1::Current,
        RetrievalGrainV1::LogicalMessage,
        request.limit,
        DiversityLimits::default(),
        ContextBudget {
            max_bytes: 64 * 1024,
            max_tokens: 16 * 1024,
            estimator_version: "words-v1".to_string(),
        },
    )
    .map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })?
    .with_retrieval_scope(SessionRetrievalScope::AllSessionsInAuthorizedRoot)
    .with_freshness_policy(if request.catch_up {
        SessionFreshnessPolicy::RequireFresh
    } else {
        SessionFreshnessPolicy::AllowStored
    });
    let filters = SessionRetrievalFilters {
        project_key: request.project_key.map(str::to_string),
        parent_session_id: request.parent_session_id.map(str::to_string),
        source: None,
        include_summaries: false,
        scope: request.scope,
        message_type: request.message_type,
        roles: Vec::new(),
        time_range: request.time_range,
        git_filter: request.git_filter.clone(),
        workflow_scope: request.workflow_scope.clone(),
    };
    let mut command = SessionRetrievalCommand::new(query, filters, request.goals, store_scope);
    command.project_selector = project_selector;
    Ok(command)
}

fn deferred_all_registered_payload(request: &MessageSearchRequest<'_>) -> Value {
    let mut payload = base_message_search_payload(request);
    apply_typed_error(
        &mut payload,
        "deferred",
        "all_registered_deferred_to_pr15",
        "all_registered session retrieval requires PR15 canonical multi-root scope",
    );
    payload["project_scope"] = json!("all_registered");
    payload["searched_project_count"] = json!(0);
    payload["skipped_project_count"] = json!(0);
    payload["catch_up_skipped_project_count"] = json!(0);
    payload
}

fn render_temporal_message_search_md(payload: &Value) -> String {
    let mut markdown = render_message_search_md(payload);
    if let Some(coverage) = payload
        .get("temporal")
        .and_then(|temporal| temporal.get("coverage"))
    {
        let _ = write!(
            markdown,
            "\n- Coverage: visible {}, hidden {}, unknown {}, redacted {}\n",
            coverage["visible"].as_u64().unwrap_or_default(),
            coverage["hidden"].as_u64().unwrap_or_default(),
            coverage["unknown"].as_u64().unwrap_or_default(),
            coverage["redacted"].as_u64().unwrap_or_default(),
        );
    }
    if payload
        .get("refresh_required")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        markdown.push_str(
            "- Refresh required: run `tracedecay_session_refresh` with action `begin`.\n",
        );
    }
    if let Some(error) = payload.get("error") {
        let _ = writeln!(
            markdown,
            "- Problem: `{}` — {}",
            error["code"].as_str().unwrap_or("session_retrieval_error"),
            error["message"]
                .as_str()
                .unwrap_or("session retrieval failed"),
        );
        if let Some(reason) = error.get("reason").and_then(Value::as_str) {
            let _ = writeln!(markdown, "- Unavailable reason: `{reason}`");
        }
    }
    if let Some(status) = payload.get("service_status") {
        let last_progress = status
            .get("last_progress_at_unix_micros")
            .and_then(Value::as_i64)
            .map_or_else(|| "none".to_string(), |value| value.to_string());
        let _ = writeln!(
            markdown,
            "- Refresh worker: last progress {last_progress}, backlog {}, blocker `{}`, retry class `{}`",
            status["backlog"].as_u64().unwrap_or_default(),
            status["blocker"].as_str().unwrap_or("none"),
            status["retry_class"].as_str().unwrap_or("none"),
        );
    }
    markdown
}

pub(crate) async fn handle_message_search_with_service(
    project_root: Option<&Path>,
    store_scope: SessionRetrievalStoreScope,
    args: Value,
    service: Option<&dyn SessionRetrievalServicePort>,
) -> Result<ToolResult> {
    let request = parse_message_search_request(&args)?;
    let project_selector = project_selector(&args)?;
    let has_project_selector = project_selector.is_some();
    let project_scope = optional_message_search_string(&args, "project_scope")?;
    if let Some(project_scope) = project_scope {
        if project_scope != "all_registered" {
            return Err(argument_error(
                "project_scope must be omitted or all_registered",
            ));
        }
        if project_selector.is_some() {
            return Err(argument_error(
                "project_scope cannot be combined with project_id, project_path, or project_selector",
            ));
        }
        let payload = deferred_all_registered_payload(&request);
        return Ok(tool_json_with_md(project_root, &args, &payload, || {
            render_temporal_message_search_md(&payload)
        }));
    }
    if matches!(store_scope, SessionRetrievalStoreScope::Profile) && project_selector.is_some() {
        return Err(argument_error(
            "profile session storage cannot be combined with a project selector",
        ));
    }
    let command = retrieval_command(&request, store_scope, project_selector)?;
    let outcome = match service {
        Some(service) => service.execute(command).await,
        None => SessionRetrievalServiceOutcome::Unavailable(
            SessionRetrievalUnavailable::service_not_configured(),
        ),
    };
    let mut payload = render_service_outcome(&request, outcome);
    payload["store_scope"] = json!(store_scope.as_str());
    if matches!(store_scope, SessionRetrievalStoreScope::Project)
        && project_root.is_some()
        && !has_project_selector
    {
        payload["selected_project_root"] = json!(project_root);
    }
    Ok(tool_json_with_md(project_root, &args, &payload, || {
        render_temporal_message_search_md(&payload)
    }))
}

#[cfg(test)]
mod cutover_tests {
    use std::path::Path;
    use std::sync::Mutex;

    use serde_json::{Value, json};
    use tracedecay_domain::{
        RetrievalAnchorId, SessionSourceCoverageV1, SessionSourceFrontierV1, SessionSourceIdV1,
        SessionTemporalCoverageRequestV1, TemporalCoverageCountsV1, TemporalModeV1,
    };

    use super::{
        SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalPageView,
        SessionRetrievalServiceFuture, SessionRetrievalServiceOutcome, SessionRetrievalServicePort,
        SessionRetrievalStoreScope, SessionRetrievalUnavailable, SessionRetrievalUnavailableReason,
        SessionRetrievalWorkerBlocker, SessionRetrievalWorkerRetryClass,
        SessionRetrievalWorkerStatusView, SessionTemporalMetadataView,
        SessionTemporalWatermarksView, handle_message_search_with_service,
        render_temporal_message_search_md,
    };
    use crate::application::session::{
        SessionDataFreshness, SessionFreshnessPolicy, SessionRetrievalScope,
    };
    use crate::query::temporal::ports::{
        TemporalMessageTypeFilterV1, TemporalSessionScopeFilterV1,
    };

    #[derive(Default)]
    struct RecordingService {
        commands: Mutex<Vec<SessionRetrievalCommand>>,
        outcome: Mutex<Option<SessionRetrievalServiceOutcome>>,
    }

    impl RecordingService {
        fn with_outcome(outcome: SessionRetrievalServiceOutcome) -> Self {
            Self {
                commands: Mutex::new(Vec::new()),
                outcome: Mutex::new(Some(outcome)),
            }
        }

        fn calls(&self) -> usize {
            self.commands.lock().unwrap().len()
        }

        fn command(&self) -> SessionRetrievalCommand {
            self.commands.lock().unwrap()[0].clone()
        }
    }

    impl SessionRetrievalServicePort for RecordingService {
        fn execute(&self, command: SessionRetrievalCommand) -> SessionRetrievalServiceFuture<'_> {
            self.commands.lock().unwrap().push(command);
            let outcome = self.outcome.lock().unwrap().clone().unwrap_or(
                SessionRetrievalServiceOutcome::CompleteZero {
                    temporal: temporal(),
                    freshness: SessionDataFreshness::Fresh,
                },
            );
            Box::pin(async move { outcome })
        }
    }

    fn temporal() -> SessionTemporalMetadataView {
        SessionTemporalMetadataView {
            anchors: vec![RetrievalAnchorId::new("anchor.message.1").unwrap()],
            watermarks: SessionTemporalWatermarksView::default(),
            coverage: TemporalCoverageCountsV1 {
                visible: 1,
                hidden: 2,
                unknown: 3,
                redacted: 4,
            },
            source_coverage: Vec::new(),
            cursor: Some("cursor.next".to_string()),
            explanations: vec![SessionRetrievalExplanationView {
                anchor: RetrievalAnchorId::new("anchor.message.1").unwrap(),
                summary: "exact phrase and current evidence".to_string(),
            }],
            omissions: Vec::new(),
            authorized_root: None,
        }
    }

    fn temporal_with_stale_source() -> SessionTemporalMetadataView {
        SessionTemporalMetadataView {
            source_coverage: vec![
                SessionSourceCoverageV1::from_frontiers(
                    SessionSourceIdV1::new("cursor").unwrap(),
                    SessionSourceFrontierV1::new(10),
                    SessionSourceFrontierV1::new(5),
                    SessionSourceFrontierV1::new(10),
                    SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current),
                )
                .unwrap(),
            ],
            ..temporal()
        }
    }

    fn json_args() -> Value {
        json!({
            "query": "database backup",
            "format": "json"
        })
    }

    fn response_payload(result: &crate::mcp::tools::ToolResult) -> Value {
        serde_json::from_str(result.value["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn existing_filters_translate_to_one_root_wide_temporal_query() {
        let service = RecordingService::default();
        let args = json!({
            "query": " database backup ",
            "provider": "claude",
            "project_key": "project-key",
            "parent_session_id": "parent-session",
            "include_subagents": false,
            "message_type": "direct_user",
            "since": 10,
            "until": 20,
            "branch": "feature/message-search",
            "workflow_run": "wf_123",
            "workflow_agent": "researcher",
            "limit": 7,
            "format": "json"
        });

        handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            args,
            Some(&service),
        )
        .await
        .unwrap();

        let command = service.command();
        assert_eq!(command.query().query(), "database backup");
        assert_eq!(command.query().provider(), Some("claude"));
        assert_eq!(command.query().limit(), 7);
        assert_eq!(
            command.query().retrieval_scope(),
            &SessionRetrievalScope::AllSessionsInAuthorizedRoot
        );
        assert_eq!(
            command.query().freshness_policy(),
            SessionFreshnessPolicy::AllowStored
        );
        assert_eq!(
            command.filters().project_key.as_deref(),
            Some("project-key")
        );
        assert_eq!(
            command.filters().parent_session_id.as_deref(),
            Some("parent-session")
        );
        assert_eq!(command.filters().scope.as_str(), "parents_only");
        assert_eq!(command.filters().message_type.as_str(), "direct_user");
        assert_eq!(command.filters().time_range.start_time, Some(10));
        assert_eq!(command.filters().time_range.end_time, Some(20));
        assert_eq!(
            command.filters().git_filter.branch.as_deref(),
            Some("feature/message-search")
        );
        assert_eq!(
            command
                .filters()
                .workflow_scope
                .as_ref()
                .map(|scope| scope.run_id.as_str()),
            Some("wf_123")
        );
        let semantic = command.query().semantic_filter();
        assert_eq!(semantic.project_key.as_deref(), Some("project-key"));
        assert_eq!(
            semantic.parent_session_id.as_deref(),
            Some("parent-session")
        );
        assert_eq!(
            semantic.session_scope,
            TemporalSessionScopeFilterV1::ParentsOnly
        );
        assert_eq!(
            semantic.message_type,
            TemporalMessageTypeFilterV1::DirectUser
        );
        assert_eq!(
            semantic.git_branch.as_deref(),
            Some("feature/message-search")
        );
        assert_eq!(semantic.workflow_run.as_deref(), Some("wf_123"));
        assert_eq!(semantic.workflow_agent.as_deref(), Some("researcher"));
        assert_eq!(
            (semantic.start_time, semantic.end_time),
            (Some(10), Some(20))
        );
        assert!(!command.goals());
    }

    #[tokio::test]
    async fn compatibility_filters_bind_the_temporal_cursor_request() {
        let first = RecordingService::default();
        handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json!({
                "query": "database backup",
                "scope": "parents_only",
                "message_type": "direct_user",
                "since": 10,
                "until": 20,
                "format": "json"
            }),
            Some(&first),
        )
        .await
        .unwrap();

        let changed = RecordingService::default();
        handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json!({
                "query": "database backup",
                "scope": "subagents_only",
                "message_type": "tool_result",
                "since": 11,
                "until": 21,
                "format": "json"
            }),
            Some(&changed),
        )
        .await
        .unwrap();

        let first = first.command();
        let changed = changed.command();
        assert_ne!(
            first.query().compatibility_filter_digest(),
            changed.query().compatibility_filter_digest()
        );
        assert!(
            first
                .query()
                .compatibility_filter_digest()
                .is_some_and(|digest| digest.starts_with("sha256:"))
        );
    }

    #[tokio::test]
    async fn goals_mode_keeps_query_optional_but_normal_search_requires_it() {
        let service = RecordingService::default();
        handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json!({"goals": true, "format": "json"}),
            Some(&service),
        )
        .await
        .unwrap();
        let command = service.command();
        assert_eq!(command.query().query(), "");
        assert!(command.goals());
        assert!(command.query().semantic_filter().goals);

        let missing = RecordingService::default();
        let error = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json!({"format": "json"}),
            Some(&missing),
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("missing required parameter: query")
        );
        assert_eq!(missing.calls(), 0);
    }

    #[tokio::test]
    async fn malformed_optional_arguments_are_rejected_without_broadening() {
        for (field, value) in [
            ("provider", json!(7)),
            ("project_key", json!(false)),
            ("parent_session_id", json!([])),
            ("include_subagents", json!("yes")),
            ("catch_up", json!(1)),
            ("limit", json!("ten")),
            ("project_scope", json!(true)),
        ] {
            let service = RecordingService::default();
            let mut args = json_args();
            args[field] = value;
            let error = handle_message_search_with_service(
                Some(Path::new("/repo")),
                SessionRetrievalStoreScope::Project,
                args,
                Some(&service),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains(field), "{error}");
            assert_eq!(service.calls(), 0);
        }

        let service = RecordingService::default();
        let mut args = json_args();
        args["workflow_agent"] = json!("researcher");
        let error = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            args,
            Some(&service),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("workflow_run"), "{error}");
        assert_eq!(service.calls(), 0);

        let service = RecordingService::default();
        let mut args = json_args();
        args["include_subagents"] = json!(false);
        args["scope"] = json!("subagents_only");
        let error = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            args,
            Some(&service),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("include_subagents"), "{error}");
        assert_eq!(service.calls(), 0);
    }

    #[tokio::test]
    async fn catch_up_defaults_false_and_true_is_only_a_freshness_precondition() {
        let stored = RecordingService::default();
        let stored_result = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json_args(),
            Some(&stored),
        )
        .await
        .unwrap();
        let stored_payload = response_payload(&stored_result);
        assert_eq!(
            stored.command().query().freshness_policy(),
            SessionFreshnessPolicy::AllowStored
        );
        assert_eq!(stored_payload["catch_up"], false);
        assert_eq!(stored_payload["catch_up_performed"], false);
        assert_eq!(stored_payload["catch_up_failures"], json!([]));

        let fresh = RecordingService::default();
        let mut args = json_args();
        args["catch_up"] = json!(true);
        let fresh_result = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            args,
            Some(&fresh),
        )
        .await
        .unwrap();
        let fresh_payload = response_payload(&fresh_result);
        assert_eq!(
            fresh.command().query().freshness_policy(),
            SessionFreshnessPolicy::RequireFresh
        );
        assert_eq!(fresh.calls(), 1);
        assert_eq!(fresh_payload["catch_up"], true);
        assert_eq!(fresh_payload["catch_up_performed"], false);
        assert_eq!(fresh_payload["catch_up_failures"], json!([]));
        assert_eq!(fresh_payload["refresh_required"], false);
    }

    #[tokio::test]
    async fn stale_freshness_precondition_returns_coverage_and_typed_refresh_action() {
        let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Stale {
            temporal: temporal_with_stale_source(),
            freshness: SessionDataFreshness::Stored { generation_lag: 5 },
        });
        let result = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json!({
                "query": "database backup",
                "catch_up": true,
                "format": "json"
            }),
            Some(&service),
        )
        .await
        .unwrap();
        let payload = response_payload(&result);

        assert_eq!(payload["status"], "stale");
        assert_eq!(payload["outcome"], "stale");
        assert_eq!(payload["refresh_required"], true);
        assert_eq!(payload["next_action"]["kind"], "session_refresh");
        assert_eq!(payload["next_action"]["tool"], "tracedecay_session_refresh");
        assert_eq!(payload["temporal"]["coverage"]["visible"], 1);
        assert_eq!(payload["temporal"]["coverage"]["hidden"], 2);
        assert_eq!(payload["temporal"]["freshness"]["generation_lag"], 5);
        assert_eq!(
            payload["temporal"]["source_coverage"][0]["source_id"],
            "cursor"
        );
        assert_eq!(
            payload["temporal"]["source_coverage"][0]["observed_frontier"],
            10
        );
        assert_eq!(
            payload["temporal"]["source_coverage"][0]["committed_frontier"],
            5
        );
        assert_eq!(
            payload["temporal"]["source_coverage"][0]["reason"]["kind"],
            "projection_behind_source"
        );
        assert_eq!(payload["catch_up_performed"], false);
        assert_eq!(payload["catch_up_failures"], json!([]));
    }

    #[tokio::test]
    async fn partial_outcome_preserves_results_temporal_metadata_and_omissions() {
        let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Partial {
            page: SessionRetrievalPageView {
                results: Vec::new(),
                temporal: temporal(),
            },
            freshness: SessionDataFreshness::Stored { generation_lag: 2 },
            omitted: 9,
        });
        let result = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json!({
                "query": "database backup",
                "catch_up": true,
                "format": "json"
            }),
            Some(&service),
        )
        .await
        .unwrap();
        let payload = response_payload(&result);

        assert_eq!(payload["status"], "partial");
        assert_eq!(payload["outcome"], "partial");
        assert_eq!(payload["omitted"], 9);
        assert_eq!(payload["refresh_required"], true);
        assert_eq!(payload["temporal"]["anchors"][0], "anchor.message.1");
        assert_eq!(payload["temporal"]["cursor"], "cursor.next");
        assert_eq!(
            payload["temporal"]["explanations"][0]["summary"],
            "exact phrase and current evidence"
        );
    }

    #[tokio::test]
    async fn fresh_partial_outcome_uses_cursor_without_requesting_refresh() {
        let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Partial {
            page: SessionRetrievalPageView {
                results: Vec::new(),
                temporal: temporal(),
            },
            freshness: SessionDataFreshness::Fresh,
            omitted: 3,
        });
        let result = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json!({
                "query": "database backup",
                "catch_up": true,
                "format": "json"
            }),
            Some(&service),
        )
        .await
        .unwrap();
        let payload = response_payload(&result);

        assert_eq!(payload["outcome"], "partial");
        assert_eq!(payload["omitted"], 3);
        assert_eq!(payload["temporal"]["freshness"]["state"], "fresh");
        assert_eq!(payload["temporal"]["cursor"], "cursor.next");
        assert_eq!(payload["refresh_required"], false);
        assert!(payload["next_action"].is_null());
    }

    #[tokio::test]
    async fn all_registered_is_pr15_deferred_without_registry_or_service_call() {
        let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Denied);
        let result = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json!({
                "query": "database backup",
                "project_scope": "all_registered",
                "format": "json"
            }),
            Some(&service),
        )
        .await
        .unwrap();
        let payload = response_payload(&result);

        assert_eq!(service.calls(), 0);
        assert_eq!(payload["status"], "deferred");
        assert_eq!(payload["outcome"], "deferred");
        assert_eq!(payload["project_scope"], "all_registered");
        assert_eq!(payload["error"]["code"], "all_registered_deferred_to_pr15");
        assert_eq!(payload["results"], json!([]));
    }

    #[tokio::test]
    async fn explicit_project_selector_and_profile_dispatch_remain_single_root() {
        let project = RecordingService::default();
        handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json!({
                "query": "database backup",
                "project_selector": {
                    "project_id": "project.target",
                    "path": "/target"
                },
                "format": "json"
            }),
            Some(&project),
        )
        .await
        .unwrap();
        let project_command = project.command();
        assert_eq!(
            project_command.store_scope(),
            SessionRetrievalStoreScope::Project
        );
        assert_eq!(
            project_command
                .project_selector()
                .and_then(|selector| selector.project_id.as_deref()),
            Some("project.target")
        );
        assert_eq!(
            project_command
                .project_selector()
                .and_then(|selector| selector.project_path.as_deref()),
            Some("/target")
        );

        let profile = RecordingService::default();
        handle_message_search_with_service(
            None,
            SessionRetrievalStoreScope::Profile,
            json_args(),
            Some(&profile),
        )
        .await
        .unwrap();
        assert_eq!(
            profile.command().store_scope(),
            SessionRetrievalStoreScope::Profile
        );
        assert!(profile.command().project_selector().is_none());
    }

    #[tokio::test]
    async fn complete_zero_and_terminal_error_outcomes_are_typed() {
        let complete_zero =
            RecordingService::with_outcome(SessionRetrievalServiceOutcome::CompleteZero {
                temporal: temporal(),
                freshness: SessionDataFreshness::Fresh,
            });
        let complete_zero_result = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json_args(),
            Some(&complete_zero),
        )
        .await
        .unwrap();
        let complete_zero_payload = response_payload(&complete_zero_result);
        assert_eq!(complete_zero_payload["status"], "ok");
        assert_eq!(complete_zero_payload["outcome"], "complete_zero");
        assert_eq!(
            complete_zero_payload["temporal"]["freshness"]["state"],
            "fresh"
        );
        assert_eq!(
            complete_zero_payload["temporal"]["anchors"][0],
            "anchor.message.1"
        );
        assert_eq!(complete_zero_payload["temporal"]["cursor"], "cursor.next");
        assert_eq!(
            complete_zero_payload["temporal"]["explanations"][0]["summary"],
            "exact phrase and current evidence"
        );

        let complete = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Complete {
            page: SessionRetrievalPageView {
                results: Vec::new(),
                temporal: temporal(),
            },
            freshness: SessionDataFreshness::Fresh,
        });
        let complete_result = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json_args(),
            Some(&complete),
        )
        .await
        .unwrap();
        let complete_payload = response_payload(&complete_result);
        assert_eq!(complete_payload["status"], "ok");
        assert_eq!(complete_payload["outcome"], "complete");
        assert_eq!(complete_payload["temporal"]["freshness"]["state"], "fresh");

        let terminal = [
            (
                SessionRetrievalServiceOutcome::WrongScope,
                "wrong_scope",
                "session_retrieval_wrong_scope",
            ),
            (
                SessionRetrievalServiceOutcome::Locked,
                "locked",
                "session_retrieval_locked",
            ),
            (
                SessionRetrievalServiceOutcome::Redacted,
                "redacted",
                "session_retrieval_redacted",
            ),
            (
                SessionRetrievalServiceOutcome::Deleted,
                "deleted",
                "session_retrieval_deleted",
            ),
            (
                SessionRetrievalServiceOutcome::Denied,
                "denied",
                "session_retrieval_denied",
            ),
            (
                SessionRetrievalServiceOutcome::Unavailable(
                    SessionRetrievalUnavailable::service_not_configured(),
                ),
                "unavailable",
                "session_retrieval_service_unavailable",
            ),
            (
                SessionRetrievalServiceOutcome::BudgetExhausted,
                "budget_exhausted",
                "session_retrieval_budget_exhausted",
            ),
            (
                SessionRetrievalServiceOutcome::Cancelled,
                "cancelled",
                "session_retrieval_cancelled",
            ),
        ];
        for (outcome, status, code) in terminal {
            let service = RecordingService::with_outcome(outcome);
            let result = handle_message_search_with_service(
                Some(Path::new("/repo")),
                SessionRetrievalStoreScope::Project,
                json_args(),
                Some(&service),
            )
            .await
            .unwrap();
            let payload = response_payload(&result);
            assert_eq!(payload["status"], status);
            assert_eq!(payload["outcome"], status);
            assert_eq!(payload["error"]["code"], code);
        }
    }

    #[tokio::test]
    async fn unavailable_outcome_exposes_typed_worker_status() {
        let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Unavailable(
            SessionRetrievalUnavailable {
                reason: SessionRetrievalUnavailableReason::RefreshWorkerRecovering,
                worker: Some(SessionRetrievalWorkerStatusView {
                    last_progress_at_unix_micros: Some(42),
                    backlog: 7,
                    blocker: Some(SessionRetrievalWorkerBlocker::WorkerPanicked),
                    retry_class: Some(SessionRetrievalWorkerRetryClass::Projector),
                }),
            },
        ));

        let result = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json_args(),
            Some(&service),
        )
        .await
        .unwrap();
        let payload = response_payload(&result);

        assert_eq!(payload["error"]["reason"], "refresh_worker_recovering");
        assert_eq!(payload["error"]["retryable"], true);
        assert_eq!(
            payload["service_status"]["last_progress_at_unix_micros"],
            42
        );
        assert_eq!(payload["service_status"]["backlog"], 7);
        assert_eq!(payload["service_status"]["blocker"], "worker_panicked");
        assert_eq!(payload["service_status"]["retry_class"], "projector");
        let markdown = render_temporal_message_search_md(&payload);
        assert!(markdown.contains("Unavailable reason: `refresh_worker_recovering`"));
        assert!(markdown.contains(
            "Refresh worker: last progress 42, backlog 7, blocker `worker_panicked`, retry class `projector`"
        ));
    }

    #[tokio::test]
    async fn unavailable_outcome_reports_no_progress_backlog_as_stalled() {
        let service = RecordingService::with_outcome(SessionRetrievalServiceOutcome::Unavailable(
            SessionRetrievalUnavailable {
                reason: SessionRetrievalUnavailableReason::RefreshWorkerStalled,
                worker: Some(SessionRetrievalWorkerStatusView {
                    last_progress_at_unix_micros: None,
                    backlog: 14,
                    blocker: Some(SessionRetrievalWorkerBlocker::Storage),
                    retry_class: Some(SessionRetrievalWorkerRetryClass::Storage),
                }),
            },
        ));

        let result = handle_message_search_with_service(
            Some(Path::new("/repo")),
            SessionRetrievalStoreScope::Project,
            json_args(),
            Some(&service),
        )
        .await
        .unwrap();
        let payload = response_payload(&result);

        assert_eq!(payload["error"]["reason"], "refresh_worker_stalled");
        assert_eq!(payload["error"]["retryable"], true);
        assert_eq!(
            payload["service_status"]["last_progress_at_unix_micros"],
            Value::Null
        );
        assert_eq!(payload["service_status"]["backlog"], 14);
        assert_eq!(payload["service_status"]["blocker"], "storage");
        assert_eq!(payload["service_status"]["retry_class"], "storage");
    }
}
