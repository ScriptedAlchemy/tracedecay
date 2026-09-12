//! Maintenance tick policy: continuation, cadence, and store-window selection.

use std::time::Duration;

/// Resume a bounded maintenance phase over the normal graph window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceContinuation {
    /// Resume the bounded semantic-vector phase over the normal graph window.
    SemanticVectorRetention,
    /// Resume bounded code-generation retention over the normal graph window.
    CodeGenerationRetention,
}

impl MaintenanceContinuation {
    /// Two phases asking to continue collapse to the one whose continuation
    /// tick still advances both.
    #[must_use]
    pub fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::CodeGenerationRetention, _) | (_, Self::CodeGenerationRetention) => {
                Self::CodeGenerationRetention
            }
            (Self::SemanticVectorRetention, Self::SemanticVectorRetention) => {
                Self::SemanticVectorRetention
            }
        }
    }
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
            Self::Continue(MaintenanceContinuation::SemanticVectorRetention) => {
                "semantic_vector_progress"
            }
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
            (Self::Continue(left), Self::Continue(right)) => Self::Continue(left.combine(right)),
            (Self::Continue(continuation), Self::Complete)
            | (Self::Complete, Self::Continue(continuation)) => Self::Continue(continuation),
            (Self::Complete, Self::Complete) => Self::Complete,
        }
    }
}

/// The maintenance loop parks on `tokio::time::sleep_until`, so every deadline
/// it derives must be measured on the same clock the timer wheel uses.
pub type CadenceInstant = tokio::time::Instant;

/// Interval and retry-delay policy for the maintenance loop.
#[derive(Debug)]
pub struct MaintenanceCadence {
    interval: Duration,
    retry_delay: Duration,
    not_before: Option<CadenceInstant>,
    in_flight: bool,
}

impl MaintenanceCadence {
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            retry_delay: interval.min(Duration::from_mins(1)),
            not_before: None,
            in_flight: false,
        }
    }

    pub fn reserve(&mut self, now: CadenceInstant) -> bool {
        if self.in_flight || self.not_before.is_some_and(|not_before| now < not_before) {
            return false;
        }
        self.in_flight = true;
        true
    }

    pub fn finish(
        &mut self,
        now: CadenceInstant,
        outcome: MaintenanceTickOutcome,
    ) -> CadenceInstant {
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
    pub fn pull_forward(&mut self, now: CadenceInstant, deadline: CadenceInstant) -> CadenceInstant {
        if self.in_flight {
            return deadline;
        }
        let pulled = deadline.min(now + self.retry_delay);
        if self.not_before.is_some_and(|not_before| not_before > pulled) {
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

/// Whether any retention or compaction window is configured.
#[must_use]
pub fn retention_maintenance_enabled(
    orphan_store_gc_days: Option<u64>,
    incident_debris_retention_days: Option<u64>,
    compaction: bool,
) -> bool {
    orphan_store_gc_days.is_some() || incident_debris_retention_days.is_some() || compaction
}
