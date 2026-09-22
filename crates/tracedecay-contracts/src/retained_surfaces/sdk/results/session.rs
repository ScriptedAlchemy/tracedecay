use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    HydrationStateV1, SessionSourceCoverageReasonV1, SessionSourceCoverageStateV1, TemporalModeV1,
};

use super::{RetainedErrorV1, RetainedOutcomeStatusV1};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRecordV1 {
    pub provider: String,
    pub session_id: String,
    pub project_key: String,
    pub project_path: String,
    pub title: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub transcript_path: Option<String>,
    pub metadata_json: Option<String>,
    pub parent_session_id: Option<String>,
    pub is_subagent: bool,
    pub agent_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionMessageV1 {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub role: String,
    pub timestamp: Option<i64>,
    pub ordinal: i64,
    pub text: String,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<String>,
    pub source_path: Option<String>,
    pub source_offset: Option<i64>,
    pub metadata_json: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MessageSearchHitV1 {
    pub session: SessionRecordV1,
    pub message: SessionMessageV1,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitScopeV1 {
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub commit: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalCoverageV1 {
    pub visible: u64,
    pub hidden: u64,
    pub unknown: u64,
    pub redacted: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalWatermarksV1 {
    pub generation: u64,
    pub source: u64,
    pub projection: u64,
    pub index: u64,
    pub summary: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalExplanationV1 {
    pub anchor: String,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HydrationStateResultV1 {
    Available,
    RetainedButUnavailable,
    Redacted,
    Deleted,
    RetentionExpired,
    Unauthorized,
    Locked,
    UnverifiableLegacy,
}

impl From<HydrationStateV1> for HydrationStateResultV1 {
    fn from(value: HydrationStateV1) -> Self {
        match value {
            HydrationStateV1::Available => Self::Available,
            HydrationStateV1::RetainedButUnavailable => Self::RetainedButUnavailable,
            HydrationStateV1::Redacted => Self::Redacted,
            HydrationStateV1::Deleted => Self::Deleted,
            HydrationStateV1::RetentionExpired => Self::RetentionExpired,
            HydrationStateV1::Unauthorized => Self::Unauthorized,
            HydrationStateV1::Locked => Self::Locked,
            HydrationStateV1::UnverifiableLegacy => Self::UnverifiableLegacy,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalOmissionV1 {
    pub rank: u32,
    pub anchor: String,
    pub reason: HydrationStateResultV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TemporalCoverageOmissionV1 {
    RootContinuationUnavailable {
        strict_population: TemporalPopulationCountV1,
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TemporalPopulationCountV1 {
    Exact { count: u64 },
    AtLeast { count: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionSourceCoverageV1 {
    pub source_id: String,
    pub observed_frontier: u64,
    pub committed_frontier: u64,
    pub target_watermark: u64,
    pub request: SessionCoverageRequestV1,
    pub covered_intervals: Vec<SessionCoverageIntervalV1>,
    pub missing_intervals: Vec<SessionCoverageIntervalV1>,
    pub state: SessionCoverageStateV1,
    pub reason: SessionCoverageReasonV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionCoverageRequestV1 {
    pub mode: SessionCoverageModeV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionCoverageModeV1 {
    Current,
    AsOf { cutoff: i64 },
    Evolution,
    Forensic,
}

impl From<TemporalModeV1> for SessionCoverageModeV1 {
    fn from(value: TemporalModeV1) -> Self {
        match value {
            TemporalModeV1::Current => Self::Current,
            TemporalModeV1::AsOf { cutoff } => Self::AsOf { cutoff: cutoff.0 },
            TemporalModeV1::Evolution => Self::Evolution,
            TemporalModeV1::Forensic => Self::Forensic,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionCoverageStateV1 {
    Fresh,
    Stale,
    Partial,
    Locked,
    Redacted,
    RetentionWithheld,
    Unavailable,
}

impl From<SessionSourceCoverageStateV1> for SessionCoverageStateV1 {
    fn from(value: SessionSourceCoverageStateV1) -> Self {
        match value {
            SessionSourceCoverageStateV1::Fresh => Self::Fresh,
            SessionSourceCoverageStateV1::Stale => Self::Stale,
            SessionSourceCoverageStateV1::Partial => Self::Partial,
            SessionSourceCoverageStateV1::Locked => Self::Locked,
            SessionSourceCoverageStateV1::Redacted => Self::Redacted,
            SessionSourceCoverageStateV1::RetentionWithheld => Self::RetentionWithheld,
            SessionSourceCoverageStateV1::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionCoverageIntervalV1 {
    pub knowledge: ClosedUtcIntervalV1,
    pub valid: ValidCoverageIntervalV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClosedUtcIntervalV1 {
    pub from_inclusive: Option<i64>,
    pub through_inclusive: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", content = "interval", rename_all = "snake_case")]
pub enum ValidCoverageIntervalV1 {
    Known(ClosedUtcIntervalV1),
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionCoverageReasonV1 {
    CaughtUp,
    ProjectionBehindSource {
        lag: u64,
    },
    SourceBehindTarget {
        lag: u64,
    },
    ProjectionAndSourceBehind {
        projection_lag: u64,
        source_lag: u64,
    },
    Locked,
    Redacted,
    RetentionWithheld,
    Unavailable,
}

impl From<&SessionSourceCoverageReasonV1> for SessionCoverageReasonV1 {
    fn from(value: &SessionSourceCoverageReasonV1) -> Self {
        match value {
            SessionSourceCoverageReasonV1::CaughtUp => Self::CaughtUp,
            SessionSourceCoverageReasonV1::ProjectionBehindSource { lag } => {
                Self::ProjectionBehindSource { lag: *lag }
            }
            SessionSourceCoverageReasonV1::SourceBehindTarget { lag } => {
                Self::SourceBehindTarget { lag: *lag }
            }
            SessionSourceCoverageReasonV1::ProjectionAndSourceBehind {
                projection_lag,
                source_lag,
            } => Self::ProjectionAndSourceBehind {
                projection_lag: *projection_lag,
                source_lag: *source_lag,
            },
            SessionSourceCoverageReasonV1::Locked => Self::Locked,
            SessionSourceCoverageReasonV1::Redacted => Self::Redacted,
            SessionSourceCoverageReasonV1::RetentionWithheld => Self::RetentionWithheld,
            SessionSourceCoverageReasonV1::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalMetadataV1 {
    pub anchors: Vec<String>,
    pub watermarks: TemporalWatermarksV1,
    pub coverage: TemporalCoverageV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_coverage: Vec<SessionSourceCoverageV1>,
    pub explanations: Vec<TemporalExplanationV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omissions: Vec<TemporalOmissionV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage_omissions: Vec<TemporalCoverageOmissionV1>,
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<TemporalFreshnessV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TemporalFreshnessV1 {
    Fresh,
    Stored { generation_lag: u64 },
    Partial { generation_lag: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetainedNextActionV1 {
    pub kind: String,
    pub tool: String,
    pub action: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalWorkerStatusV1 {
    pub last_progress_at_unix_micros: Option<i64>,
    pub backlog: usize,
    pub blocker: Option<String>,
    pub retry_class: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageSearchFreshnessV1 {
    Fresh,
    Stored,
    Partial,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MessageSearchRootV1 {
    pub project_id: String,
    pub root: String,
    pub status: RetainedOutcomeStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<MessageSearchFreshnessV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MessageSearchSkipV1 {
    pub project_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MessageSearchResultV1 {
    pub catch_up: bool,
    pub catch_up_failures: Vec<String>,
    pub catch_up_performed: bool,
    pub catch_up_provider: String,
    pub count: Option<usize>,
    pub goals: bool,
    pub include_subagents: bool,
    pub message_type: String,
    pub next_action: Option<RetainedNextActionV1>,
    pub outcome: RetainedOutcomeStatusV1,
    pub parent_session_id: Option<String>,
    pub project_key: Option<String>,
    pub provider: String,
    pub query: Option<String>,
    pub refresh_required: bool,
    pub requested_provider: Option<String>,
    pub results: Option<Vec<MessageSearchHitV1>>,
    pub scope: String,
    pub since: Option<i64>,
    pub status: RetainedOutcomeStatusV1,
    pub until: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RetainedErrorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_filter: Option<GitScopeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_filter_applied: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<MessageSearchRootV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searched_project_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_project_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_status: Option<RetrievalWorkerStatusV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<Vec<MessageSearchSkipV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_project_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<TemporalMetadataV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_filter_applied: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_run_parent_session: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshFrontierResultV1 {
    pub observed_through: u64,
    pub committed_through: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshProgressV1 {
    pub operation_id: String,
    pub session_id: String,
    pub frontier: SessionRefreshFrontierResultV1,
    pub coverage: TemporalCoverageV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_coverage: Vec<SessionSourceCoverageV1>,
    pub committed_batches: u64,
    pub committed_records: u64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionRefreshTerminalStateResultV1 {
    Complete,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshReceiptV1 {
    pub operation_id: String,
    pub session_id: String,
    pub frontier: SessionRefreshFrontierResultV1,
    pub coverage: TemporalCoverageV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_coverage: Vec<SessionSourceCoverageV1>,
    pub state: SessionRefreshTerminalStateResultV1,
    pub failure_code: Option<String>,
    pub terminal_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshStatusResultV1 {
    pub outcome: RetainedOutcomeStatusV1,
    pub scope: String,
    pub tool: String,
    pub progress: Option<SessionRefreshProgressV1>,
    pub receipt: Option<SessionRefreshReceiptV1>,
    pub error: Option<RetainedErrorV1>,
}

macro_rules! refresh_effect_result {
    ($name:ident) => {
        #[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            pub outcome: RetainedOutcomeStatusV1,
            pub scope: String,
            pub tool: String,
            pub accepted_at: Option<i64>,
            pub handle: Option<String>,
            pub operation_id: Option<String>,
            pub progress: Option<SessionRefreshProgressV1>,
            pub receipt: Option<SessionRefreshReceiptV1>,
            pub error: Option<RetainedErrorV1>,
        }
    };
}

refresh_effect_result!(SessionRefreshCancelResultV1);
refresh_effect_result!(SessionRefreshBeginResultV1);

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionCorrelationHitV1 {
    pub provider: String,
    pub session_id: String,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    pub event_count: i64,
    pub span_count: i64,
    pub sources: Vec<String>,
    pub commit_sha: Option<String>,
    pub committed_at: Option<i64>,
    pub span_overlap_kind: Option<String>,
    pub relation: Option<String>,
    pub evidence: Option<String>,
    pub confidence: Option<i64>,
    pub evidence_message_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CorrelationIndexCountModeV1 {
    PresenceOnly,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CorrelationIndexV1 {
    pub projection_available: bool,
    pub generation: Option<String>,
    pub source_watermark: Option<String>,
    pub spans_present: bool,
    pub commits_present: bool,
    pub span_count: Option<u64>,
    pub commit_count: Option<u64>,
    pub backfill_watermark: Option<i64>,
    pub count_mode: CorrelationIndexCountModeV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionsForResultV1 {
    pub count: usize,
    pub results: Vec<SessionCorrelationHitV1>,
    pub status: RetainedOutcomeStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<CorrelationIndexV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_empty: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_sessions: Option<Vec<SessionCorrelationHitV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatusV1 {
    Running,
    Completed,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowQueryModeV1 {
    Session,
    GitScope,
    Run,
    Agent,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowCoverageV1 {
    Complete,
    Conclusive,
    BoundedPrefix,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunV1 {
    pub run_id: String,
    pub parent_session_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub phase_json: Option<String>,
    pub status: WorkflowStatusV1,
    pub started_ts: Option<i64>,
    pub ended_ts: Option<i64>,
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub agent_count: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowAgentV1 {
    pub run_id: String,
    pub agent_label: String,
    pub agent_id: String,
    pub phase: Option<String>,
    pub transcript_path: Option<String>,
    pub agent_session_id: Option<String>,
    pub status: WorkflowStatusV1,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub tokens: i64,
    pub started_ts: Option<i64>,
    pub ended_ts: Option<i64>,
}

const fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowsResultV1 {
    pub status: RetainedOutcomeStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<WorkflowAgentV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<WorkflowAgentV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents_complete: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents_coverage: Option<WorkflowCoverageV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents_returned: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RetainedErrorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_filter: Option<GitScopeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lookup_complete: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lookup_coverage: Option<WorkflowCoverageV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<WorkflowQueryModeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<WorkflowRunV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<WorkflowRunV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[cfg(test)]
mod coverage_projection_tests {
    use tracedecay_domain::{
        HydrationStateV1, SessionSourceCoverageReasonV1, SessionSourceCoverageStateV1,
        TemporalModeV1, UtcMicros,
    };

    use super::{
        HydrationStateResultV1, SessionCoverageModeV1, SessionCoverageReasonV1,
        SessionCoverageStateV1,
    };

    #[test]
    fn coverage_projection_keeps_mode_cutoff_and_status_labels() {
        assert_eq!(
            SessionCoverageModeV1::from(TemporalModeV1::AsOf {
                cutoff: UtcMicros(7),
            }),
            SessionCoverageModeV1::AsOf { cutoff: 7 }
        );
        assert_eq!(
            SessionCoverageModeV1::from(TemporalModeV1::Forensic),
            SessionCoverageModeV1::Forensic
        );
        assert_eq!(
            SessionCoverageStateV1::from(SessionSourceCoverageStateV1::RetentionWithheld),
            SessionCoverageStateV1::RetentionWithheld
        );
        assert_eq!(
            SessionCoverageReasonV1::from(&SessionSourceCoverageReasonV1::ProjectionBehindSource {
                lag: 4
            }),
            SessionCoverageReasonV1::ProjectionBehindSource { lag: 4 }
        );
        assert_eq!(
            HydrationStateResultV1::from(HydrationStateV1::UnverifiableLegacy),
            HydrationStateResultV1::UnverifiableLegacy
        );
    }
}
