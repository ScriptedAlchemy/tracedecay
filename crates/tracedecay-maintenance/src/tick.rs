//! Maintenance tick policy: continuation, cadence, and store-window selection.

use std::time::{Duration, Instant};

/// Resume a bounded maintenance phase over the normal graph window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceContinuation {
    /// Resume bounded code-generation retention over the normal graph window.
    CodeGenerationRetention,
}

/// Outcome of one maintenance tick or per-store unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceTickOutcome {
    Complete,
    Continue(MaintenanceContinuation),
    Retry,
}

impl MaintenanceTickOutcome {
    #[must_use]
    pub fn is_complete(self) -> bool {
        self == Self::Complete
    }

    #[must_use]
    pub fn continuation(self) -> Option<MaintenanceContinuation> {
        match self {
            Self::Continue(continuation) => Some(continuation),
            Self::Complete | Self::Retry => None,
        }
    }

    #[must_use]
    pub fn succeeded(self) -> bool {
        !matches!(self, Self::Retry)
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Continue(MaintenanceContinuation::CodeGenerationRetention) => {
                "code_generation_progress"
            }
            Self::Retry => "retry",
        }
    }

    /// A failure wins over ordinary bounded progress so the next short tick
    /// retries the complete maintenance journey.
    #[must_use]
    pub fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Retry, _) | (_, Self::Retry) => Self::Retry,
            (Self::Continue(continuation), Self::Continue(_) | Self::Complete)
            | (Self::Complete, Self::Continue(continuation)) => Self::Continue(continuation),
            (Self::Complete, Self::Complete) => Self::Complete,
        }
    }
}

/// Interval and retry-delay policy for the maintenance loop.
#[derive(Debug)]
pub struct MaintenanceCadence {
    interval: Duration,
    retry_delay: Duration,
    not_before: Option<Instant>,
    in_flight: bool,
}

impl MaintenanceCadence {
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self::with_retry_delay(interval, interval.min(Duration::from_mins(1)))
    }

    pub(crate) fn with_retry_delay(interval: Duration, retry_delay: Duration) -> Self {
        Self {
            interval,
            retry_delay,
            not_before: None,
            in_flight: false,
        }
    }

    pub fn reserve(&mut self, now: Instant) -> bool {
        if self.in_flight || self.not_before.is_some_and(|not_before| now < not_before) {
            return false;
        }
        self.in_flight = true;
        true
    }

    pub fn finish(&mut self, now: Instant, outcome: MaintenanceTickOutcome) -> Instant {
        self.in_flight = false;
        let delay = match outcome {
            MaintenanceTickOutcome::Complete => self.interval,
            MaintenanceTickOutcome::Continue(_) | MaintenanceTickOutcome::Retry => self.retry_delay,
        };
        let deadline = now + delay;
        self.not_before = Some(deadline);
        deadline
    }

    #[must_use]
    pub fn retry_delay(&self) -> Duration {
        self.retry_delay
    }

    /// Pull the next admission forward to at most one retry delay from `now`
    /// for work that just became collectable. An in-flight tick is left
    /// alone (its `finish` sets the next deadline); a deadline already
    /// nearer than that stays where it is.
    #[must_use]
    pub fn pull_forward(&mut self, now: Instant, deadline: Instant) -> Instant {
        if self.in_flight {
            return deadline;
        }
        let pulled = deadline.min(now + self.retry_delay);
        if self
            .not_before
            .is_some_and(|not_before| not_before > pulled)
        {
            self.not_before = Some(pulled);
        }
        pulled
    }
}

/// Pure round-robin window selection over stably-sorted store keys.
#[must_use]
pub fn select_store_window(
    keys: &[String],
    after: Option<&str>,
    budget: usize,
) -> (Vec<usize>, Option<String>) {
    let count = keys.len();
    if count == 0 || budget == 0 {
        return (Vec::new(), after.map(str::to_owned));
    }
    let start = match after {
        Some(cursor) => keys.partition_point(|key| key.as_str() <= cursor) % count,
        None => 0,
    };
    let take = budget.min(count);
    let indices = (0..take)
        .map(|offset| (start + offset) % count)
        .collect::<Vec<_>>();
    let next = indices.last().map(|&index| keys[index].clone());
    (indices, next)
}

