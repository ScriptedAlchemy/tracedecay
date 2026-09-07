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
    BudgetExceeded { resource: &'static str },
    #[error("temporal persisted state requires an explicit reset: {resource}")]
    ResetRequired { resource: &'static str },
    #[error("temporal read failed during {operation}: {message}")]
    Read {
        operation: &'static str,
        message: String,
    },
}

#[cfg(test)]
mod tests;
