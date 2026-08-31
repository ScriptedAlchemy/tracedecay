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

#[hotpath::measure_all]
impl GraphBudgetKind {
    #[must_use]
    #[hotpath::skip]
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

/// Structured identity of one graph-conflict verdict: the guard site that
/// refused, and the evidence it compared when the site has any. Rendered into
/// the error display so every operator log line names the failing check
/// instead of an undiagnosable bare "graph database conflict" (issue #765).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphConflictContextV1 {
    /// Static, greppable name of the refusing guard site.
    pub site: &'static str,
    /// The state the guard required (head/sequence/digest rendering).
    pub expected: Option<String>,
    /// The state the guard observed.
    pub actual: Option<String>,
}

impl fmt::Display for GraphConflictContextV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "at `{}`", self.site)?;
        match (&self.expected, &self.actual) {
            (Some(expected), Some(actual)) => {
                write!(formatter, " (expected {expected}, actual {actual})")
            }
            (Some(expected), None) => write!(formatter, " (expected {expected})"),
            (None, Some(actual)) => write!(formatter, " (actual {actual})"),
            (None, None) => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GraphDbError {
    #[error("operation cancelled")]
    Cancelled,
    #[error("invalid graph database request: {message}")]
    InvalidRequest { message: String },
    #[error("graph database conflict {context}")]
    Conflict { context: GraphConflictContextV1 },
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
    /// A write reached a generation that is sealed into an immutable
    /// compacted store. Sealed rows accept exact idempotent replays only;
    /// anything else is refused with this typed error rather than a generic
    /// conflict, because no retry can ever make the write admissible.
    #[error("sealed graph generation store is immutable: {message}")]
    SealedStoreImmutable { message: String },
    #[error("graph database unavailable: {message}")]
    Unavailable { message: String },
    #[error("graph database durability is uncertain: {message}")]
    DurabilityUncertain { message: String },
    #[error("graph database is closed")]
    Closed,
}

#[hotpath::measure_all]
impl GraphDbError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
        }
    }

    /// A conflict verdict from `site` with no compared evidence.
    #[must_use]
    #[hotpath::skip]
    pub const fn conflict(site: &'static str) -> Self {
        Self::Conflict {
            context: GraphConflictContextV1 {
                site,
                expected: None,
                actual: None,
            },
        }
    }

    /// A conflict verdict from `site` carrying the compared evidence: what
    /// the guard required and what it observed.
    #[must_use]
    pub fn conflict_observed(
        site: &'static str,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        Self::Conflict {
            context: GraphConflictContextV1 {
                site,
                expected: Some(expected.into()),
                actual: Some(actual.into()),
            },
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::Unavailable {
            message: message.into(),
        }
    }

    #[must_use]
    #[hotpath::skip]
    pub const fn budget_exhausted(kind: GraphBudgetKind, limit: u64) -> Self {
        Self::BudgetExhausted { kind, limit }
    }

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
    fn conflict_renders_site_and_compared_evidence() {
        assert_eq!(
            GraphDbError::conflict("publication.expected_prior_head").to_string(),
            "graph database conflict at `publication.expected_prior_head`"
        );
        let observed = GraphDbError::conflict_observed(
            "publication.expected_prior_head",
            "head seq 3",
            "head seq 5",
        );
        assert_eq!(
            observed.to_string(),
            "graph database conflict at `publication.expected_prior_head` \
             (expected head seq 3, actual head seq 5)"
        );
        let GraphDbError::Conflict { context } = observed else {
            panic!("conflict constructor must produce the conflict variant");
        };
        assert_eq!(context.site, "publication.expected_prior_head");
        assert_eq!(context.expected.as_deref(), Some("head seq 3"));
        assert_eq!(context.actual.as_deref(), Some("head seq 5"));
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
