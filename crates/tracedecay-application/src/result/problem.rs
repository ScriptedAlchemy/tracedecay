use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{CancellationStage, EffectReceipt, EffectTermination};
use crate::error::ApplicationContractError;

/// Safe adapter-independent retry instruction. Adapters preserve it verbatim.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum RetryDirective {
    Never,
    SameRequest,
    AfterDelay,
    AfterRevalidate,
    AfterReconcile,
}

/// Request identity boundary within which a retry remains valid.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum RetryScope {
    SameRequest,
    SameOperation,
    FreshRequest,
}

/// Layer that owns resolving the problem rather than merely presenting it.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ProblemOwningLayer {
    Adapter,
    Application,
    Runtime,
    Port,
}

/// Whether the problem occurred before admission or is an admitted terminal.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ProblemTerminality {
    PreAdmission,
    AdmittedTerminal,
}

/// Bounded action an adapter may offer without inferring executable authority.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum LegalAction {
    CorrectRequest,
    Reauthorize,
    Refresh,
    Retry,
    Reconcile,
    Reset,
    ContactAdministrator,
}

/// Sanitized detail that may cross the application boundary.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SafeDiagnostic {
    pub code: String,
    pub message: String,
}

impl SafeDiagnostic {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, ApplicationContractError> {
        let diagnostic = Self {
            code: code.into(),
            message: message.into(),
        };
        diagnostic.validate()?;
        Ok(diagnostic)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        for (field, value, limit) in [
            ("safe diagnostic code", self.code.as_str(), 128_usize),
            ("safe diagnostic message", self.message.as_str(), 512_usize),
        ] {
            if value.is_empty()
                || value.trim() != value
                || value.len() > limit
                || value.chars().any(char::is_control)
            {
                return Err(ApplicationContractError::InvalidIdentifier { field });
            }
        }
        Ok(())
    }
}

/// Stable problem-code taxonomy for request failures and admitted terminals.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationProblemKind {
    InvalidRequest,
    NotFoundOrNotAuthorized,
    Conflict,
    PartialEffect,
    Stale,
    Unsupported,
    Unavailable,
    ExecutionFailed,
    ResetRequired,
    Saturated,
    Cancelled,
    TimedOut,
}

/// Stable reason an application authority is unavailable.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationUnavailableClassV1 {
    Authority,
    BackendUnavailable,
    BackendDisconnected,
    BackendRetryable,
}

/// Stable non-retryable class for an admitted execution failure.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationExecutionFailureClassV1 {
    Denied,
    MalformedOutput,
    Permanent,
}

