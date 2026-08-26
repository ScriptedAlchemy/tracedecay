//! Client-owned pre-flight admission for bursts of GitHub reads.
//!
//! Each CI client retains only the last quota checkpoint it observed. Clones
//! share that checkpoint, while a client opened for another credential starts
//! with no observation. This keeps admission subordinate to the concrete
//! transport owner instead of creating a second process-global authority.

use std::sync::Mutex;

use tracedecay_domain::UtcMicros;
use tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1;

/// Requests held back from one burst so concurrent review or release reads are
/// not starved by a CI scan.
const GITHUB_RATE_LIMIT_RESERVE_V1: u32 = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubRateLimitAdmissionV1 {
    /// The client has not observed a usable checkpoint.
    Unknown,
    Admitted {
        remaining: u32,
    },
    Degraded {
        admitted_requests: u32,
        checkpoint: GitHubReviewRateLimitCheckpointV1,
    },
    Exhausted {
        checkpoint: GitHubReviewRateLimitCheckpointV1,
    },
    /// The client-owned observation could not be read.
    Unavailable,
}

impl GitHubRateLimitAdmissionV1 {
    #[must_use]
    pub fn admitted_requests(&self, planned_requests: u32) -> u32 {
        match self {
            Self::Unknown | Self::Admitted { .. } => planned_requests,
            Self::Degraded {
                admitted_requests, ..
            } => (*admitted_requests).min(planned_requests),
            Self::Exhausted { .. } | Self::Unavailable => 0,
        }
    }

    #[must_use]
    pub fn checkpoint(&self) -> Option<&GitHubReviewRateLimitCheckpointV1> {
        match self {
            Self::Degraded { checkpoint, .. } | Self::Exhausted { checkpoint } => Some(checkpoint),
            Self::Unknown | Self::Admitted { .. } | Self::Unavailable => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GitHubRateLimitRecordOutcomeV1 {
    Recorded,
    Invalid,
    Unavailable,
}

#[derive(Default)]
pub(super) struct GitHubRateLimitTrackerV1 {
    checkpoint: Mutex<Option<GitHubReviewRateLimitCheckpointV1>>,
}

impl GitHubRateLimitTrackerV1 {
    pub(super) fn record(
        &self,
        checkpoint: &GitHubReviewRateLimitCheckpointV1,
    ) -> GitHubRateLimitRecordOutcomeV1 {
        if checkpoint.validate().is_err() {
            return GitHubRateLimitRecordOutcomeV1::Invalid;
        }
        let Ok(mut retained) = self.checkpoint.lock() else {
            return GitHubRateLimitRecordOutcomeV1::Unavailable;
        };
        *retained = Some(checkpoint.clone());
        GitHubRateLimitRecordOutcomeV1::Recorded
    }

    pub(super) fn admit(
        &self,
        planned_requests: u32,
        now: UtcMicros,
    ) -> GitHubRateLimitAdmissionV1 {
        let Ok(mut retained) = self.checkpoint.lock() else {
            return GitHubRateLimitAdmissionV1::Unavailable;
        };
        let Some(checkpoint) = retained.as_ref().cloned() else {
            return GitHubRateLimitAdmissionV1::Unknown;
        };
        if checkpoint.reset_at.0 <= now.0 {
            *retained = None;
            return GitHubRateLimitAdmissionV1::Unknown;
        }
        drop(retained);

        let usable = checkpoint
            .remaining
            .saturating_sub(GITHUB_RATE_LIMIT_RESERVE_V1);
        if usable == 0 {
            return GitHubRateLimitAdmissionV1::Exhausted { checkpoint };
        }
        if usable >= planned_requests {
            return GitHubRateLimitAdmissionV1::Admitted {
                remaining: checkpoint.remaining,
            };
        }
        GitHubRateLimitAdmissionV1::Degraded {
            admitted_requests: usable,
            checkpoint,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn checkpoint(remaining: u32, reset_at: i64) -> GitHubReviewRateLimitCheckpointV1 {
        GitHubReviewRateLimitCheckpointV1 {
            limit: 5_000,
            remaining,
            reset_at: UtcMicros(reset_at),
        }
    }

    #[test]
    fn an_unobserved_client_never_refuses_a_burst() {
        let tracker = GitHubRateLimitTrackerV1::default();
        assert_eq!(
            tracker.admit(20, UtcMicros(1_000)),
            GitHubRateLimitAdmissionV1::Unknown
        );
    }

    #[test]
    fn exhausted_and_partial_windows_are_admitted_truthfully() {
        let tracker = GitHubRateLimitTrackerV1::default();
        assert_eq!(
            tracker.record(&checkpoint(0, 9_000)),
            GitHubRateLimitRecordOutcomeV1::Recorded
        );
        assert!(matches!(
            tracker.admit(20, UtcMicros(1_000)),
            GitHubRateLimitAdmissionV1::Exhausted { .. }
        ));

        assert_eq!(
            tracker.record(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1 + 3, 9_000)),
            GitHubRateLimitRecordOutcomeV1::Recorded
        );
        assert!(matches!(
            tracker.admit(20, UtcMicros(1_000)),
            GitHubRateLimitAdmissionV1::Degraded {
                admitted_requests: 3,
                ..
            }
        ));
    }

    #[test]
    fn a_reset_window_is_forgotten() {
        let tracker = GitHubRateLimitTrackerV1::default();
        tracker.record(&checkpoint(0, 1_000));
        assert_eq!(
            tracker.admit(20, UtcMicros(2_000)),
            GitHubRateLimitAdmissionV1::Unknown
        );
    }

    #[test]
    fn an_invalid_checkpoint_is_not_retained() {
        let tracker = GitHubRateLimitTrackerV1::default();
        let invalid = GitHubReviewRateLimitCheckpointV1 {
            limit: 10,
            remaining: 11,
            reset_at: UtcMicros(9_000),
        };
        assert_eq!(
            tracker.record(&invalid),
            GitHubRateLimitRecordOutcomeV1::Invalid
        );
        assert_eq!(
            tracker.admit(20, UtcMicros(1_000)),
            GitHubRateLimitAdmissionV1::Unknown
        );
    }

    #[test]
    fn poisoned_client_state_is_typed_unavailable() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        let poison = Arc::clone(&tracker);
        let _ = std::thread::spawn(move || {
            let _guard = poison.checkpoint.lock().unwrap();
            panic!("poison fixture");
        })
        .join();
        assert_eq!(
            tracker.admit(20, UtcMicros(1_000)),
            GitHubRateLimitAdmissionV1::Unavailable
        );
    }
}
