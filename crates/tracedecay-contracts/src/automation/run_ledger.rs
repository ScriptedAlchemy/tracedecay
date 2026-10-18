//! Durable automation run ledger records: the rows the automation runtime
//! appends for every run and the run-inspection tools return verbatim.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::retrieval::SessionRetrievalBudgetStageV1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskKind {
    MemoryCurator,
    SessionReflector,
    SkillWriter,
    CombinedReview,
    UserJob,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskFailureClass {
    Retryable,
    Permanent,
    Timeout,
    Unavailable,
    Denied,
    Disconnected,
    MalformedOutput,
    /// The backend refused the request because it exceeds its input or
    /// context-window limit.
    InputTooLarge,
}

impl AgentTaskFailureClass {
    pub fn is_retryable(self) -> bool {
        // Denial is a policy state: retrying without a configuration change
        // reproduces it, so it is never retried. A disconnect means the
        // backend was reached and may be reachable again.
        matches!(
            self,
            Self::Retryable | Self::Timeout | Self::Unavailable | Self::Disconnected
        )
    }

    /// A later run may succeed where this one failed: every retryable class,
    /// plus malformed output, which a fresh prompt can repair, and an
    /// oversized input, which a later run bounds from fresh evidence.
    pub fn is_retryable_on_later_run(self) -> bool {
        self.is_retryable() || matches!(self, Self::MalformedOutput | Self::InputTooLarge)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentTaskRetryAttempt {
    pub attempt: u32,
    pub succeeded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_classification: Option<AgentTaskFailureClass>,
    pub backoff_millis: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTrigger {
    #[default]
    ManualCli,
    ManualMcp,
    Dashboard,
    Application,
    Scheduler,
    HostReceipt,
}

impl AutomationTrigger {
    /// Explicit operator-triggered runs are admitted independently of whether
    /// recurring scheduling is enabled. Backend availability, host mode,
    /// policy, cancellation, and deadline checks still apply.
    pub const fn is_on_demand(self) -> bool {
        matches!(
            self,
            Self::ManualCli | Self::ManualMcp | Self::Dashboard | Self::Application
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

impl AutomationRunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Skipped)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunArtifactKind {
    Traces,
    Feedback,
    GeneratedEvals,
    ValidationGate,
    OptimizerDiagnosis,
    CodexHandoff,
}

impl AutomationRunArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Traces => "traces",
            Self::Feedback => "feedback",
            Self::GeneratedEvals => "generated_evals",
            Self::ValidationGate => "validation_gate",
            Self::OptimizerDiagnosis => "optimizer_diagnosis",
            Self::CodexHandoff => "codex_handoff",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "traces" => Some(Self::Traces),
            "feedback" => Some(Self::Feedback),
            "generated_evals" => Some(Self::GeneratedEvals),
            "validation_gate" => Some(Self::ValidationGate),
            "optimizer_diagnosis" => Some(Self::OptimizerDiagnosis),
            "codex_handoff" => Some(Self::CodexHandoff),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AutomationRunArtifact {
    pub schema_version: u32,
    pub kind: String,
    pub path: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AutomationRunLedgerRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub trigger: AutomationTrigger,
    pub task: AgentTaskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_key: Option<String>,
    pub backend: String,
    /// The durable backend/configuration identity this run executed under.
    ///
    /// A settled deterministic failure stays suppressed only while this
    /// matches the identity now configured, so the scheduler can re-admit the
    /// task the moment the backend or configuration changes. Records written
    /// before this field existed carry `None` and never suppress: an
    /// unidentified failure cannot be shown to have failed under the current
    /// identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict_json: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: AutomationRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_ops: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_ops: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_ops: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_report: Option<Value>,
    #[serde(default)]
    pub reviewed_count: usize,
    pub accepted_count: usize,
    pub rejected_count: usize,
    #[serde(default)]
    pub skipped_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Exhausted retrieval boundary of a `session_evidence_budget_exhausted`
    /// skip; present exactly on those skips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_evidence_budget_stage: Option<SessionRetrievalBudgetStageV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_classification: Option<AgentTaskFailureClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_retryable: Option<bool>,
    #[serde(default)]
    pub backend_attempt_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backend_attempts: Vec<AgentTaskRetryAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_ref: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<AutomationRunArtifact>,
    pub started_at: String,
    pub completed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_micros: Option<i64>,
}
