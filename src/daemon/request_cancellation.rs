//! Daemon-generation request cancellation shared by socket and in-process invocations.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracedecay_runtime_core::cancellation::CancellationToken;

const PENDING_CAPACITY: usize = 1_024;
const PENDING_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
struct State {
    active: BTreeMap<String, CancellationToken>,
    pending: BTreeMap<String, Instant>,
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
    expire_pending(&mut state, Instant::now());
    if state.active.contains_key(request_id) {
        return None;
    }
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
    expire_pending(&mut state, now);
    if let Some(token) = state.active.get(request_id).cloned() {
        drop(state);
        token.cancel();
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

fn expire_pending(state: &mut State, now: Instant) {
    state
        .pending
        .retain(|_, admitted_at| now.saturating_duration_since(*admitted_at) < PENDING_TTL);
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{cancel, register, state};

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
}
