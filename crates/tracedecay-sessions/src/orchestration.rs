use crate::TranscriptIngestStats;

/// Failure behavior needed by the provider-run retry loop.
pub trait ProviderRunFailure {
    fn retryable(&self) -> bool;
}

/// One provider driver's bounded contribution to an orchestration pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRunOutcome<F> {
    pub stats: TranscriptIngestStats,
    pub failures: Vec<F>,
    pub bytes_consumed: u64,
    pub deferred_units: u64,
    pub byte_bounds_enforced: bool,
    admitted: bool,
}

impl<F> ProviderRunOutcome<F> {
    pub fn bounded(
        stats: TranscriptIngestStats,
        bytes_consumed: u64,
        deferred_by_byte_cap: bool,
    ) -> Self {
        Self {
            stats,
            failures: Vec::new(),
            bytes_consumed,
            deferred_units: u64::from(deferred_by_byte_cap),
            byte_bounds_enforced: true,
            admitted: true,
        }
    }

    pub fn skipped() -> Self {
        Self {
            stats: TranscriptIngestStats::default(),
            failures: Vec::new(),
            bytes_consumed: 0,
            deferred_units: 0,
            byte_bounds_enforced: true,
            admitted: false,
        }
    }

    pub fn failed(failure: F, bytes_consumed: u64) -> Self {
        let mut outcome = Self::bounded(TranscriptIngestStats::default(), bytes_consumed, false);
        outcome.failures.push(failure);
        outcome
    }

    pub fn add_failure(&mut self, failure: F) {
        self.failures.push(failure);
    }

    pub fn add_stats(&mut self, stats: TranscriptIngestStats) {
        self.stats = self.stats.merge(stats);
    }

    pub fn add_deferred_units(&mut self, deferred_units: u64) {
        self.deferred_units = self.deferred_units.saturating_add(deferred_units);
    }

    pub fn succeeded(&self) -> bool {
        self.failures.is_empty()
    }
}

impl<F: ProviderRunFailure> ProviderRunOutcome<F> {
    pub fn retryable(&self) -> bool {
        self.failures
            .last()
            .is_some_and(ProviderRunFailure::retryable)
    }
}

/// Pass-level aggregation of provider-run outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRunFold<F> {
    pub stats: TranscriptIngestStats,
    pub failures: Vec<F>,
    pub units_admitted: u64,
    pub units_completed: u64,
    pub units_failed: u64,
    pub deferred_units: u64,
    pub byte_bounds_enforced: bool,
}

impl<F> Default for ProviderRunFold<F> {
    fn default() -> Self {
        Self {
            stats: TranscriptIngestStats::default(),
            failures: Vec::new(),
            units_admitted: 0,
            units_completed: 0,
            units_failed: 0,
            deferred_units: 0,
            byte_bounds_enforced: true,
        }
    }
}

impl<F> ProviderRunFold<F> {
    pub fn record_retry(&mut self, outcome: &ProviderRunOutcome<F>) {
        self.deferred_units = self.deferred_units.saturating_add(outcome.deferred_units);
        self.byte_bounds_enforced &= outcome.byte_bounds_enforced;
    }

    pub fn record(&mut self, outcome: ProviderRunOutcome<F>) {
        if !outcome.admitted {
            return;
        }
        self.units_admitted = self.units_admitted.saturating_add(1);
        if outcome.failures.is_empty() {
            if outcome.deferred_units == 0 {
                self.units_completed = self.units_completed.saturating_add(1);
            }
        } else {
            self.units_failed = self.units_failed.saturating_add(1);
        }
        self.stats = self.stats.merge(outcome.stats);
        self.failures.extend(outcome.failures);
        self.deferred_units = self.deferred_units.saturating_add(outcome.deferred_units);
        self.byte_bounds_enforced &= outcome.byte_bounds_enforced;
    }
}

#[cfg(test)]
mod tests {
    use super::{ProviderRunFailure, ProviderRunFold, ProviderRunOutcome};
    use crate::TranscriptIngestStats;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Failure {
        retryable: bool,
    }

    impl ProviderRunFailure for Failure {
        fn retryable(&self) -> bool {
            self.retryable
        }
    }

    #[test]
    fn skipped_provider_does_not_count_as_admitted() {
        let mut fold = ProviderRunFold::<Failure>::default();

        fold.record(ProviderRunOutcome::skipped());

        assert_eq!(fold, ProviderRunFold::default());
    }

    #[test]
    fn fold_aggregates_bounded_provider_outcomes() {
        let mut fold = ProviderRunFold::<Failure>::default();
        fold.record(ProviderRunOutcome::bounded(
            TranscriptIngestStats {
                sessions_upserted: u64::MAX,
                messages_upserted: 2,
            },
            7,
            false,
        ));
        fold.record(ProviderRunOutcome::bounded(
            TranscriptIngestStats {
                sessions_upserted: 1,
                messages_upserted: 3,
            },
            11,
            true,
        ));

        assert_eq!(fold.stats.sessions_upserted, u64::MAX);
        assert_eq!(fold.stats.messages_upserted, 5);
        assert_eq!(fold.units_admitted, 2);
        assert_eq!(fold.units_completed, 1);
        assert_eq!(fold.units_failed, 0);
        assert_eq!(fold.deferred_units, 1);
        assert!(fold.byte_bounds_enforced);
    }

    #[test]
    fn retry_and_terminal_failure_have_distinct_accounting() {
        let failure = Failure { retryable: true };
        let mut outcome = ProviderRunOutcome::failed(failure.clone(), 5);
        outcome.add_deferred_units(2);
        outcome.byte_bounds_enforced = false;
        assert!(outcome.retryable());

        let mut fold = ProviderRunFold::default();
        fold.record_retry(&outcome);
        assert_eq!(fold.units_admitted, 0);
        assert_eq!(fold.deferred_units, 2);
        assert!(!fold.byte_bounds_enforced);

        fold.record(outcome);
        assert_eq!(fold.units_admitted, 1);
        assert_eq!(fold.units_completed, 0);
        assert_eq!(fold.units_failed, 1);
        assert_eq!(fold.failures, vec![failure]);
        assert_eq!(fold.deferred_units, 4);
    }
}
