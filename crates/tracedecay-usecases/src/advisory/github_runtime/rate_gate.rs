//! Atomic, client-owned admission for individual GitHub CI requests.
//!
//! Every request reserves one observed quota slot before it can reach the
//! provider. A dropped permit releases an unused slot, while a completed
//! request reconciles the reservation with the provider checkpoint it
//! observed. Unknown quota admits one request so its response can establish
//! the current window; callers must acquire again before a following page.

use std::sync::{Arc, Mutex};

use tracedecay_domain::UtcMicros;
use tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1;

const GITHUB_RATE_LIMIT_RESERVE_V1: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
pub(super) enum GitHubRateLimitRecordOutcomeV1 {
    Recorded,
    Invalid,
    Unavailable,
}

pub(super) enum GitHubRequestAdmissionV1 {
    Granted(GitHubRequestQuotaPermitV1),
    RateLimited(GitHubReviewRateLimitCheckpointV1),
    Unavailable,
}

#[derive(Default)]
struct GitHubRateLimitStateV1 {
    checkpoint: Option<GitHubReviewRateLimitCheckpointV1>,
    latest_reset_at: Option<UtcMicros>,
    blocked_until: Option<UtcMicros>,
    reservations: u32,
    unknown_probe_in_flight: bool,
    unknown_probe_failed_closed: bool,
}

#[derive(Default)]
pub(super) struct GitHubRateLimitTrackerV1 {
    state: Mutex<GitHubRateLimitStateV1>,
}

impl GitHubRateLimitTrackerV1 {
    #[cfg(test)]
    pub(super) fn record(
        &self,
        checkpoint: &GitHubReviewRateLimitCheckpointV1,
    ) -> GitHubRateLimitRecordOutcomeV1 {
        if checkpoint.validate().is_err() {
            return GitHubRateLimitRecordOutcomeV1::Invalid;
        }
        let Ok(mut state) = self.state.lock() else {
            return GitHubRateLimitRecordOutcomeV1::Unavailable;
        };
        Self::record_locked(&mut state, checkpoint);
        GitHubRateLimitRecordOutcomeV1::Recorded
    }

    pub(super) fn acquire(self: Arc<Self>, now: UtcMicros) -> GitHubRequestAdmissionV1 {
        enum Decision {
            UnknownProbe,
            Reserved(UtcMicros),
            RateLimited(GitHubReviewRateLimitCheckpointV1),
            Unavailable,
        }
        let decision = {
            let Ok(mut state) = self.state.lock() else {
                return GitHubRequestAdmissionV1::Unavailable;
            };
            if state
                .blocked_until
                .is_some_and(|blocked_until| blocked_until.0 <= now.0)
            {
                state.blocked_until = None;
            }
            if state.blocked_until.is_none()
                && state
                    .checkpoint
                    .as_ref()
                    .is_some_and(|checkpoint| checkpoint.reset_at.0 <= now.0)
            {
                state.checkpoint = None;
                state.unknown_probe_failed_closed = false;
            }
            if let Some(blocked_until) = state.blocked_until {
                state.checkpoint.as_ref().cloned().map_or(
                    Decision::Unavailable,
                    |mut checkpoint| {
                        checkpoint.reset_at = blocked_until;
                        Decision::RateLimited(checkpoint)
                    },
                )
            } else if let Some(checkpoint) = state.checkpoint.as_ref().cloned() {
                let available = checkpoint
                    .remaining
                    .saturating_sub(GITHUB_RATE_LIMIT_RESERVE_V1)
                    .saturating_sub(state.reservations);
                if available == 0 {
                    Decision::RateLimited(checkpoint)
                } else {
                    state.reservations = state.reservations.saturating_add(1);
                    Decision::Reserved(checkpoint.reset_at)
                }
            } else {
                if state.unknown_probe_in_flight || state.unknown_probe_failed_closed {
                    Decision::Unavailable
                } else {
                    state.unknown_probe_in_flight = true;
                    Decision::UnknownProbe
                }
            }
        };
        match decision {
            Decision::UnknownProbe => {
                GitHubRequestAdmissionV1::Granted(GitHubRequestQuotaPermitV1 {
                    tracker: self,
                    reserved_reset_at: None,
                    unknown_probe: true,
                    finished: false,
                })
            }
            Decision::Reserved(reset_at) => {
                GitHubRequestAdmissionV1::Granted(GitHubRequestQuotaPermitV1 {
                    tracker: self,
                    reserved_reset_at: Some(reset_at),
                    unknown_probe: false,
                    finished: false,
                })
            }
            Decision::RateLimited(checkpoint) => GitHubRequestAdmissionV1::RateLimited(checkpoint),
            Decision::Unavailable => GitHubRequestAdmissionV1::Unavailable,
        }
    }

