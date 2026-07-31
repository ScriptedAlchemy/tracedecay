use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

/// Search scope for messages emitted by one workflow run or agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowScopeFilter {
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// Lifecycle state of a workflow run or agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Running,
    Completed,
    Failed,
    Unknown,
}

impl WorkflowStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_disk(value: &str) -> Self {
        let trimmed = value.trim();
        if matches_token(trimmed, &["completed", "done", "success", "succeeded"]) {
            Self::Completed
        } else if matches_token(
            trimmed,
            &["running", "in_progress", "started", "active", "pending"],
        ) {
            Self::Running
        } else if matches_token(
            trimmed,
            &[
                "failed",
                "error",
                "errored",
                "blocked",
                "interrupted",
                "cancelled",
                "canceled",
                "timeout",
                "timed_out",
            ],
        ) {
            Self::Failed
        } else {
            Self::Unknown
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub run_id: String,
    pub parent_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_json: Option<String>,
    pub status: WorkflowStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_ts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_ts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub agent_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowAgent {
    pub run_id: String,
    pub agent_label: String,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    pub status: WorkflowStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_ts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_ts: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowGitScope {
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowRunScope {
    Session { session_id: String },
    GitScope(WorkflowGitScope),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowRunListRequest {
    pub scope: WorkflowRunScope,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowRunDetailRequest {
    pub run_id: String,
    pub limit: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowIndexState {
    AuthorityNotRetained,
    IndexNotBuilt,
}

impl WorkflowIndexState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthorityNotRetained => "authority_not_retained",
            Self::IndexNotBuilt => "workflow_index_not_built",
        }
    }

    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::IndexNotBuilt)
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::AuthorityNotRetained => "registered project session database is unavailable",
            Self::IndexNotBuilt => "the workflow index has not been built for this project yet",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowRunListOutcome {
    Runs(Vec<WorkflowRun>),
    Unavailable(WorkflowIndexState),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowRunDetail {
    pub run: WorkflowRun,
    pub agents: Vec<WorkflowAgent>,
    /// Total indexed agents for the run, independent of the requested bound.
    pub agent_count: i64,
    /// Whether `agents` completely answers the request. For run detail this
    /// means every indexed agent is present; for an exact-label lookup it
    /// means the label was checked without relying on a bounded prefix.
    pub agents_complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowRunDetailOutcome {
    Run(Box<WorkflowRunDetail>),
    NotFound,
    Unavailable(WorkflowIndexState),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowReadError {
    message: String,
}

impl WorkflowReadError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WorkflowReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WorkflowReadError {}

pub type WorkflowRunListFuture<'a> =
    Pin<Box<dyn Future<Output = Result<WorkflowRunListOutcome, WorkflowReadError>> + Send + 'a>>;
pub type WorkflowRunDetailFuture<'a> =
    Pin<Box<dyn Future<Output = Result<WorkflowRunDetailOutcome, WorkflowReadError>> + Send + 'a>>;

pub trait WorkflowIndexReadPort: Send + Sync {
    fn runs(&self, request: WorkflowRunListRequest) -> WorkflowRunListFuture<'_>;

    fn run(&self, request: WorkflowRunDetailRequest) -> WorkflowRunDetailFuture<'_>;

    /// Looks up one agent label without treating a bounded run prefix as
    /// authoritative absence.
    ///
    /// Implementations with exact storage lookup should override this method.
    /// The fallback performs one bounded probe and leaves `agents_complete`
    /// false when that cannot prove absence.
    fn agent(&self, run_id: String, agent_label: String) -> WorkflowRunDetailFuture<'_> {
        Box::pin(async move {
            let outcome = self
                .run(WorkflowRunDetailRequest { run_id, limit: 1 })
                .await?;
            let mut detail = match outcome {
                WorkflowRunDetailOutcome::Run(detail) => detail,
                other => return Ok(other),
            };
            detail
                .agents
                .retain(|agent| agent.agent_label == agent_label);
            if !detail.agents.is_empty() {
                detail.agents_complete = true;
            }
            Ok(WorkflowRunDetailOutcome::Run(detail))
        })
    }
}

fn matches_token(value: &str, tokens: &[&str]) -> bool {
    tokens.iter().any(|token| value.eq_ignore_ascii_case(token))
}

#[cfg(test)]
mod tests {
    use super::{
        WorkflowIndexState, WorkflowRunDetailOutcome, WorkflowRunListOutcome, WorkflowStatus,
    };

    #[test]
    fn workflow_status_preserves_unknown_values() {
        assert_eq!(WorkflowStatus::from_disk("done"), WorkflowStatus::Completed);
        assert_eq!(WorkflowStatus::from_disk("blocked"), WorkflowStatus::Failed);
        assert_eq!(
            WorkflowStatus::from_disk("future-state"),
            WorkflowStatus::Unknown
        );
    }

    #[test]
    fn workflow_list_distinguishes_empty_from_unbuilt() {
        let empty = WorkflowRunListOutcome::Runs(Vec::new());
        let unbuilt = WorkflowRunListOutcome::Unavailable(WorkflowIndexState::IndexNotBuilt);

        assert!(matches!(empty, WorkflowRunListOutcome::Runs(runs) if runs.is_empty()));
        assert!(matches!(
            unbuilt,
            WorkflowRunListOutcome::Unavailable(WorkflowIndexState::IndexNotBuilt)
        ));
    }

    #[test]
    fn workflow_unavailable_states_preserve_mount_authority() {
        assert_ne!(
            WorkflowIndexState::AuthorityNotRetained,
            WorkflowIndexState::IndexNotBuilt
        );
    }

    #[test]
    fn workflow_detail_distinguishes_missing_from_unbuilt() {
        assert!(matches!(
            WorkflowRunDetailOutcome::NotFound,
            WorkflowRunDetailOutcome::NotFound
        ));
        assert!(matches!(
            WorkflowRunDetailOutcome::Unavailable(WorkflowIndexState::IndexNotBuilt),
            WorkflowRunDetailOutcome::Unavailable(WorkflowIndexState::IndexNotBuilt)
        ));
    }
}
