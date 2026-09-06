//! Bounded wait for a single-writer file lock held by a sibling hook.
//!
//! Both hook spools admit one writer at a time through an advisory file lock,
//! and both acquire it with a non-blocking `try_lock`. A held lock is transient
//! by construction: the holder performs one bounded, already-durable append and
//! drops. Agents fire sibling callbacks concurrently — a single edit can wake
//! several host hooks at once — so refusing the first contended writer turned
//! an ordinary interleaving into a lost event and a non-zero hook exit the
//! agent shows its user. Wait the holder out instead; a genuinely stuck holder
//! still reports contention rather than stalling the callback past its
//! synchronous deadline.

use std::time::Duration;

/// ponytail: fixed backoff, 100ms worst case. Make it adaptive only if a real
/// workload shows hooks exhausting the budget.
const ATTEMPTS: u32 = 50;
const BACKOFF: Duration = Duration::from_millis(2);

/// Retries `attempt` while it reports lock contention, then returns its last
/// outcome. Any other error, and any success, returns immediately.
pub(crate) fn wait_out_contention<T, E>(
    mut attempt: impl FnMut() -> Result<T, E>,
    is_contended: impl Fn(&E) -> bool,
) -> Result<T, E> {
    let mut outcome = attempt();
    for _ in 1..ATTEMPTS {
        match &outcome {
            Err(error) if is_contended(error) => std::thread::sleep(BACKOFF),
            _ => return outcome,
        }
        outcome = attempt();
    }
    outcome
}
