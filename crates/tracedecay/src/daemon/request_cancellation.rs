//! Daemon-generation request cancellation shared by socket and in-process invocations.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracedecay_runtime_core::cancellation::CancellationToken;

const PENDING_CAPACITY: usize = 1_024;
const PENDING_TTL: Duration = Duration::from_mins(1);
const COMPLETED_CAPACITY: usize = 1_024;
const COMPLETED_TTL: Duration = Duration::from_mins(1);

#[derive(Default)]
struct State {
    active: BTreeMap<String, CancellationToken>,
    pending: BTreeMap<String, Instant>,
    completed: BTreeMap<String, Instant>,
}

pub(super) struct Lease {
    request_id: String,
    token: CancellationToken,
}

impl Lease {
    pub(super) fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

pub(super) fn register(request_id: &str) -> Option<Lease> {
    let token = CancellationToken::for_application_request(request_id);
    let mut state = state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    expire_ephemeral(&mut state, Instant::now());
    if state.active.contains_key(request_id) {
        return None;
    }
    // Request IDs are daemon-generation-unique. Removing the tombstone makes
    // an explicit retry possible without allowing a late cancellation for the
    // completed invocation to poison that retry before it registers.
    state.completed.remove(request_id);
    if state.pending.remove(request_id).is_some() {
        token.cancel();
    }
    state.active.insert(request_id.to_owned(), token.clone());
    Some(Lease {
        request_id: request_id.to_owned(),
        token,
    })
}

pub(super) fn cancel(request_id: &str) -> bool {
    let mut state = state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    expire_ephemeral(&mut state, now);
    if let Some(token) = state.active.get(request_id).cloned() {
        drop(state);
        token.cancel();
        true
    } else if state.completed.contains_key(request_id) {
        true
    } else {
        if state.pending.len() >= PENDING_CAPACITY {
            return false;
        }
        state.pending.insert(request_id.to_owned(), now);
        false
    }
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

fn expire_ephemeral(state: &mut State, now: Instant) {
    state
        .pending
        .retain(|_, admitted_at| now.saturating_duration_since(*admitted_at) < PENDING_TTL);
    state
        .completed
        .retain(|_, completed_at| now.saturating_duration_since(*completed_at) < COMPLETED_TTL);
}

fn record_completed(state: &mut State, request_id: String, now: Instant) {
    if state.completed.len() >= COMPLETED_CAPACITY
        && let Some(oldest) = state
            .completed
            .iter()
            .min_by_key(|(_, completed_at)| **completed_at)
            .map(|(request_id, _)| request_id.clone())
    {
        state.completed.remove(&oldest);
    }
    state.completed.insert(request_id, now);
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .active
            .get(&self.request_id)
            .is_some_and(|token| token.is_same_token(&self.token))
        {
            state.active.remove(&self.request_id);
            record_completed(&mut state, self.request_id.clone(), Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{COMPLETED_CAPACITY, State, cancel, record_completed, register, state};

    #[test]
    fn pre_registration_cancellation_is_retained_and_cleanup_is_exact() {
        assert!(!cancel("request.git.pending"));
        let lease = register("request.git.pending").expect("request registers once");
        assert!(lease.token().is_cancelled());
        assert!(cancel("request.git.pending"));
        drop(lease);

        let state = state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(!state.active.contains_key("request.git.pending"));
        assert!(!state.pending.contains_key("request.git.pending"));
    }

    #[test]
    fn cancellation_after_completion_does_not_poison_same_id_retry() {
        let request_id = "request.completed-before-cancel";
        let completed = register(request_id).expect("initial request registers");
        drop(completed);

        assert!(
            cancel(request_id),
            "a late cancellation is acknowledged against the completed request"
        );
        let retried = register(request_id).expect("same-id retry registers");
        assert!(
            !retried.token().is_cancelled(),
            "the late cancellation must not become a pending cancellation for the retry"
        );
        drop(retried);
    }

    #[test]
    fn completed_tombstones_evict_the_oldest_at_capacity() {
        let mut state = State::default();
        let started = Instant::now();
        for ordinal in 0..COMPLETED_CAPACITY {
            record_completed(
                &mut state,
                format!("request.completed.{ordinal}"),
                started + Duration::from_millis(u64::try_from(ordinal).expect("ordinal")),
            );
        }
        record_completed(
            &mut state,
            "request.completed.newest".to_owned(),
            started + Duration::from_secs(10),
        );

        assert_eq!(state.completed.len(), COMPLETED_CAPACITY);
        assert!(!state.completed.contains_key("request.completed.0"));
        assert!(state.completed.contains_key("request.completed.newest"));
    }
}
