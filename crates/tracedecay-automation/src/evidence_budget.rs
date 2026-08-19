//! Typed accounting contract for session-evidence budget exhaustion.
//!
//! A retrieval attempt that exhausts its context/work budgets is a correct
//! terminal *skip* for the run that observed it: the evidence exists but
//! cannot be admitted inside the configured bounds, and re-running the same
//! bounded query on the next scheduler tick reproduces the exhaustion.
//! This module owns that state as a typed outcome so schedulers hold back
//! for a deterministic window instead of re-attempting (and re-reporting)
//! the exhausted retrieval on every tick.

/// Canonical ledger label for a session-evidence retrieval attempt that
/// exhausted its budgets. The label is minted where the retrieval outcome is
/// accepted and read back by the scheduler gate; both sides use this one
/// authority.
pub const SESSION_EVIDENCE_BUDGET_EXHAUSTED: &str = "session_evidence_budget_exhausted";

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
        Self::new(DEFAULT_SUPPRESSION_SECS)
    }
}

impl SessionEvidenceBudgetBackoff {
    #[must_use]
    pub const fn new(suppression_secs: u64) -> Self {
        Self { suppression_secs }
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
    use super::*;

    #[test]
    fn suppresses_ticks_inside_the_window_and_reports_its_end() {
        let backoff = SessionEvidenceBudgetBackoff::new(600);
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
        let backoff = SessionEvidenceBudgetBackoff::new(600);
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
        let backoff = SessionEvidenceBudgetBackoff::new(600);
        let re_anchored = SessionEvidenceBudgetExceeded {
            observed_at_secs: 1_600,
        };

        assert_eq!(
            backoff.gate(re_anchored, 1_660),
            SessionEvidenceBudgetGate::Suppressed { until_secs: 2_200 }
        );
    }

    #[test]
    fn window_arithmetic_saturates_instead_of_wrapping() {
        let backoff = SessionEvidenceBudgetBackoff::new(u64::MAX);
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
