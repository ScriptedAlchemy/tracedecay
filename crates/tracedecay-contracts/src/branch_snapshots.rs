//! Exact local branch-ref snapshot values for generation-bound reads.
//!
//! The native `gix` reads that produce these live in
//! `tracedecay_query::native_git`; this module carries only the request
//! control, results, and typed errors.

#[derive(Clone, Debug)]
pub struct LocalBranchReadControlV1 {
    pub max_refs: usize,
    pub after: Option<String>,
    pub deadline: Option<crate::Deadline>,
    pub cancellation: Option<crate::CancellationSignal>,
}

impl LocalBranchReadControlV1 {
    pub fn termination(&self) -> Option<LocalBranchSnapshotErrorV1> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(crate::CancellationSignal::is_cancelled)
        {
            return Some(LocalBranchSnapshotErrorV1::Cancelled);
        }
        self.deadline
            .as_ref()
            .is_some_and(|deadline| deadline.is_elapsed_at(crate::clock::now_micros()))
            .then_some(LocalBranchSnapshotErrorV1::TimedOut)
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum LocalBranchSnapshotErrorV1 {
    #[error("branch name is not a valid local reference: {branch}")]
    InvalidReference { branch: String },
    #[error("local branch was not found: {branch}")]
    NotFound { branch: String },
    #[error("Git repository is unavailable")]
    RepositoryUnavailable,
    #[error("local branch reference is unavailable: {branch}")]
    ReferenceUnavailable { branch: String },
    #[error("local branch enumeration is unavailable")]
    EnumerationUnavailable,
    #[error("local branch enumeration requires a positive reference limit")]
    InvalidLimit,
    #[error("local branch enumeration exceeded its bounded capacity after {examined} refs")]
    CapacityExceeded { examined: usize },
    #[error("local branch read was cancelled")]
    Cancelled,
    #[error("local branch read timed out")]
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchSnapshot {
    pub name: String,
    pub commit: String,
    pub tree: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalBranchRevisionV1 {
    pub commit: tracedecay_domain::GitOidV1,
    pub tree: tracedecay_domain::GitOidV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalBranchSnapshotsV1 {
    pub snapshots: Vec<BranchSnapshot>,
    pub examined: usize,
    pub truncated: bool,
    pub next_after: Option<String>,
}
