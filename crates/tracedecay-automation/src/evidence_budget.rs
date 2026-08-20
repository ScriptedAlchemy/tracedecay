//! Typed accounting contract for session-evidence budget exhaustion.
//!
//! A retrieval attempt that exhausts its context/work budgets is a correct
//! terminal *skip* for the run that observed it: the evidence exists but
//! cannot be admitted inside the configured bounds, and re-running the same
//! bounded query on the next scheduler tick reproduces the exhaustion.
//! This module owns that state as a typed outcome so schedulers hold back
//! for a deterministic window instead of re-attempting (and re-reporting)
//! the exhausted retrieval on every tick.

use std::num::NonZeroU64;

pub use tracedecay_domain::{
    SESSION_EVIDENCE_BUDGET_EXHAUSTED, SESSION_EVIDENCE_BUDGET_SUPPRESSED,
};

/// A budget-exhausted retrieval attempt observed by an automation task.
///
/// The state is anchored on the attempt that actually ran the retrieval and
/// observed exhaustion; suppressed ticks between attempts do not move it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionEvidenceBudgetExceeded {
    /// Unix seconds when the exhausted attempt completed.
    pub observed_at_secs: i64,
}

/// Scheduler decision derived from a standing budget-exhausted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvidenceBudgetGate {
    /// The suppression window has elapsed; one fresh retrieval attempt may
    /// run (and, if it exhausts again, re-anchor the state).
    AttemptPermitted,
    /// The tick falls inside the suppression window; the task must skip
    /// without attempting retrieval.
    Suppressed {
        /// Unix seconds when the window ends and an attempt is permitted.
        until_secs: i64,
    },
}

/// Deterministic backoff between budget-exhausted retrieval attempts.
///
/// Exhaustion only clears when the underlying evidence or budgets change, so
/// the window trades staleness of the next attempt against wasted retrieval
/// work: at most one attempt per window instead of one per scheduler tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionEvidenceBudgetBackoff {
    suppression_secs: u64,
}

/// One retrieval attempt per hour while the budget stays exhausted. The
/// scheduler tick is measured in seconds, so this collapses a standing
/// exhaustion from one attempt (and one report) per tick to at most one per
/// hour without hiding the state.
const DEFAULT_SUPPRESSION_SECS: u64 = 3_600;

impl Default for SessionEvidenceBudgetBackoff {
    fn default() -> Self {
        Self {
            suppression_secs: DEFAULT_SUPPRESSION_SECS,
        }
    }
}

impl SessionEvidenceBudgetBackoff {
    /// A zero window would degenerate into "attempt on every tick" — exactly
    /// the retry loop this contract exists to stop — so it is unrepresentable:
    /// the constructor only accepts non-zero windows.
    #[must_use]
    pub const fn new(suppression_secs: NonZeroU64) -> Self {
        Self {
            suppression_secs: suppression_secs.get(),
        }
    }

    #[must_use]
    pub const fn suppression_secs(&self) -> u64 {
        self.suppression_secs
    }

    /// Gates one scheduler tick against the most recent exhausted attempt.
    #[must_use]
    pub fn gate(
        &self,
        exceeded: SessionEvidenceBudgetExceeded,
        now_secs: i64,
    ) -> SessionEvidenceBudgetGate {
        let until_secs = exceeded
            .observed_at_secs
            .saturating_add(i64::try_from(self.suppression_secs).unwrap_or(i64::MAX));
        if now_secs < until_secs {
            SessionEvidenceBudgetGate::Suppressed { until_secs }
        } else {
            SessionEvidenceBudgetGate::AttemptPermitted
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    fn window(secs: u64) -> SessionEvidenceBudgetBackoff {
        SessionEvidenceBudgetBackoff::new(NonZeroU64::new(secs).expect("non-zero test window"))
    }

    #[test]
    fn suppresses_ticks_inside_the_window_and_reports_its_end() {
        let backoff = window(600);
        let exceeded = SessionEvidenceBudgetExceeded {
            observed_at_secs: 1_000,
        };

        assert_eq!(
            backoff.gate(exceeded, 1_000),
            SessionEvidenceBudgetGate::Suppressed { until_secs: 1_600 }
        );
        assert_eq!(
            backoff.gate(exceeded, 1_599),
            SessionEvidenceBudgetGate::Suppressed { until_secs: 1_600 }
        );
    }

    #[test]
    fn permits_one_attempt_once_the_window_elapses() {
        let backoff = window(600);
        let exceeded = SessionEvidenceBudgetExceeded {
            observed_at_secs: 1_000,
        };

        assert_eq!(
            backoff.gate(exceeded, 1_600),
            SessionEvidenceBudgetGate::AttemptPermitted
        );
        assert_eq!(
            backoff.gate(exceeded, 5_000),
            SessionEvidenceBudgetGate::AttemptPermitted
        );
    }

    #[test]
    fn re_anchoring_on_a_later_attempt_restarts_the_window() {
        let backoff = window(600);
        let re_anchored = SessionEvidenceBudgetExceeded {
            observed_at_secs: 1_600,
        };

        assert_eq!(
            backoff.gate(re_anchored, 1_660),
            SessionEvidenceBudgetGate::Suppressed { until_secs: 2_200 }
        );
    }

    #[test]
    fn zero_windows_are_unrepresentable_and_the_minimum_window_still_suppresses() {
        // `new(0)` cannot exist: the constructor only accepts NonZeroU64.
        assert!(NonZeroU64::new(0).is_none());

        // The smallest representable window still suppresses the tick that
        // observed the exhaustion instead of degenerating into "always try".
        let backoff = window(1);
        let exceeded = SessionEvidenceBudgetExceeded {
            observed_at_secs: 1_000,
        };
        assert_eq!(
            backoff.gate(exceeded, 1_000),
            SessionEvidenceBudgetGate::Suppressed { until_secs: 1_001 }
        );
        assert_eq!(
            backoff.gate(exceeded, 1_001),
            SessionEvidenceBudgetGate::AttemptPermitted
        );
    }

    #[test]
    fn exhausted_and_suppressed_labels_are_distinct_typed_states() {
        assert_ne!(
            SESSION_EVIDENCE_BUDGET_EXHAUSTED, SESSION_EVIDENCE_BUDGET_SUPPRESSED,
            "a suppressed tick must never present as a fresh exhausted attempt"
        );
    }

    #[test]
    fn window_arithmetic_saturates_instead_of_wrapping() {
        let backoff = window(u64::MAX);
        let exceeded = SessionEvidenceBudgetExceeded {
            observed_at_secs: i64::MAX - 1,
        };

        assert_eq!(
            backoff.gate(exceeded, i64::MAX - 1),
            SessionEvidenceBudgetGate::Suppressed {
                until_secs: i64::MAX
            }
        );
        assert_eq!(
            backoff.gate(exceeded, i64::MAX),
            SessionEvidenceBudgetGate::AttemptPermitted
        );
    }

    #[test]
    fn default_window_is_one_hour() {
        assert_eq!(
            SessionEvidenceBudgetBackoff::default().suppression_secs(),
            3_600
        );
    }
}
