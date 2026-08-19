//! Evidence-bearing observations for the Observatory's product-health views.
//!
//! These records keep an observed negative distinct from missing join evidence.
//! Projectors may count only the facts carried here; they may not infer a
//! correctness, cost, or effect outcome from temporal proximity.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::CoverageStateV1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelianceDecisionV1 {
    Accepted,
    Rejected,
    Overridden,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelianceVerificationV1 {
    Correct,
    Incorrect,
    NoEligibleVerification,
    Unknown,
    Censored,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AppropriateRelianceObservedV1 {
    pub decision_ref: String,
    pub decision: RelianceDecisionV1,
    pub verification: RelianceVerificationV1,
    pub independently_verified: bool,
    pub override_rationale_present: bool,
}

impl AppropriateRelianceObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_ref(&self.decision_ref)?;
        if matches!(self.decision, RelianceDecisionV1::Overridden)
            && !self.override_rationale_present
        {
            return Err("override_rationale");
        }
        match self.verification {
            RelianceVerificationV1::Correct | RelianceVerificationV1::Incorrect
                if !self.independently_verified =>
            {
                Err("independent_verification")
            }
            RelianceVerificationV1::NoEligibleVerification
            | RelianceVerificationV1::Unknown
            | RelianceVerificationV1::Censored
                if self.independently_verified =>
            {
                Err("independent_verification")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedTernaryV1 {
    Yes,
    No,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTerminalV1 {
    Succeeded,
    Failed,
    Skipped,
    Running,
    Queued,
}

impl AutomationTerminalV1 {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Skipped)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutomationFunnelObservedV1 {
    pub run_ref: String,
    pub ledger_coverage: CoverageStateV1,
    pub eligible: ObservedTernaryV1,
    pub admitted: ObservedTernaryV1,
    pub executed: ObservedTernaryV1,
    pub useful_work: ObservedTernaryV1,
    pub effect: ObservedTernaryV1,
    pub recovery: ObservedTernaryV1,
    pub terminal: AutomationTerminalV1,
}

impl AutomationFunnelObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_ref(&self.run_ref)?;
        if self.ledger_coverage == CoverageStateV1::Sampled {
            return Err("ledger_coverage");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDecisionDispositionV1 {
    Allow,
    Deny,
    Abstain,
    Indeterminate,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskCalibrationEvidenceV1 {
    pub cohort_ref: String,
    pub support: u32,
    pub support_floor: u32,
    pub drift_valid: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskIntelligenceDecisionObservedV1 {
    pub proposal_ref: String,
    pub task_ref: String,
    pub evaluator_revision: u64,
    pub disposition: TaskDecisionDispositionV1,
    pub deterministic_fallback: bool,
    pub calibration: Option<TaskCalibrationEvidenceV1>,
    pub decomposition_candidate_count: Option<u32>,
    pub route_candidate_count: Option<u32>,
}

impl TaskIntelligenceDecisionObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_ref(&self.proposal_ref)?;
        validate_ref(&self.task_ref)?;
        if self.evaluator_revision == 0 {
            return Err("evaluator_revision");
        }
        if let Some(calibration) = &self.calibration {
            validate_ref(&calibration.cohort_ref)?;
            if calibration.support_floor == 0 || calibration.support < calibration.support_floor {
                return Err("calibration_support");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcomeV1 {
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskIntelligenceOutcomeObservedV1 {
    pub proposal_ref: String,
    pub attempt_ref: String,
    pub outcome: TaskOutcomeV1,
    pub independently_reviewed: ObservedTernaryV1,
    pub accepted: ObservedTernaryV1,
    pub effect: ObservedTernaryV1,
}

impl TaskIntelligenceOutcomeObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_ref(&self.proposal_ref)?;
        validate_ref(&self.attempt_ref)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptTerminalV1 {
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderReliabilityObservedV1 {
    pub attempt_ref: String,
    pub backend: String,
    pub protocol: String,
    pub model: Option<String>,
    pub fallback: ObservedTernaryV1,
    pub progress: ObservedTernaryV1,
    pub cancellation: ObservedTernaryV1,
    pub recovery: ObservedTernaryV1,
    pub artifact_count: u32,
    pub terminal: ProviderAttemptTerminalV1,
    pub effect: ObservedTernaryV1,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_amount: Option<f64>,
    pub cost_currency: Option<String>,
    pub usage_coverage: CoverageStateV1,
    pub usage_unavailable_reason: Option<String>,
}

impl ProviderReliabilityObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_ref(&self.attempt_ref)?;
        validate_ref(&self.backend)?;
        validate_ref(&self.protocol)?;
        if self
            .model
            .as_deref()
            .is_some_and(|value| validate_ref(value).is_err())
        {
            return Err("model");
        }
        if self
            .cost_amount
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            return Err("cost_amount");
        }
        let usage_complete = self.input_tokens.is_some()
            && self.output_tokens.is_some()
            && self.cost_amount.is_some()
            && self.cost_currency.is_some();
        let usage_absent = self.input_tokens.is_none()
            && self.output_tokens.is_none()
            && self.cost_amount.is_none()
            && self.cost_currency.is_none();
        match self.usage_coverage {
            CoverageStateV1::Known if usage_complete && self.usage_unavailable_reason.is_none() => {
            }
            CoverageStateV1::Unknown | CoverageStateV1::Capped
                if usage_absent && self.usage_unavailable_reason.is_some() => {}
            _ => return Err("usage_coverage"),
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteOperationV1 {
    Query,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteCoverageObservedV1 {
    pub operation_ref: String,
    pub operation: RemoteOperationV1,
    pub expected_shards: Option<u32>,
    pub observed_shards: Option<u32>,
    pub pending_local_evidence: Option<u32>,
    pub terminal_succeeded: ObservedTernaryV1,
    pub coverage: CoverageStateV1,
    pub unavailable_reason: Option<String>,
}

impl RemoteCoverageObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_ref(&self.operation_ref)?;
        if self
            .expected_shards
            .zip(self.observed_shards)
            .is_some_and(|(expected, observed)| observed > expected)
        {
            return Err("shard_coverage");
        }
        if self.coverage == CoverageStateV1::Known && self.unavailable_reason.is_some() {
            return Err("coverage_reason");
        }
        if self.coverage != CoverageStateV1::Known && self.unavailable_reason.is_none() {
            return Err("coverage_reason");
        }
        Ok(())
    }
}

/// Transport that rejected a surface argument. Unknown preserves missing
/// attribution instead of inventing cli/mcp/http.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectedArgumentSurfaceV1 {
    Cli,
    Mcp,
    Http,
    Unknown,
}

/// Normalized rejected-argument name. Raw flags, values, and tokens never
/// enter this vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectedArgumentNameV1 {
    RequestBody,
    Pagination,
    RequestHandle,
    Operation,
    Lifecycle,
    Unknown,
}

/// Closed error class for a rejected surface argument.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectedArgumentErrorClassV1 {
    Missing,
    InvalidShape,
    OutOfBounds,
    Unsupported,
    Unauthorized,
    Stale,
    Unknown,
}

/// Canonical dispatcher rejection observation. Counts only; no raw argument
/// values, error text, or reversible tokens.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct RejectedArgumentObservedV1 {
    pub surface: RejectedArgumentSurfaceV1,
    pub operation: String,
    pub argument: RejectedArgumentNameV1,
    pub error_class: RejectedArgumentErrorClassV1,
    pub schema_revision: u16,
}

impl RejectedArgumentObservedV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_ref(&self.operation)?;
        if self.schema_revision == 0 {
            return Err("schema_revision");
        }
        Ok(())
    }
}

fn validate_ref(value: &str) -> Result<(), &'static str> {
    if crate::canonical_text::is_canonical_text_within(
        value,
        crate::canonical_text::CANONICAL_TEXT_MAX_BYTES,
    ) {
        Ok(())
    } else {
        Err("identifier")
    }
}
