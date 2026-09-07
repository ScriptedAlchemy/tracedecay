//! Request cancellation shared by socket and in-process invocations.
//!
//! One [`RequestCancellationRegistryV1`] is owned by each
//! [`DaemonInvocationService`](crate::DaemonInvocationService) instance. The
//! service's clones and both the root transport and executor paths call into
//! that exact table; they do not keep a second one. Independent compositions
//! in one process therefore own independent tables: registering, cancelling,
//! or tombstoning a request ID in one cannot be observed from another, and a
//! retired service takes its pending cancellations with it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
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

/// The cancellation table of one invocation service generation.
///
/// Clones share the same table. A [`Lease`] retains the table it was
/// registered in, so dropping it can only settle that owner's entry.
#[derive(Clone, Default)]
pub struct RequestCancellationRegistryV1 {
    state: Arc<Mutex<State>>,
}

pub struct Lease {
    state: Arc<Mutex<State>>,
    request_id: String,
    token: CancellationToken,
}

impl Lease {
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl RequestCancellationRegistryV1 {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn register(&self, request_id: &str) -> Option<Lease> {
        let token = CancellationToken::for_application_request(request_id);
        let mut state = self.lock();
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
            state: Arc::clone(&self.state),
            request_id: request_id.to_owned(),
            token,
        })
    }

    #[hotpath::measure(label = "daemon.invocation.cancel")]
    pub fn cancel(&self, request_id: &str) -> bool {
        let mut state = self.lock();
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
        let mut state = self
            .state
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

    use super::{COMPLETED_CAPACITY, RequestCancellationRegistryV1, State, record_completed};

    #[test]
    fn pre_registration_cancellation_is_retained_and_cleanup_is_exact() {
        let registry = RequestCancellationRegistryV1::default();
        assert!(!registry.cancel("request.git.pending"));
        let lease = registry
            .register("request.git.pending")
            .expect("request registers once");
        assert!(lease.token().is_cancelled());
        assert!(registry.cancel("request.git.pending"));
        drop(lease);

        let state = registry.lock();
        assert!(!state.active.contains_key("request.git.pending"));
        assert!(!state.pending.contains_key("request.git.pending"));
    }

    #[test]
    fn cancellation_after_completion_does_not_poison_same_id_retry() {
        let registry = RequestCancellationRegistryV1::default();
        let request_id = "request.completed-before-cancel";
        let completed = registry
            .register(request_id)
            .expect("initial request registers");
        drop(completed);

        assert!(
            registry.cancel(request_id),
            "a late cancellation is acknowledged against the completed request"
        );
        let retried = registry
            .register(request_id)
            .expect("same-id retry registers");
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

    /// Two owners are two tables: the same request ID registers in both, and
    /// cancelling or completing it in one is invisible to the other.
    #[test]
    fn independent_registries_do_not_share_registrations_or_cancellations() {
        let request_id = "request.shared-id";
        let first = RequestCancellationRegistryV1::default();
        let second = RequestCancellationRegistryV1::default();

        let first_lease = first.register(request_id).expect("first owner registers");
        let second_lease = second
            .register(request_id)
            .expect("an identical request ID registers in an independent owner");

        assert!(first.cancel(request_id));
        assert!(first_lease.token().is_cancelled());
        assert!(
            !second_lease.token().is_cancelled(),
            "cancelling in one owner must not reach the other owner's request"
        );

        drop(first_lease);
        assert!(
            first.cancel(request_id),
            "the first owner tombstones its own completed request"
        );
        assert!(
            second.lock().active.contains_key(request_id),
            "completion in one owner must not remove the other owner's live registration"
        );
        drop(second_lease);
    }

    /// A pending cancellation belongs to the owner that received it: a new
    /// generation registering the same ID starts uncancelled.
    #[test]
    fn pending_cancellation_does_not_leak_into_another_registry() {
        let request_id = "request.pending-across-generations";
        let retired = RequestCancellationRegistryV1::default();
        assert!(!retired.cancel(request_id));
        drop(retired);

        let replacement = RequestCancellationRegistryV1::default();
        let lease = replacement
            .register(request_id)
            .expect("replacement owner registers");
        assert!(
            !lease.token().is_cancelled(),
            "a cancellation received by a retired owner must not pre-cancel a new generation"
        );
    }

    /// Clones of one owner are one table: they refuse a simultaneous duplicate
    /// and observe the same early cancellation.
    #[test]
    fn registry_clones_share_one_table() {
        let request_id = "request.shared-owner";
        let owner = RequestCancellationRegistryV1::default();
        let adapter = owner.clone();

        assert!(!adapter.cancel(request_id));
        let lease = owner
            .register(request_id)
            .expect("the owner registers the request");
        assert!(
            lease.token().is_cancelled(),
            "an early cancellation through a clone reaches the registration"
        );
        assert!(
            adapter.register(request_id).is_none(),
            "a clone must refuse a simultaneous duplicate registration"
        );
        drop(lease);
        assert!(
            adapter.register(request_id).is_some(),
            "after completion the same ID registers again"
        );
    }

    /// Dropping a stale lease cannot remove the token that replaced it.
    #[test]
    fn stale_lease_drop_leaves_the_newer_registration_in_place() {
        let request_id = "request.stale-lease";
        let registry = RequestCancellationRegistryV1::default();
        let stale = registry.register(request_id).expect("first registration");
        let stale_token = stale.token();
        {
            // Settle the first registration without dropping its lease so the
            // ID can be registered again while the stale lease still exists.
            let mut state = registry.lock();
            state.active.remove(request_id);
        }
        let newer = registry
            .register(request_id)
            .expect("the ID registers again once the first entry is settled");

        drop(stale);

        assert!(
            registry.lock().active.contains_key(request_id),
            "a stale lease must not remove the newer token"
        );
        assert!(registry.cancel(request_id));
        assert!(newer.token().is_cancelled());
        assert!(!stale_token.is_cancelled());
    }
}
