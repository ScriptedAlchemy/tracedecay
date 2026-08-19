use std::fmt;

use thiserror::Error;

/// Named graph-operation budget that was exhausted.
///
/// `GraphDbError::BudgetExhausted` carries this identity plus the numeric
/// limit so callers can name the actual ceiling instead of collapsing every
/// class to a generic "read budget".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphBudgetKind {
    Read,
    Write,
    Capacity,
    Mutation,
}

impl GraphBudgetKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Capacity => "capacity",
            Self::Mutation => "mutation",
        }
    }

    /// Parses a budget name produced by [`Self::as_str`]. Returns `None` for
    /// projection-local budget names that have no graph-db equivalent.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "capacity" => Some(Self::Capacity),
            "mutation" => Some(Self::Mutation),
            _ => None,
        }
    }
}

impl fmt::Display for GraphBudgetKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GraphDbError {
    #[error("operation cancelled")]
    Cancelled,
    #[error("invalid graph database request: {message}")]
    InvalidRequest { message: String },
    #[error("graph database conflict")]
    Conflict,
    #[error("graph {kind} budget exhausted (limit {limit})")]
    BudgetExhausted { kind: GraphBudgetKind, limit: u64 },
    #[error("graph operation deadline exceeded")]
    DeadlineExceeded,
    #[error(
        "graph projection `{namespace}/{projection}` is quarantined after recovery mismatch: {message}"
    )]
    ProjectionMismatch {
        namespace: String,
        projection: String,
        message: String,
    },
    #[error(
        "graph generation `{namespace}/{projection}/{generation}` is quarantined after recovery mismatch: {message}"
    )]
    GenerationMismatch {
        namespace: String,
        projection: String,
        generation: String,
        message: String,
    },
    #[error("graph database reset required: {message}")]
    ResetRequired { message: String },
    #[error("graph database is corrupt: {message}")]
    Corrupt { message: String },
    #[error("graph database unavailable: {message}")]
    Unavailable { message: String },
    #[error("graph database durability is uncertain: {message}")]
    DurabilityUncertain { message: String },
    #[error("graph database is closed")]
    Closed,
}

impl GraphDbError {
    /// Constructs a typed contract-validation failure.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
        }
    }

    /// Constructs a typed authority or infrastructure availability failure.
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::Unavailable {
            message: message.into(),
        }
    }

    /// Constructs a typed budget-exhaustion failure that names the ceiling.
    #[must_use]
    pub const fn budget_exhausted(kind: GraphBudgetKind, limit: u64) -> Self {
        Self::BudgetExhausted { kind, limit }
    }

    /// Constructs a typed budget-exhaustion failure from a `usize` ceiling.
    #[must_use]
    pub fn budget_exhausted_count(kind: GraphBudgetKind, limit: usize) -> Self {
        Self::budget_exhausted(kind, u64::try_from(limit).unwrap_or(u64::MAX))
    }
}

pub(crate) fn rollback_failure(
    context: &str,
    primary: impl std::fmt::Display,
    rollback: impl std::fmt::Display,
) -> GraphDbError {
    GraphDbError::DurabilityUncertain {
        message: format!("{context} failure `{primary}` followed by rollback failure: {rollback}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{GraphBudgetKind, GraphDbError, rollback_failure};

    #[test]
    fn budget_exhausted_names_kind_and_limit() {
        let error = GraphDbError::budget_exhausted(GraphBudgetKind::Mutation, 4_096);
        assert_eq!(
            error.to_string(),
            "graph mutation budget exhausted (limit 4096)"
        );
        assert_eq!(
            GraphDbError::budget_exhausted_count(GraphBudgetKind::Write, 4 * 1024 * 1024)
                .to_string(),
            "graph write budget exhausted (limit 4194304)"
        );
    }

    #[test]
    fn budget_kind_from_name_round_trips_and_rejects_unnamed() {
        assert_eq!(
            GraphBudgetKind::from_name("read"),
            Some(GraphBudgetKind::Read)
        );
        assert_eq!(
            GraphBudgetKind::from_name("write"),
            Some(GraphBudgetKind::Write)
        );
        assert_eq!(
            GraphBudgetKind::from_name("capacity"),
            Some(GraphBudgetKind::Capacity)
        );
        assert_eq!(
            GraphBudgetKind::from_name("mutation"),
            Some(GraphBudgetKind::Mutation)
        );
        assert_eq!(GraphBudgetKind::from_name(""), None);
        assert_eq!(GraphBudgetKind::from_name("unnamed"), None);
    }

    #[test]
    fn rollback_failure_preserves_both_errors_and_context() {
        assert_eq!(
            rollback_failure("format initialization", "create failed", "rollback failed"),
            GraphDbError::DurabilityUncertain {
                message: "format initialization failure `create failed` followed by rollback failure: rollback failed"
                    .to_owned(),
            }
        );
    }
}