    fn record_locked(
        state: &mut GitHubRateLimitStateV1,
        observed: &GitHubReviewRateLimitCheckpointV1,
    ) -> bool {
        let observation_is_stale = state.latest_reset_at.is_some_and(|latest_reset_at| {
            observed.reset_at.0 < latest_reset_at.0
                || (state.checkpoint.is_none() && observed.reset_at == latest_reset_at)
        });
        if observation_is_stale {
            return false;
        }
        state.latest_reset_at = Some(observed.reset_at);
        state.unknown_probe_failed_closed = false;
        match state.checkpoint.as_mut() {
            Some(current) if current.reset_at.0 > observed.reset_at.0 => {}
            Some(current) if current.reset_at == observed.reset_at => {
                current.limit = current.limit.min(observed.limit);
                current.remaining = current.remaining.min(observed.remaining);
            }
            Some(_) => {
                state.checkpoint = Some(observed.clone());
            }
            None => {
                state.checkpoint = Some(observed.clone());
            }
        }
        true
    }

    fn finish(
        &self,
        reserved_reset_at: Option<UtcMicros>,
        unknown_probe: bool,
        observed: Option<&GitHubReviewRateLimitCheckpointV1>,
        blocked_until: Option<UtcMicros>,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let observed = observed.filter(|checkpoint| checkpoint.validate().is_ok());
        if reserved_reset_at.is_some()
            && let Some(checkpoint) = state.checkpoint.as_mut()
            && observed.is_none()
        {
            checkpoint.remaining = checkpoint.remaining.saturating_sub(1);
        }
        if reserved_reset_at.is_some() {
            state.reservations = state.reservations.saturating_sub(1);
        }
        if let Some(blocked_until) = blocked_until {
            state.blocked_until = Some(state.blocked_until.map_or(blocked_until, |current| {
                UtcMicros(current.0.max(blocked_until.0))
            }));
        }
        let accepted_observation =
            observed.is_some_and(|observed| Self::record_locked(&mut state, observed));
        if unknown_probe {
            state.unknown_probe_in_flight = false;
            state.unknown_probe_failed_closed = !accepted_observation;
        }
    }

    fn release(&self, reserved_reset_at: Option<UtcMicros>, unknown_probe: bool) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if reserved_reset_at.is_some() {
            state.reservations = state.reservations.saturating_sub(1);
        }
        if unknown_probe {
            state.unknown_probe_in_flight = false;
        }
    }
}

pub(super) struct GitHubRequestQuotaPermitV1 {
    tracker: Arc<GitHubRateLimitTrackerV1>,
    reserved_reset_at: Option<UtcMicros>,
    unknown_probe: bool,
    finished: bool,
}

impl GitHubRequestQuotaPermitV1 {
    pub(super) fn finish(
        mut self,
        observed: Option<&GitHubReviewRateLimitCheckpointV1>,
        blocked_until: Option<UtcMicros>,
    ) {
        self.tracker.finish(
            self.reserved_reset_at,
            self.unknown_probe,
            observed,
            blocked_until,
        );
        self.finished = true;
    }
}

