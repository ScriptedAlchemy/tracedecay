mod contracts;
mod cursor_authentication;
mod execution;
mod paging;
mod request;
mod snapshot;

pub use contracts::*;
pub use cursor_authentication::*;
pub use execution::*;
pub use paging::*;
pub use request::*;
pub use snapshot::*;

use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TemporalPortError {
    #[error("{field} is not a canonical binding")]
    InvalidBinding { field: &'static str },
    #[error("temporal execution generation must be non-zero")]
    ZeroGeneration,
    #[error("temporal execution snapshot was not authorized")]
    UnauthorizedSnapshot,
    #[error("temporal execution participant manifest must not be empty")]
    EmptyParticipantManifest,
    #[error("temporal execution participant manifest contains a duplicate source")]
    DuplicateParticipant,
    #[error("temporal execution participant manifest has {observed} entries; maximum is {maximum}")]
    ParticipantLimitExceeded { observed: usize, maximum: usize },
    #[error(
        "temporal execution participant manifest has {observed} canonical bytes; maximum is {maximum}"
    )]
    ParticipantManifestBytesExceeded { observed: usize, maximum: usize },
    #[error("temporal kernel {field} version must be non-zero")]
    ZeroVersion { field: &'static str },
    #[error("temporal execution was cancelled")]
    Cancelled,
    #[error("temporal execution deadline elapsed")]
    DeadlineExceeded,
    #[error("temporal execution exceeded its {resource} budget")]
    BudgetExceeded {
        resource: &'static str,
        /// Present when the refusing boundary keeps a counter. A request-shape
        /// check — a field cap, a parameter ceiling — has none, and reports
        /// `None` rather than inventing numbers.
        accounting: Option<ReadBudgetAccounting>,
    },
    #[error("temporal persisted state requires an explicit reset: {resource}")]
    ResetRequired { resource: &'static str },
    #[error("temporal read failed during {operation}: {message}")]
    Read {
        operation: &'static str,
        message: String,
    },
}

/// What a bounded budget had counted when it refused.
///
/// Both variants are exact. A bounded read never counts the rows it declined to
/// read, so an exhausted read reports what it consumed and that storage held
/// more — never a total it would have to run the refused scan to learn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetObservation {
    /// The request asked for more units than the admitted maximum.
    Requested(u64),
    /// The read consumed its whole budget and storage still held more.
    ConsumedWithMoreAvailable(u64),
}

/// A bounded read boundary's accounting at the moment it refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadBudgetAccounting {
    pub limit: u64,
    pub observed: BudgetObservation,
}

impl ReadBudgetAccounting {
    #[must_use]
    pub const fn requested(limit: u64, requested: u64) -> Self {
        Self {
            limit,
            observed: BudgetObservation::Requested(requested),
        }
    }

    #[must_use]
    pub const fn consumed_with_more(limit: u64, consumed: u64) -> Self {
        Self {
            limit,
            observed: BudgetObservation::ConsumedWithMoreAvailable(consumed),
        }
    }
}

#[cfg(test)]
mod tests;
