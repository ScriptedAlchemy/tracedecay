//! Shared decision logic for bounded host-admission replay workers.
//!
//! Both the per-profile and per-project replay workers run one bounded pass at
//! a time, evaluate progress against the pending spool, and apply the same
//! backoff schedule. The pass classification and backoff curve live here; each
//! worker keeps its own scope-specific loop, cancellation, and eviction.

use std::time::Duration;

use super::HostAdmissionOutcome;

const MAX_BACKOFF: Duration = Duration::from_secs(2);
const INITIAL_BACKOFF: Duration = Duration::from_millis(25);

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
pub(crate) enum ReplayPassDecision {
    /// The spool shrank and more work remains — yield and re-run immediately.
    ProgressPending,
    /// No progress but the outcome is retryable — apply bounded backoff.
    Backoff,
    /// Terminal disposition — log and stop until the next external kick.
    Stop,
    /// Re-evaluate the work condition without backoff.
    Requeue,
}

/// Classify one replay pass from its pending-count delta and outcome.
pub(crate) fn classify_replay_pass(
    pending_before: usize,
    pending_after: usize,
    outcome: &HostAdmissionOutcome,
) -> ReplayPassDecision {
    let made_progress = pending_after < pending_before;
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