impl Drop for GitHubRequestQuotaPermitV1 {
    fn drop(&mut self) {
        if !self.finished {
            self.tracker
                .release(self.reserved_reset_at, self.unknown_probe);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(remaining: u32, reset_at: i64) -> GitHubReviewRateLimitCheckpointV1 {
        GitHubReviewRateLimitCheckpointV1 {
            limit: 5_000,
            remaining,
            reset_at: UtcMicros(reset_at),
        }
    }

    fn granted(outcome: GitHubRequestAdmissionV1) -> GitHubRequestQuotaPermitV1 {
        let GitHubRequestAdmissionV1::Granted(permit) = outcome else {
            panic!("expected one request permit");
        };
        permit
    }

    #[test]
    fn an_unknown_first_response_at_the_reserve_refuses_page_two() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        let first = granted(Arc::clone(&tracker).acquire(UtcMicros(1_000)));
        first.finish(Some(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1, 9_000)), None);

        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(1_000)),
            GitHubRequestAdmissionV1::RateLimited(_)
        ));
    }

    #[test]
    fn an_unknown_first_response_without_a_checkpoint_refuses_page_two() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        let first = granted(Arc::clone(&tracker).acquire(UtcMicros(1_000)));
        first.finish(None, None);

        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(1_000)),
            GitHubRequestAdmissionV1::Unavailable
        ));
    }

    #[test]
    fn an_unknown_first_response_with_an_invalid_checkpoint_refuses_page_two() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        let first = granted(Arc::clone(&tracker).acquire(UtcMicros(1_000)));
        first.finish(
            Some(&GitHubReviewRateLimitCheckpointV1 {
                limit: 10,
                remaining: 11,
                reset_at: UtcMicros(9_000),
            }),
            None,
        );

        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(1_000)),
            GitHubRequestAdmissionV1::Unavailable
        ));
    }

    #[test]
    fn an_expired_bootstrap_checkpoint_does_not_clear_fail_closed_state() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        tracker.record(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1 + 1, 2_000));
        let probe = granted(Arc::clone(&tracker).acquire(UtcMicros(2_000)));

        probe.finish(Some(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1, 2_000)), None);

        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(2_000)),
            GitHubRequestAdmissionV1::Unavailable
        ));
    }

    #[test]
    fn secondary_limit_blocks_positive_primary_quota_until_its_deadline() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        tracker.record(&checkpoint(4_000, 9_000));
        let permit = granted(Arc::clone(&tracker).acquire(UtcMicros(1_000)));
        permit.finish(Some(&checkpoint(3_999, 9_000)), Some(UtcMicros(2_000)));

        let GitHubRequestAdmissionV1::RateLimited(blocked) =
            Arc::clone(&tracker).acquire(UtcMicros(1_999))
        else {
            panic!("secondary limit must block before its deadline");
        };
        assert_eq!(blocked.limit, 5_000);
        assert_eq!(blocked.remaining, 3_999);
        assert_eq!(blocked.reset_at, UtcMicros(2_000));
        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(2_000)),
            GitHubRequestAdmissionV1::Granted(_)
        ));
    }

    #[test]
    fn retry_after_only_blocks_known_quota_until_its_deadline() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        tracker.record(&checkpoint(4_000, 9_000));
        let permit = granted(Arc::clone(&tracker).acquire(UtcMicros(1_000)));
        permit.finish(None, Some(UtcMicros(2_000)));

        let GitHubRequestAdmissionV1::RateLimited(blocked) =
            Arc::clone(&tracker).acquire(UtcMicros(1_999))
        else {
            panic!("Retry-After must block before its deadline");
        };
        assert_eq!(blocked.limit, 5_000);
        assert_eq!(blocked.remaining, 3_999);
        assert_eq!(blocked.reset_at, UtcMicros(2_000));
        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(2_000)),
            GitHubRequestAdmissionV1::Granted(_)
        ));
    }

    #[test]
    fn secondary_limit_remains_typed_after_the_primary_window_expires() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        tracker.record(&checkpoint(4_000, 3_000));
        let permit = granted(Arc::clone(&tracker).acquire(UtcMicros(1_000)));
        permit.finish(Some(&checkpoint(3_999, 3_000)), Some(UtcMicros(6_000)));

        for now in [UtcMicros(3_000), UtcMicros(5_999)] {
            let GitHubRequestAdmissionV1::RateLimited(blocked) = Arc::clone(&tracker).acquire(now)
            else {
                panic!("secondary limit must remain typed through its deadline");
            };
            assert_eq!(blocked.limit, 5_000);
            assert_eq!(blocked.remaining, 3_999);
            assert_eq!(blocked.reset_at, UtcMicros(6_000));
        }
        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(6_000)),
            GitHubRequestAdmissionV1::Granted(_)
        ));
    }

    #[test]
    fn synchronized_bursts_cannot_spend_more_than_the_shared_remainder() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        tracker.record(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1 + 3, 9_000));
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();

        for _ in 0..2 {
            let tracker = Arc::clone(&tracker);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                let mut spent = 0;
                while let GitHubRequestAdmissionV1::Granted(permit) =
                    Arc::clone(&tracker).acquire(UtcMicros(1_000))
                {
                    permit.finish(None, None);
                    spent += 1;
                }
                spent
            }));
        }

        barrier.wait();
        let spent: u32 = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum();
        assert_eq!(spent, 3);
    }

    #[test]
    fn synchronized_unknown_bursts_share_one_bootstrap_probe() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        let start = Arc::new(std::sync::Barrier::new(3));
        let acquired = Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();

        for _ in 0..2 {
            let tracker = Arc::clone(&tracker);
            let start = Arc::clone(&start);
            let acquired = Arc::clone(&acquired);
            workers.push(std::thread::spawn(move || {
                start.wait();
                let outcome = Arc::clone(&tracker).acquire(UtcMicros(1_000));
                acquired.wait();
                matches!(outcome, GitHubRequestAdmissionV1::Granted(_))
            }));
        }

        start.wait();
        acquired.wait();
        let granted = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|was_granted| *was_granted)
            .count();
        assert_eq!(granted, 1);
    }

    #[test]
    fn dropping_an_unused_permit_releases_its_reservation() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        tracker.record(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1 + 3, 9_000));

        drop(granted(Arc::clone(&tracker).acquire(UtcMicros(1_000))));
        for _ in 0..3 {
            granted(Arc::clone(&tracker).acquire(UtcMicros(1_000))).finish(None, None);
        }
        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(1_000)),
            GitHubRequestAdmissionV1::RateLimited(_)
        ));
    }

    #[test]
    fn dropping_an_unused_unknown_probe_allows_a_replacement() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());

        drop(granted(Arc::clone(&tracker).acquire(UtcMicros(1_000))));

        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(1_000)),
            GitHubRequestAdmissionV1::Granted(_)
        ));
    }

    #[test]
    fn a_new_window_preserves_outstanding_reservations() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        tracker.record(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1 + 2, 9_000));
        let first = granted(Arc::clone(&tracker).acquire(UtcMicros(1_000)));
        let second = granted(Arc::clone(&tracker).acquire(UtcMicros(1_000)));

        tracker.record(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1 + 2, 10_000));

        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(1_000)),
            GitHubRequestAdmissionV1::RateLimited(_)
        ));
        drop(first);
        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(1_000)),
            GitHubRequestAdmissionV1::Granted(_)
        ));
        drop(second);
    }

    #[test]
    fn a_headerless_old_window_completion_charges_the_new_checkpoint() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        tracker.record(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1 + 1, 9_000));
        let old_window = granted(Arc::clone(&tracker).acquire(UtcMicros(1_000)));
        tracker.record(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1 + 1, 10_000));

        old_window.finish(None, None);

        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(1_000)),
            GitHubRequestAdmissionV1::RateLimited(_)
        ));
    }

    #[test]
    fn a_late_expired_checkpoint_cannot_reopen_failed_unknown_authority() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        tracker.record(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1 + 1, 2_000));
        let old_window = granted(Arc::clone(&tracker).acquire(UtcMicros(1_000)));
        let new_window_probe = granted(Arc::clone(&tracker).acquire(UtcMicros(2_000)));
        new_window_probe.finish(None, None);

        old_window.finish(Some(&checkpoint(GITHUB_RATE_LIMIT_RESERVE_V1, 2_000)), None);

        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(2_000)),
            GitHubRequestAdmissionV1::Unavailable
        ));
    }

    #[test]
    fn a_reset_window_stops_gating() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        tracker.record(&checkpoint(0, 1_000));
        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(2_000)),
            GitHubRequestAdmissionV1::Granted(_)
        ));
    }

    #[test]
    fn an_invalid_checkpoint_is_not_retained() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        let invalid = GitHubReviewRateLimitCheckpointV1 {
            limit: 10,
            remaining: 11,
            reset_at: UtcMicros(9_000),
        };
        assert_eq!(
            tracker.record(&invalid),
            GitHubRateLimitRecordOutcomeV1::Invalid
        );
        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(1_000)),
            GitHubRequestAdmissionV1::Granted(_)
        ));
    }

    #[test]
    fn poisoned_client_state_is_typed_unavailable() {
        let tracker = Arc::new(GitHubRateLimitTrackerV1::default());
        let poison = Arc::clone(&tracker);
        let _ = std::thread::spawn(move || {
            let _guard = poison.state.lock().unwrap();
            panic!("poison fixture");
        })
        .join();
        assert!(matches!(
            Arc::clone(&tracker).acquire(UtcMicros(1_000)),
            GitHubRequestAdmissionV1::Unavailable
        ));
    }
}
