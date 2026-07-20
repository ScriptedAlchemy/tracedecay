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

    pub const fn is_pre_admission(&self) -> bool {
        matches!(self, Self::Cancelled { .. } | Self::TimedOut { .. })
    }
}