/// Application failure or admitted terminal. Resource-addressed denial
/// intentionally shares one shape with absence and hidden policy outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplicationProblem {
    InvalidRequest {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    NotFoundOrNotAuthorized {
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Conflict {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    /// The primary effect committed, but a required post-commit step failed.
    /// The canonical receipt prevents callers from blindly replaying it.
    PartialEffect {
        diagnostic: SafeDiagnostic,
        committed_receipt: EffectReceipt,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Stale {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Unsupported {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Unavailable {
        classification: ApplicationUnavailableClassV1,
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    ExecutionFailed {
        classification: ApplicationExecutionFailureClassV1,
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    ResetRequired {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Saturated {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Cancelled {
        stage: CancellationStage,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    TimedOut {
        stage: CancellationStage,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
enum ApplicationProblemWire {
    InvalidRequest {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    NotFoundOrNotAuthorized {
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Conflict {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    PartialEffect {
        diagnostic: SafeDiagnostic,
        committed_receipt: EffectReceipt,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Stale {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Unsupported {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Unavailable {
        classification: ApplicationUnavailableClassV1,
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    ExecutionFailed {
        classification: ApplicationExecutionFailureClassV1,
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    ResetRequired {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Saturated {
        diagnostic: SafeDiagnostic,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    Cancelled {
        stage: CancellationStage,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    TimedOut {
        stage: CancellationStage,
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
}

impl From<ApplicationProblem> for ApplicationProblemWire {
    fn from(problem: ApplicationProblem) -> Self {
        match problem {
            ApplicationProblem::InvalidRequest {
                diagnostic,
                retry,
                legal_actions,
            } => Self::InvalidRequest {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblem::NotFoundOrNotAuthorized {
                retry,
                legal_actions,
            } => Self::NotFoundOrNotAuthorized {
                retry,
                legal_actions,
            },
            ApplicationProblem::Conflict {
                diagnostic,
                retry,
                legal_actions,
            } => Self::Conflict {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblem::PartialEffect {
                diagnostic,
                committed_receipt,
                retry,
                legal_actions,
            } => Self::PartialEffect {
                diagnostic,
                committed_receipt,
                retry,
                legal_actions,
            },
            ApplicationProblem::Stale {
                diagnostic,
                retry,
                legal_actions,
            } => Self::Stale {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblem::Unsupported {
                diagnostic,
                retry,
                legal_actions,
            } => Self::Unsupported {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblem::Unavailable {
                classification,
                diagnostic,
                retry,
                legal_actions,
            } => Self::Unavailable {
                classification,
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblem::ExecutionFailed {
                classification,
                diagnostic,
                retry,
                legal_actions,
            } => Self::ExecutionFailed {
                classification,
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblem::ResetRequired {
                diagnostic,
                retry,
                legal_actions,
            } => Self::ResetRequired {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblem::Saturated {
                diagnostic,
                retry,
                legal_actions,
            } => Self::Saturated {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblem::Cancelled {
                stage,
                retry,
                legal_actions,
            } => Self::Cancelled {
                stage,
                retry,
                legal_actions,
            },
            ApplicationProblem::TimedOut {
                stage,
                retry,
                legal_actions,
            } => Self::TimedOut {
                stage,
                retry,
                legal_actions,
            },
        }
    }
}

impl Serialize for ApplicationProblem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        ApplicationProblemWire::from(self.clone()).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ApplicationProblem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ApplicationProblemWire::deserialize(deserializer)?;
        Self::from_wire(wire).map_err(serde::de::Error::custom)
    }
}

impl ApplicationProblem {
    fn from_wire(wire: ApplicationProblemWire) -> Result<Self, ApplicationContractError> {
        let problem = match wire {
            ApplicationProblemWire::InvalidRequest {
                diagnostic,
                retry,
                legal_actions,
            } => Self::InvalidRequest {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblemWire::NotFoundOrNotAuthorized {
                retry,
                legal_actions,
            } => Self::NotFoundOrNotAuthorized {
                retry,
                legal_actions,
            },
            ApplicationProblemWire::Conflict {
                diagnostic,
                retry,
                legal_actions,
            } => Self::Conflict {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblemWire::PartialEffect {
                diagnostic,
                committed_receipt,
                retry,
                legal_actions,
            } => Self::PartialEffect {
                diagnostic,
                committed_receipt,
                retry,
                legal_actions,
            },
            ApplicationProblemWire::Stale {
                diagnostic,
                retry,
                legal_actions,
            } => Self::Stale {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblemWire::Unsupported {
                diagnostic,
                retry,
                legal_actions,
            } => Self::Unsupported {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblemWire::Unavailable {
                classification,
                diagnostic,
                retry,
                legal_actions,
            } => Self::Unavailable {
                classification,
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblemWire::ExecutionFailed {
                classification,
                diagnostic,
                retry,
                legal_actions,
            } => Self::ExecutionFailed {
                classification,
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblemWire::ResetRequired {
                diagnostic,
                retry,
                legal_actions,
            } => Self::ResetRequired {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblemWire::Saturated {
                diagnostic,
                retry,
                legal_actions,
            } => Self::Saturated {
                diagnostic,
                retry,
                legal_actions,
            },
            ApplicationProblemWire::Cancelled {
                stage,
                retry,
                legal_actions,
            } => Self::Cancelled {
                stage,
                retry,
                legal_actions,
            },
            ApplicationProblemWire::TimedOut {
                stage,
                retry,
                legal_actions,
            } => Self::TimedOut {
                stage,
                retry,
                legal_actions,
            },
        };
        problem.validate()?;
        Ok(problem)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if let Some(diagnostic) = self.diagnostic() {
            diagnostic.validate()?;
        }

        match self {
            Self::PartialEffect {
                committed_receipt,
                retry,
                legal_actions,
                ..
            } => {
                if *retry != RetryDirective::Never
                    || legal_actions.as_slice() != [LegalAction::Reconcile]
                    || committed_receipt.outcome != EffectTermination::Partial
                    || (committed_receipt.committed_state.is_none()
                        && committed_receipt.external_proof.is_none())
                {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "partial effect terminal",
                    });
                }
                committed_receipt.validate()?;
            }
            Self::ResetRequired {
                retry,
                legal_actions,
                ..
            } => {
                if *retry != RetryDirective::Never
                    || legal_actions.as_slice() != [LegalAction::Reset]
                {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "reset-required terminal",
                    });
                }
            }
            Self::Unavailable {
                classification,
                retry,
                legal_actions,
                ..
            } if !matches!(classification, ApplicationUnavailableClassV1::Authority) => {
                if *retry != RetryDirective::AfterRevalidate
                    || legal_actions.as_slice() != [LegalAction::Retry]
                {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "admitted unavailable terminal",
                    });
                }
            }
            Self::ExecutionFailed {
                retry,
                legal_actions,
                ..
            } => {
                if *retry != RetryDirective::Never
                    || legal_actions.as_slice() != [LegalAction::ContactAdministrator]
                {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "execution-failed terminal",
                    });
                }
            }
            Self::Cancelled { stage, .. } | Self::TimedOut { stage, .. } => {
                if matches!(
                    stage,
                    CancellationStage::Reconciling | CancellationStage::AfterCommit
                ) {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "application problem cancellation stage",
                    });
                }
            }
            Self::InvalidRequest { .. }
            | Self::NotFoundOrNotAuthorized { .. }
            | Self::Conflict { .. }
            | Self::Stale { .. }
            | Self::Unsupported { .. }
            | Self::Unavailable { .. }
            | Self::Saturated { .. } => {}
        }
        Ok(())
    }

    pub const fn kind(&self) -> ApplicationProblemKind {
        match self {
            Self::InvalidRequest { .. } => ApplicationProblemKind::InvalidRequest,
            Self::NotFoundOrNotAuthorized { .. } => ApplicationProblemKind::NotFoundOrNotAuthorized,
            Self::Conflict { .. } => ApplicationProblemKind::Conflict,
            Self::PartialEffect { .. } => ApplicationProblemKind::PartialEffect,
            Self::Stale { .. } => ApplicationProblemKind::Stale,
            Self::Unsupported { .. } => ApplicationProblemKind::Unsupported,
            Self::Unavailable { .. } => ApplicationProblemKind::Unavailable,
            Self::ExecutionFailed { .. } => ApplicationProblemKind::ExecutionFailed,
            Self::ResetRequired { .. } => ApplicationProblemKind::ResetRequired,
            Self::Saturated { .. } => ApplicationProblemKind::Saturated,
            Self::Cancelled { .. } => ApplicationProblemKind::Cancelled,
            Self::TimedOut { .. } => ApplicationProblemKind::TimedOut,
        }
    }

    pub const fn terminality(&self) -> ProblemTerminality {
        match self {
            Self::PartialEffect { .. }
            | Self::ResetRequired { .. }
            | Self::ExecutionFailed { .. } => ProblemTerminality::AdmittedTerminal,
            Self::Unavailable { classification, .. }
                if !matches!(classification, ApplicationUnavailableClassV1::Authority) =>
            {
                ProblemTerminality::AdmittedTerminal
            }
            Self::Cancelled { stage, .. } | Self::TimedOut { stage, .. }
                if !matches!(stage, CancellationStage::BeforeAdmission) =>
            {
                ProblemTerminality::AdmittedTerminal
            }
            _ => ProblemTerminality::PreAdmission,
        }
    }

    pub const fn is_admitted_terminal(&self) -> bool {
        matches!(self.terminality(), ProblemTerminality::AdmittedTerminal)
    }

    pub fn not_found_or_not_authorized(retry: RetryDirective) -> Self {
        Self::NotFoundOrNotAuthorized {
            retry,
            legal_actions: Vec::new(),
        }
    }

    pub fn cancelled_before_admission() -> Self {
        Self::Cancelled {
            stage: CancellationStage::BeforeAdmission,
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        }
    }

    pub fn timed_out_before_admission() -> Self {
        Self::TimedOut {
            stage: CancellationStage::BeforeAdmission,
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        }
    }

    pub fn cancelled(stage: CancellationStage) -> Result<Self, ApplicationContractError> {
        let problem = Self::Cancelled {
            stage,
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        };
        problem.validate()?;
        Ok(problem)
    }

    pub fn timed_out(stage: CancellationStage) -> Result<Self, ApplicationContractError> {
        let problem = Self::TimedOut {
            stage,
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        };
        problem.validate()?;
        Ok(problem)
    }

    pub const fn cancellation_stage(&self) -> Option<CancellationStage> {
        match self {
            Self::Cancelled { stage, .. } | Self::TimedOut { stage, .. } => Some(*stage),
            _ => None,
        }
    }

    pub const fn unavailable_classification(&self) -> Option<ApplicationUnavailableClassV1> {
        match self {
            Self::Unavailable { classification, .. } => Some(*classification),
            _ => None,
        }
    }

    pub const fn execution_failure_classification(
        &self,
    ) -> Option<ApplicationExecutionFailureClassV1> {
        match self {
            Self::ExecutionFailed { classification, .. } => Some(*classification),
            _ => None,
        }
    }

    pub fn unavailable(diagnostic: SafeDiagnostic) -> Self {
        Self::Unavailable {
            classification: ApplicationUnavailableClassV1::Authority,
            diagnostic,
            retry: RetryDirective::AfterDelay,
            legal_actions: vec![LegalAction::Retry],
        }
    }

    pub fn admitted_unavailable(
        classification: ApplicationUnavailableClassV1,
        diagnostic: SafeDiagnostic,
    ) -> Result<Self, ApplicationContractError> {
        if matches!(classification, ApplicationUnavailableClassV1::Authority) {
            return Err(ApplicationContractError::Inconsistent {
                field: "admitted unavailable classification",
            });
        }
        let problem = Self::Unavailable {
            classification,
            diagnostic,
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Retry],
        };
        problem.validate()?;
        Ok(problem)
    }

    pub fn execution_failed(
        classification: ApplicationExecutionFailureClassV1,
        diagnostic: SafeDiagnostic,
    ) -> Result<Self, ApplicationContractError> {
        let problem = Self::ExecutionFailed {
            classification,
            diagnostic,
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::ContactAdministrator],
        };
        problem.validate()?;
        Ok(problem)
    }

    pub fn stale(diagnostic: SafeDiagnostic) -> Self {
        Self::Stale {
            diagnostic,
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        }
    }

    pub fn reset_required(diagnostic: SafeDiagnostic) -> Self {
        Self::ResetRequired {
            diagnostic,
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::Reset],
        }
    }

    pub const fn retry(&self) -> RetryDirective {
        match self {
            Self::InvalidRequest { retry, .. }
            | Self::NotFoundOrNotAuthorized { retry, .. }
            | Self::Conflict { retry, .. }
            | Self::PartialEffect { retry, .. }
            | Self::Stale { retry, .. }
            | Self::Unsupported { retry, .. }
            | Self::Unavailable { retry, .. }
            | Self::ExecutionFailed { retry, .. }
            | Self::ResetRequired { retry, .. }
            | Self::Saturated { retry, .. }
            | Self::Cancelled { retry, .. }
            | Self::TimedOut { retry, .. } => *retry,
        }
    }

    pub fn legal_actions(&self) -> &[LegalAction] {
        match self {
            Self::InvalidRequest { legal_actions, .. }
            | Self::NotFoundOrNotAuthorized { legal_actions, .. }
            | Self::Conflict { legal_actions, .. }
            | Self::PartialEffect { legal_actions, .. }
            | Self::Stale { legal_actions, .. }
            | Self::Unsupported { legal_actions, .. }
            | Self::Unavailable { legal_actions, .. }
            | Self::ExecutionFailed { legal_actions, .. }
            | Self::ResetRequired { legal_actions, .. }
            | Self::Saturated { legal_actions, .. }
            | Self::Cancelled { legal_actions, .. }
            | Self::TimedOut { legal_actions, .. } => legal_actions,
        }
    }

    pub fn diagnostic(&self) -> Option<&SafeDiagnostic> {
        match self {
            Self::InvalidRequest { diagnostic, .. }
            | Self::Conflict { diagnostic, .. }
            | Self::PartialEffect { diagnostic, .. }
            | Self::Stale { diagnostic, .. }
            | Self::Unsupported { diagnostic, .. }
            | Self::Unavailable { diagnostic, .. }
            | Self::ExecutionFailed { diagnostic, .. }
            | Self::ResetRequired { diagnostic, .. }
            | Self::Saturated { diagnostic, .. } => Some(diagnostic),
            Self::NotFoundOrNotAuthorized { .. }
            | Self::Cancelled { .. }
            | Self::TimedOut { .. } => None,
        }
    }

    pub const fn canonical_code(&self) -> &'static str {
        match self {
            Self::InvalidRequest { .. } => "invalid_request",
            Self::NotFoundOrNotAuthorized { .. } => "not_found_or_not_authorized",
            Self::Conflict { .. } => "conflict",
            Self::PartialEffect { .. } => "partial_effect",
            Self::Stale { .. } => "stale",
            Self::Unsupported { .. } => "unsupported",
            Self::Unavailable { .. } => "unavailable",
            Self::ExecutionFailed { .. } => "execution_failed",
            Self::ResetRequired { .. } => "reset_required",
            Self::Saturated { .. } => "saturated",
            Self::Cancelled { .. } => "cancelled",
            Self::TimedOut { .. } => "timed_out",
        }
    }

    pub fn safe_message(&self) -> &str {
        self.diagnostic()
            .map(|diagnostic| diagnostic.message.as_str())
            .unwrap_or_else(|| match self {
                Self::NotFoundOrNotAuthorized { .. } => {
                    "The requested resource was not found or is not authorized"
                }
                Self::Cancelled { stage, .. } => match stage {
                    CancellationStage::BeforeAdmission => {
                        "The request was cancelled before admission"
                    }
                    _ => "The admitted request was cancelled",
                },
                Self::TimedOut { stage, .. } => match stage {
                    CancellationStage::BeforeAdmission => "The request timed out before admission",
                    _ => "The admitted request timed out",
                },
                _ => unreachable!("diagnostic-bearing problem handled above"),
            })
    }

    pub fn committed_receipt(&self) -> Option<&EffectReceipt> {
        match self {
            Self::PartialEffect {
                committed_receipt, ..
            } => Some(committed_receipt),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "problem/tests.rs"]
mod tests;
