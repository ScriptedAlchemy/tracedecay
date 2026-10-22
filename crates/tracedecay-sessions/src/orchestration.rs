use std::collections::BTreeMap;

use crate::TranscriptIngestStats;

/// Failure behavior needed by the provider-run retry loop.
pub trait ProviderRunFailure {
    fn retryable(&self) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRunOutcome<F> {
    pub stats: TranscriptIngestStats,
    pub failures: Vec<F>,
    pub bytes_consumed: u64,
    pub deferred_units: u64,
    pub byte_bounds_enforced: bool,
    /// The run durably advanced source coverage (a cursor, a skipped range,
    /// or a projection), whether or not it upserted any message.
    pub coverage_advanced: bool,
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
            coverage_advanced: bytes_consumed > 0,
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
            coverage_advanced: false,
            admitted: false,
        }
    }

    pub fn failed(failure: F, bytes_consumed: u64) -> Self {
        let mut outcome = Self::bounded(TranscriptIngestStats::default(), bytes_consumed, false);
        outcome.failures.push(failure);
        outcome.coverage_advanced = false;
        outcome
    }

    pub fn add_failure(&mut self, failure: F) {
        self.failures.push(failure);
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

/// One provider's admitted-unit dispositions in bounded catch-up passes. A
/// unit is one provider run inside a pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ProviderPassUnits {
    /// Runs that finished within their byte grant without a failure.
    pub units_projected: u64,
    /// Runs that reported a failure.
    pub units_refused: u64,
    /// Work handed to a later pass: runs that stopped at their byte grant and
    /// admitted providers the pass budget never reached.
    pub units_parked: u64,
}

impl ProviderPassUnits {
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            units_projected: self.units_projected.saturating_add(other.units_projected),
            units_refused: self.units_refused.saturating_add(other.units_refused),
            units_parked: self.units_parked.saturating_add(other.units_parked),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRunFold<F> {
    pub stats: TranscriptIngestStats,
    pub failures: Vec<F>,
    pub units_admitted: u64,
    pub units_completed: u64,
    pub units_failed: u64,
    pub deferred_units: u64,
    pub byte_bounds_enforced: bool,
    /// Some run without a failure durably advanced source coverage.
    pub coverage_advanced: bool,
    pub provider_units: BTreeMap<&'static str, ProviderPassUnits>,
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
            coverage_advanced: false,
            provider_units: BTreeMap::new(),
        }
    }
}

impl<F: ProviderRunFailure + Clone> ProviderRunFold<F> {
    /// Accounts an attempt the caller abandons to retry, keeping its
    /// non-retryable failures.
    pub fn record_retry(&mut self, outcome: &ProviderRunOutcome<F>) {
        self.deferred_units = self.deferred_units.saturating_add(outcome.deferred_units);
        self.byte_bounds_enforced &= outcome.byte_bounds_enforced;
        self.failures.extend(
            outcome
                .failures
                .iter()
                .filter(|failure| !failure.retryable())
                .cloned(),
        );
    }
}

impl<F> ProviderRunFold<F> {
    pub fn record(&mut self, provider: &'static str, outcome: ProviderRunOutcome<F>) {
        if !outcome.admitted {
            return;
        }
        self.units_admitted = self.units_admitted.saturating_add(1);
        let units = self.provider_units.entry(provider).or_default();
        if outcome.failures.is_empty() {
            self.coverage_advanced |= outcome.coverage_advanced;
            if outcome.deferred_units == 0 {
                self.units_completed = self.units_completed.saturating_add(1);
                units.units_projected = units.units_projected.saturating_add(1);
            } else {
                units.units_parked = units.units_parked.saturating_add(1);
            }
        } else {
            self.units_failed = self.units_failed.saturating_add(1);
            units.units_refused = units.units_refused.saturating_add(1);
        }
        self.stats = self.stats.merge(outcome.stats);
        self.failures.extend(outcome.failures);
        self.deferred_units = self.deferred_units.saturating_add(outcome.deferred_units);
        self.byte_bounds_enforced &= outcome.byte_bounds_enforced;
    }

    /// Accounts an admitted provider the pass budget never reached.
    pub fn park(&mut self, provider: &'static str) {
        let units = self.provider_units.entry(provider).or_default();
        units.units_parked = units.units_parked.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{ProviderPassUnits, ProviderRunFailure, ProviderRunFold, ProviderRunOutcome};
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

        fold.record("codex", ProviderRunOutcome::skipped());

        assert_eq!(fold, ProviderRunFold::default());
    }

    #[test]
    fn fold_aggregates_bounded_provider_outcomes() {
        let mut fold = ProviderRunFold::<Failure>::default();
        fold.record(
            "codex",
            ProviderRunOutcome::bounded(
                TranscriptIngestStats {
                    sessions_upserted: u64::MAX,
                    messages_upserted: 2,
                },
                7,
                false,
            ),
        );
        fold.record(
            "codex",
            ProviderRunOutcome::bounded(
                TranscriptIngestStats {
                    sessions_upserted: 1,
                    messages_upserted: 3,
                },
                11,
                true,
            ),
        );

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

        fold.record("codex", outcome);
        assert_eq!(fold.units_admitted, 1);
        assert_eq!(fold.units_completed, 0);
        assert_eq!(fold.units_failed, 1);
        assert_eq!(fold.failures, vec![failure]);
        assert_eq!(fold.deferred_units, 4);
    }

    #[test]
    fn a_retried_attempt_keeps_its_permanent_failures() {
        let permanent = Failure { retryable: false };
        let mut abandoned = ProviderRunOutcome::failed(permanent.clone(), 5);
        abandoned.add_failure(Failure { retryable: true });

        let mut fold = ProviderRunFold::default();
        fold.record_retry(&abandoned);
        fold.record(
            "codex",
            ProviderRunOutcome::bounded(TranscriptIngestStats::default(), 3, false),
        );

        assert_eq!(
            fold.failures,
            vec![permanent],
            "the retry skips a quarantined source, so only the abandoned attempt reports it"
        );
    }

    #[test]
    fn fold_tallies_each_provider_disposition_separately() {
        let mut fold = ProviderRunFold::<Failure>::default();
        fold.record(
            "codex",
            ProviderRunOutcome::bounded(
                TranscriptIngestStats {
                    sessions_upserted: 1,
                    messages_upserted: 4,
                },
                7,
                false,
            ),
        );
        fold.record(
            "cursor",
            ProviderRunOutcome::bounded(
                TranscriptIngestStats {
                    sessions_upserted: 0,
                    messages_upserted: 2,
                },
                11,
                true,
            ),
        );
        fold.record(
            "claude",
            ProviderRunOutcome::failed(Failure { retryable: true }, 3),
        );
        fold.park("kimi");
        fold.record("pi", ProviderRunOutcome::skipped());

        assert_eq!(
            fold.provider_units.into_iter().collect::<Vec<_>>(),
            vec![
                (
                    "claude",
                    ProviderPassUnits {
                        units_refused: 1,
                        ..ProviderPassUnits::default()
                    }
                ),
                (
                    "codex",
                    ProviderPassUnits {
                        units_projected: 1,
                        ..ProviderPassUnits::default()
                    }
                ),
                (
                    "cursor",
                    ProviderPassUnits {
                        units_parked: 1,
                        ..ProviderPassUnits::default()
                    }
                ),
                (
                    "kimi",
                    ProviderPassUnits {
                        units_parked: 1,
                        ..ProviderPassUnits::default()
                    }
                ),
            ],
            "a skipped provider and an empty drain leave no tally"
        );
    }
}
