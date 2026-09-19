//! Shared decision logic for bounded host-admission replay workers.
//!
//! Both the per-profile and per-project replay workers run one bounded pass at
//! a time, evaluate progress against the pending spool, and apply the same
//! backoff schedule. The pass classification and backoff curve live here; each
//! worker keeps its own scope-specific loop, cancellation, and eviction.

use std::time::Duration;

use tracedecay_sessions::admission::{HostAdmissionOutcome, HostAdmissionStatus};

const MAX_BACKOFF: Duration = Duration::from_secs(2);
const INITIAL_BACKOFF: Duration = Duration::from_millis(25);

/// Shared shift cap for profile and project host-admission replay workers.
/// Combined with [`INITIAL_BACKOFF`] this saturates at [`MAX_BACKOFF`] (2s).
pub const REPLAY_BACKOFF_SHIFT_CAP: u32 = 16;

/// The bounded backoff schedule: attempt 1 => 25ms, then doubles until the
/// per-worker `shift_cap` or the absolute [`MAX_BACKOFF`] ceiling.
pub fn replay_backoff(attempt: u32, shift_cap: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(shift_cap);
    let millis = INITIAL_BACKOFF
        .as_millis()
        .saturating_mul(1u128 << shift)
        .min(MAX_BACKOFF.as_millis());
    Duration::from_millis(millis as u64)
}

/// How a worker should proceed after one replay pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayPassDecision {
    /// The spool shrank and more work remains. Yield and re-run immediately.
    ProgressPending,
    /// No progress but the outcome is retryable. Apply bounded backoff.
    Backoff,
    /// Terminal disposition. Log and stop until the next external kick.
    Stop,
    /// Re-evaluate the work condition without backoff.
    Requeue,
    /// Closed `NotApplicable` left the spool unchanged. Stop until the next
    /// kick without backoff and without a failure log.
    TerminalNoop,
}

/// Classify one replay pass from its pending-count delta and outcome.
///
/// `NotApplicable` already closes the replay record. It must not enter the
/// retryable backoff arm, even when `is_replay_progress` is true and the
/// spool did not shrink: that arm is for work that may succeed later. A shrink
/// with records still pending continues immediately; a drained spool requeues;
/// an unchanged spool stops until the next external kick.
pub fn classify_replay_pass(
    pending_before: usize,
    pending_after: usize,
    outcome: &HostAdmissionOutcome,
) -> ReplayPassDecision {
    let made_progress = pending_after < pending_before;
    if outcome.status == HostAdmissionStatus::NotApplicable {
        if made_progress && pending_after > 0 {
            return ReplayPassDecision::ProgressPending;
        }
        if pending_after == 0 {
            return ReplayPassDecision::Requeue;
        }
        return ReplayPassDecision::TerminalNoop;
    }
    if made_progress && pending_after > 0 {
        ReplayPassDecision::ProgressPending
    } else if !made_progress
        && (outcome.retryable || (pending_after > 0 && outcome.status.is_replay_progress()))
    {
        ReplayPassDecision::Backoff
    } else if !outcome.status.is_replay_progress() {
        ReplayPassDecision::Stop
    } else {
        ReplayPassDecision::Requeue
    }
}

#[cfg(test)]
mod tests {
    use super::{ReplayPassDecision, classify_replay_pass};
    use tracedecay_sessions::admission::HostAdmissionOutcome;

    #[test]
    fn not_applicable_is_a_terminal_noop_unless_the_spool_actually_moves() {
        let closed = HostAdmissionOutcome::not_applicable("code_index_not_applicable");
        let mut flagged_retryable = closed.clone();
        flagged_retryable.retryable = true;

        assert_eq!(
            classify_replay_pass(2, 2, &closed),
            ReplayPassDecision::TerminalNoop
        );
        assert_eq!(
            classify_replay_pass(2, 3, &flagged_retryable),
            ReplayPassDecision::TerminalNoop
        );
        assert_eq!(
            classify_replay_pass(2, 1, &closed),
            ReplayPassDecision::ProgressPending
        );
        assert_eq!(
            classify_replay_pass(1, 0, &closed),
            ReplayPassDecision::Requeue
        );

        assert_eq!(
            classify_replay_pass(2, 2, &HostAdmissionOutcome::accepted_for_replay()),
            ReplayPassDecision::Backoff
        );
        assert_eq!(
            classify_replay_pass(2, 2, &HostAdmissionOutcome::spool_corrupted()),
            ReplayPassDecision::Stop
        );
    }
}
