use serde::{Deserialize, Serialize};

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

/// Stable problem-code taxonomy for failures before a request is admitted.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationProblemKind {
    InvalidRequest,
    NotFoundOrNotAuthorized,
    Conflict,
    Stale,
    Unsupported,
    Unavailable,
    Saturated,
    Cancelled,
    TimedOut,
}

/// Pre-admission application failure. Resource-addressed denial intentionally
/// shares one shape with absence and hidden policy outcomes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
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

impl ApplicationProblem {
    pub fn kind(&self) -> ApplicationProblemKind {
        match self {
            Self::InvalidRequest { .. } => ApplicationProblemKind::InvalidRequest,
            Self::NotFoundOrNotAuthorized { .. } => ApplicationProblemKind::NotFoundOrNotAuthorized,
            Self::Conflict { .. } => ApplicationProblemKind::Conflict,
            Self::Stale { .. } => ApplicationProblemKind::Stale,
            Self::Unsupported { .. } => ApplicationProblemKind::Unsupported,
            Self::Unavailable { .. } => ApplicationProblemKind::Unavailable,
            Self::Saturated { .. } => ApplicationProblemKind::Saturated,
            Self::Cancelled { .. } => ApplicationProblemKind::Cancelled,
            Self::TimedOut { .. } => ApplicationProblemKind::TimedOut,
        }
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

    pub const fn retry(&self) -> RetryDirective {
        match self {
            Self::InvalidRequest { retry, .. }
            | Self::NotFoundOrNotAuthorized { retry, .. }
            | Self::Conflict { retry, .. }
            | Self::Stale { retry, .. }
            | Self::Unsupported { retry, .. }
            | Self::Unavailable { retry, .. }
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
            | Self::Stale { legal_actions, .. }
            | Self::Unsupported { legal_actions, .. }
            | Self::Unavailable { legal_actions, .. }
            | Self::Saturated { legal_actions, .. }
            | Self::Cancelled { legal_actions, .. }
            | Self::TimedOut { legal_actions, .. } => legal_actions,
        }
    }

    pub fn diagnostic(&self) -> Option<&SafeDiagnostic> {
        match self {
            Self::InvalidRequest { diagnostic, .. }
            | Self::Conflict { diagnostic, .. }
            | Self::Stale { diagnostic, .. }
            | Self::Unsupported { diagnostic, .. }
            | Self::Unavailable { diagnostic, .. }
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
            Self::Stale { .. } => "stale",
            Self::Unsupported { .. } => "unsupported",
            Self::Unavailable { .. } => "unavailable",
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
}
