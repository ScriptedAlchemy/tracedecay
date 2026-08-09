use serde::{Deserialize, Serialize};

use super::{EffectReceipt, EffectTermination};
use crate::error::ApplicationContractError;

/// Safe adapter-independent retry instruction. Adapters preserve it verbatim.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetryDirective {
    Never,
    SameRequest,
    AfterDelay,
    AfterRevalidate,
    AfterReconcile,
}

/// Request identity boundary within which a retry remains valid.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetryScope {
    SameRequest,
    SameOperation,
    FreshRequest,
}

/// Layer that owns resolving the problem rather than merely presenting it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProblemOwningLayer {
    Adapter,
    Application,
    Runtime,
    Port,
}

/// Whether the problem occurred before admission or is an admitted terminal.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProblemTerminality {
    PreAdmission,
    AdmittedTerminal,
}

/// Bounded action an adapter may offer without inferring executable authority.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationProblemKind {
    InvalidRequest,
    NotFoundOrNotAuthorized,
    Conflict,
    PartialEffect,
    Stale,
    Unsupported,
    Unavailable,
    ResetRequired,
    Saturated,
    Cancelled,
    TimedOut,
}

impl ApplicationProblemKind {
    /// Whether this problem was produced after the application admitted the
    /// operation.  These states must remain terminal receipts at every
    /// adapter boundary; they are never safe to reinterpret as availability.
    pub const fn terminality(self) -> ProblemTerminality {
        match self {
            Self::PartialEffect | Self::ResetRequired => ProblemTerminality::AdmittedTerminal,
            Self::InvalidRequest
            | Self::NotFoundOrNotAuthorized
            | Self::Conflict
            | Self::Stale
            | Self::Unsupported
            | Self::Unavailable
            | Self::Saturated
            | Self::Cancelled
            | Self::TimedOut => ProblemTerminality::PreAdmission,
        }
    }

    pub const fn is_admitted_terminal(self) -> bool {
        matches!(self, Self::PartialEffect | Self::ResetRequired)
    }
}

/// Application failure or admitted terminal. Resource-addressed denial
/// intentionally shares one shape with absence and hidden policy outcomes.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
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
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    TimedOut {
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
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
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
    TimedOut {
        retry: RetryDirective,
        legal_actions: Vec<LegalAction>,
    },
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
                diagnostic,
                retry,
                legal_actions,
            } => Self::Unavailable {
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
                retry,
                legal_actions,
            } => Self::Cancelled {
                retry,
                legal_actions,
            },
            ApplicationProblemWire::TimedOut {
                retry,
                legal_actions,
            } => Self::TimedOut {
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
            Self::InvalidRequest { .. }
            | Self::NotFoundOrNotAuthorized { .. }
            | Self::Conflict { .. }
            | Self::Stale { .. }
            | Self::Unsupported { .. }
            | Self::Unavailable { .. }
            | Self::Saturated { .. }
            | Self::Cancelled { .. }
            | Self::TimedOut { .. } => {}
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
            Self::ResetRequired { .. } => ApplicationProblemKind::ResetRequired,
            Self::Saturated { .. } => ApplicationProblemKind::Saturated,
            Self::Cancelled { .. } => ApplicationProblemKind::Cancelled,
            Self::TimedOut { .. } => ApplicationProblemKind::TimedOut,
        }
    }

    pub const fn terminality(&self) -> ProblemTerminality {
        self.kind().terminality()
    }

    pub const fn is_admitted_terminal(&self) -> bool {
        self.kind().is_admitted_terminal()
    }

    pub fn not_found_or_not_authorized(retry: RetryDirective) -> Self {
        Self::NotFoundOrNotAuthorized {
            retry,
            legal_actions: Vec::new(),
        }
    }

    pub fn cancelled_before_admission() -> Self {
        Self::Cancelled {
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        }
    }

    pub fn timed_out_before_admission() -> Self {
        Self::TimedOut {
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        }
    }

    pub fn unavailable(diagnostic: SafeDiagnostic) -> Self {
        Self::Unavailable {
            diagnostic,
            retry: RetryDirective::AfterDelay,
            legal_actions: vec![LegalAction::Retry],
        }
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
                Self::Cancelled { .. } => "The request was cancelled before admission",
                Self::TimedOut { .. } => "The request timed out before admission",
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
mod tests {
    use super::{
        ApplicationProblem, ApplicationProblemKind, LegalAction, ProblemTerminality,
        RetryDirective, SafeDiagnostic,
    };

    #[test]
    fn reset_required_is_a_distinct_non_retryable_terminal() {
        let problem = ApplicationProblem::reset_required(
            SafeDiagnostic::new("store.reset_required", "The store must be reset.")
                .expect("fixture diagnostic is valid"),
        );

        assert_eq!(problem.kind(), ApplicationProblemKind::ResetRequired);
        assert_eq!(problem.canonical_code(), "reset_required");
        assert_eq!(problem.retry(), RetryDirective::Never);
        assert_eq!(problem.legal_actions(), &[LegalAction::Reset]);
        assert_eq!(problem.terminality(), ProblemTerminality::AdmittedTerminal);
        assert!(problem.is_admitted_terminal());
        assert!(problem.committed_receipt().is_none());

        let wire = serde_json::to_value(&problem).expect("problem serializes");
        assert_eq!(wire["kind"], "reset_required");
        assert_eq!(wire["retry"], "never");
        assert_eq!(wire["legal_actions"], serde_json::json!(["reset"]));

        let mut unknown = wire.clone();
        unknown["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ApplicationProblem>(unknown).is_err());

        let mut retrying = wire.clone();
        retrying["retry"] = serde_json::json!("after_delay");
        assert!(serde_json::from_value::<ApplicationProblem>(retrying).is_err());

        let mut wrong_action = wire;
        wrong_action["legal_actions"] = serde_json::json!(["retry"]);
        assert!(serde_json::from_value::<ApplicationProblem>(wrong_action).is_err());
    }
}