#[must_use]
pub fn cursor_after_attempted_units(
    keys: &[String],
    window: &[usize],
    attempted: usize,
    prior: Option<&str>,
) -> Option<String> {
    attempted
        .checked_sub(1)
        .and_then(|last| window.get(last))
        .and_then(|&index| keys.get(index))
        .cloned()
        .or_else(|| prior.map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::{
        MaintenanceCadence, MaintenanceContinuation, MaintenanceTickOutcome,
        cursor_after_attempted_units, select_store_window,
    };
    use std::time::Duration;

    #[test]
    fn cadence_rate_limits_failures_and_successes() {
        let started = std::time::Instant::now();
        let mut cadence = MaintenanceCadence::new(Duration::from_mins(1));

        assert!(cadence.reserve(started));
        assert!(!cadence.reserve(started));
        assert_eq!(
            cadence.finish(started, MaintenanceTickOutcome::Retry),
            started + Duration::from_mins(1)
        );
        assert!(!cadence.reserve(started + Duration::from_secs(59)));
        let retried = started + Duration::from_mins(1);
        assert!(cadence.reserve(retried));
        assert_eq!(
            cadence.finish(retried, MaintenanceTickOutcome::Complete),
            retried + Duration::from_mins(1)
        );
        assert!(!cadence.reserve(retried + Duration::from_secs(59)));
        assert!(cadence.reserve(retried + Duration::from_mins(1)));
    }

    #[test]
    fn retry_outcome_takes_precedence_over_bounded_progress() {
        let progress =
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::CodeGenerationRetention);

        assert_eq!(
            progress.combine(MaintenanceTickOutcome::Retry),
            MaintenanceTickOutcome::Retry
        );
        assert_eq!(
            MaintenanceTickOutcome::Retry.combine(progress),
            MaintenanceTickOutcome::Retry
        );
    }

    #[test]
    fn code_generation_continuation_dominates_a_complete_tick() {
        let code_generation =
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::CodeGenerationRetention);

        assert_eq!(
            code_generation.combine(MaintenanceTickOutcome::Complete),
            code_generation
        );
        assert_eq!(
            MaintenanceTickOutcome::Complete.combine(code_generation),
            code_generation
        );
        assert_eq!(
            code_generation.combine(MaintenanceTickOutcome::Retry),
            MaintenanceTickOutcome::Retry
        );
    }

    fn store_keys(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("s:{index:03}")).collect()
    }

    #[test]
    fn store_window_round_robin_reaches_every_store_and_never_starves() {
        // With more stores than the budget, feeding each tick's cursor into the
        // next must cover every store within ceil(count / budget) ticks while
        // no tick exceeds the budget, nothing reclaimable is skipped forever.
        for &(count, budget) in &[(7usize, 3usize), (50, 8), (17, 5), (8, 8), (1, 8)] {
            let keys = store_keys(count);
            let ticks = count.div_ceil(budget);
            let mut cursor: Option<String> = None;
            let mut covered = std::collections::BTreeSet::new();
            for _ in 0..ticks {
                let (window, next) = select_store_window(&keys, cursor.as_deref(), budget);
                assert!(
                    window.len() <= budget,
                    "count={count} budget={budget}: tick exceeded budget"
                );
                for index in window {
                    covered.insert(index);
                }
                cursor = next;
            }
            assert_eq!(
                covered.len(),
                count,
                "count={count} budget={budget}: not every store reached within {ticks} ticks"
            );
        }
    }

    #[test]
    fn store_window_empty_set_preserves_cursor() {
        let (window, next) = select_store_window(&[], Some("s:005"), 8);
        assert_eq!(window.len(), 0);
        assert_eq!(next.as_deref(), Some("s:005"));
    }

    #[test]
    fn maintenance_cursor_advances_only_past_attempted_units() {
        let keys = store_keys(8);
        let (window, _) = select_store_window(&keys, None, 4);

        assert_eq!(
            cursor_after_attempted_units(&keys, &window, 2, None).as_deref(),
            Some("s:001")
        );
        assert_eq!(
            cursor_after_attempted_units(&keys, &window, 0, Some("s:007")).as_deref(),
            Some("s:007")
        );
    }
}
