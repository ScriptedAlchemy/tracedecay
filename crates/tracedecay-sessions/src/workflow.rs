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

fn matches_token(value: &str, tokens: &[&str]) -> bool {
    tokens.iter().any(|token| value.eq_ignore_ascii_case(token))
}

#[cfg(test)]
mod tests {
    use super::WorkflowStatus;

    #[test]
    fn workflow_status_preserves_unknown_values() {
        assert_eq!(WorkflowStatus::from_disk("done"), WorkflowStatus::Completed);
        assert_eq!(WorkflowStatus::from_disk("blocked"), WorkflowStatus::Failed);
        assert_eq!(
            WorkflowStatus::from_disk("future-state"),
            WorkflowStatus::Unknown
        );
    }
}
