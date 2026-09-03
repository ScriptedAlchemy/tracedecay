//! Bounded embedding session pool
//! (Plan 31, `docs/plans/tracedecay-v2/31-native-fastembed-semantic-code-search.md`).
//!
//! Sessions are keyed by the complete
//! projection/privacy identity (Plan 31: "bounded sessions keyed by the
//! complete projection/privacy identity"; "Compatible warmed sessions are
//! pooled under bounded memory, concurrency, idle, and cancellation
//! policy"). The pool enforces:
//!
//! - a hard session bound with typed exhaustion errors (no silent blocking),
//! - FIFO-fair bounded waiting with cancellation and injected-clock
//!   deadlines,
//! - idle reaping driven by an injected clock (never wall time in tests),
//! - a memory ceiling over estimated resident session bytes,
//! - strict identity separation: a session warmed for one projection key,
//!   privacy domain, or key epoch never serves another.
//!
//! It depends only on the domain projection contract plus its sibling
//! `fastembed_adapter` port surface. Deadlines are `Duration` values against
//! the injected clock, bridged from query `RetrievalBudget`.
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SendError, TryRecvError, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use tracedecay_domain::{AdmittedEmbeddingProjectionKeyV1, PrivacyDomainId, ProjectionKeyV1};
use tracedecay_runtime_core::resident_memory::sampled_process_resident_bytes_v1;

use super::fastembed_adapter::{
    AdmittedProjectionArtifactV1, EmbedError, EmbeddingRuntime, EmbeddingSession,
    RuntimeFailureKindV1, RuntimeFailureV1, SemanticExecutionAuthority,
    SemanticExecutionInterruptionV1,
};

mod clock;
#[cfg(test)]
mod cold_load_tests;
pub use clock::{ManualClock, MonotonicClock, SystemMonotonicClock};

const WAITER_WAKEUP_INTERVAL: Duration = Duration::from_millis(5);

/// How often an in-flight cold model open is checked against its load
/// deadline and the actual-resident bound. Sampling only happens while a
/// cold open is executing; warm acquisitions never read the kernel surface.
const COLD_LOAD_OBSERVATION_INTERVAL: Duration = Duration::from_millis(100);

/// Process-resident sampler consulted while a cold model open is in flight.
/// Production uses the canonical kernel sampler
/// ([`sampled_process_resident_bytes_v1`]); tests inject scripted series.
/// `None` is a typed abstention (no measurement surface), never zero.
pub type ResidentBytesSamplerV1 = Arc<dyn Fn() -> Option<u64> + Send + Sync>;

const LOAD_INTERRUPTION_NONE: u8 = 0;
const LOAD_INTERRUPTION_CANCELLED: u8 = 1;
const LOAD_INTERRUPTION_DEADLINE: u8 = 2;

/// Pool-owned interruption signal handed to each cold-load `open_session`.
/// The acquisition side fires it when the load deadline elapses or when the
/// measured resident bound is breached; the loader thread observes it at its
/// next stage boundary and abandons the open with a typed error instead of
/// holding the load's memory until the runtime finishes on its own.
#[derive(Debug, Default)]
struct LoadInterruptionSignalV1 {
    state: AtomicU8,
}

impl LoadInterruptionSignalV1 {
    /// Record the first interruption; later signals keep the original cause.
    fn fire(&self, interruption: SemanticExecutionInterruptionV1) {
        let value = match interruption {
            SemanticExecutionInterruptionV1::Cancelled => LOAD_INTERRUPTION_CANCELLED,
            SemanticExecutionInterruptionV1::DeadlineExceeded => LOAD_INTERRUPTION_DEADLINE,
        };
        let _ = self.state.compare_exchange(
            LOAD_INTERRUPTION_NONE,
            value,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

impl SemanticExecutionAuthority for LoadInterruptionSignalV1 {
    fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
        match self.state.load(Ordering::SeqCst) {
            LOAD_INTERRUPTION_CANCELLED => Some(SemanticExecutionInterruptionV1::Cancelled),
            LOAD_INTERRUPTION_DEADLINE => Some(SemanticExecutionInterruptionV1::DeadlineExceeded),
            _ => None,
        }
    }
}

fn duration_micros(duration: Duration) -> Option<u64> {
    u64::try_from(duration.as_micros()).ok()
}

fn session_acquire_failed(error: SessionAcquireError) -> SessionAcquireError {
    crate::hotpath_observe::record_session_error(&error);
    error
}

/// The complete projection/privacy identity of a warmed session. It can only
/// be created from the domain's admitted embedding projection key, so a
/// projection, privacy-domain, or key-epoch change produces zero cache hits.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionIdentityV1 {
    admitted_projection: AdmittedEmbeddingProjectionKeyV1,
}

impl SessionIdentityV1 {
    fn from_authority(authority: &AdmittedProjectionArtifactV1) -> Self {
        Self {
            admitted_projection: authority.projection().clone(),
        }
    }

    pub fn projection_key(&self) -> &ProjectionKeyV1 {
        self.admitted_projection.projection_key()
    }

    pub fn privacy_domain(&self) -> &PrivacyDomainId {
        self.admitted_projection.privacy_domain()
    }

    pub fn privacy_key_epoch(&self) -> u64 {
        self.admitted_projection.privacy_key_epoch()
    }
}

/// Pool resource policy (Plan 31: bounded memory, concurrency, idle, and
/// cancellation policy; the complete resource ceiling comes from the
/// manifest).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPoolConfigV1 {
    /// Maximum concurrently checked-out sessions across all identities.
    pub max_sessions: usize,
    /// Maximum callers allowed to wait in [`SessionPool::acquire_blocking`];
    /// additional waiters fail with a typed `QueueFull` error instead of
    /// silently blocking.
    pub max_queued_waiters: usize,
    /// Idle sessions older than this are reaped. `Duration::ZERO` reaps a
    /// session as soon as it is released.
    pub idle_timeout: Duration,
    /// Ceiling over the summed resident-byte estimates of every live
    /// (active + idle) session.
    pub memory_ceiling_bytes: u64,
}

impl SessionPoolConfigV1 {
    pub fn validate(&self) -> Result<(), SessionPoolConfigError> {
        if self.max_sessions == 0 {
            return Err(SessionPoolConfigError::ZeroMaxSessions);
        }
        if self.memory_ceiling_bytes == 0 {
            return Err(SessionPoolConfigError::ZeroMemoryCeiling);
        }
        Ok(())
    }
}

/// Typed configuration failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPoolConfigError {
    ZeroMaxSessions,
    ZeroMemoryCeiling,
}

impl fmt::Display for SessionPoolConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroMaxSessions => write!(f, "max_sessions must be at least 1"),
            Self::ZeroMemoryCeiling => {
                write!(f, "memory_ceiling_bytes must be at least 1")
            }
        }
    }
}

impl Error for SessionPoolConfigError {}

/// Typed acquisition failure (Plan 31: typed exhaustion errors; no silent
/// blocking, no silent substitution).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionAcquireError {
    /// Every session slot is checked out.
    Exhausted { active: usize, max: usize },
    /// The bounded waiter queue is full.
    QueueFull { queued: usize, max: usize },
    /// Opening one more session would exceed the memory ceiling.
    MemoryCeilingExceeded {
        used_bytes: u64,
        requested_bytes: u64,
        ceiling_bytes: u64,
    },
    /// The caller's cancellation signal fired while waiting.
    Cancelled,
    /// The caller's wait budget elapsed while waiting.
    DeadlineExceeded { waited: Duration, budget: Duration },
    /// A cold model session finished opening after the artifact's admitted
    /// load deadline and was discarded before it could enter the pool.
    LoadDeadlineExceeded {
        elapsed: Duration,
        deadline: Duration,
    },
    /// Measured process resident growth during a cold model open exceeded the
    /// artifact's declared resident-byte ceiling. The load was signalled to
    /// abort; its slot and byte reservation release when the runtime returns.
    ResidentCeilingExceeded {
        tracked_resident_bytes: u64,
        observed_growth_bytes: u64,
        ceiling_bytes: u64,
    },
    /// The runtime failed to open a new session.
    Open(EmbedError),
    /// The pool has been closed.
    Closed,
}

impl fmt::Display for SessionAcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exhausted { active, max } => {
                write!(f, "session pool exhausted: {active}/{max} sessions active")
            }
            Self::QueueFull { queued, max } => {
                write!(f, "session waiter queue full: {queued}/{max} waiters")
            }
            Self::MemoryCeilingExceeded {
                used_bytes,
                requested_bytes,
                ceiling_bytes,
            } => write!(
                f,
                "session memory ceiling exceeded: {used_bytes} used + {requested_bytes} requested > {ceiling_bytes} ceiling"
            ),
            Self::Cancelled => write!(f, "session acquisition cancelled"),
            Self::DeadlineExceeded { waited, budget } => write!(
                f,
                "session acquisition deadline exceeded: waited {waited:?} of {budget:?} budget"
            ),
            Self::LoadDeadlineExceeded { elapsed, deadline } => write!(
                f,
                "cold session load deadline exceeded: elapsed {elapsed:?} exceeds {deadline:?}"
            ),
            Self::ResidentCeilingExceeded {
                tracked_resident_bytes,
                observed_growth_bytes,
                ceiling_bytes,
            } => write!(
                f,
                "cold session load resident ceiling exceeded: {tracked_resident_bytes} tracked bytes + {observed_growth_bytes} observed growth > {ceiling_bytes} byte ceiling"
            ),
            Self::Open(err) => write!(f, "failed to open session: {err}"),
            Self::Closed => write!(f, "session pool is closed"),
        }
    }
}

impl Error for SessionAcquireError {}

/// Point-in-time pool telemetry (counts and bytes only; no session content).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionPoolStats {
    pub active: usize,
    pub idle: usize,
    pub live_sessions: usize,
    pub queued_waiters: usize,
    pub resident_bytes: u64,
    pub sessions_opened: usize,
    pub sessions_closed: usize,
    pub sessions_reaped: usize,
    /// Most recent completed runtime model/session open, including an open
    /// discarded for exceeding its deadline.
    pub last_cold_load_micros: Option<u64>,
    pub closed: bool,
}

struct IdleEntry<S> {
    session: S,
    released_at: Duration,
    resident_bytes: u64,
}

struct PoolState<S> {
    idle: HashMap<SessionIdentityV1, Vec<IdleEntry<S>>>,
    active: usize,
    waiters: VecDeque<u64>,
    next_waiter_id: u64,
    availability_epoch: u64,
    resident_bytes: u64,
    sessions_opened: usize,
    sessions_closed: usize,
    sessions_reaped: usize,
    last_cold_load_micros: Option<u64>,
    closed: bool,
}

impl<S> Default for PoolState<S> {
    fn default() -> Self {
        Self {
            idle: HashMap::new(),
            active: 0,
            waiters: VecDeque::new(),
            next_waiter_id: 0,
            availability_epoch: 0,
            resident_bytes: 0,
            sessions_opened: 0,
            sessions_closed: 0,
            sessions_reaped: 0,
            last_cold_load_micros: None,
            closed: false,
        }
    }
}

impl<S> PoolState<S> {
    fn idle_sessions(&self) -> usize {
        self.idle.values().map(Vec::len).sum()
    }

    fn live_sessions(&self) -> usize {
        self.active.saturating_add(self.idle_sessions())
    }

    fn next_waiter_id(&mut self) -> u64 {
        let waiter_id = self.next_waiter_id;
        self.next_waiter_id = self.next_waiter_id.wrapping_add(1);
        waiter_id
    }

    fn allows_acquire(&self, waiter_id: Option<u64>) -> bool {
        match self.waiters.front() {
            Some(front) => waiter_id == Some(*front),
            None => true,
        }
    }

    fn mark_availability_changed(&mut self) {
        self.availability_epoch = self.availability_epoch.wrapping_add(1);
    }
}

struct PoolInner<R: EmbeddingRuntime, C: MonotonicClock> {
    runtime: R,
    clock: C,
    config: SessionPoolConfigV1,
    resident_sampler: ResidentBytesSamplerV1,
    state: Mutex<PoolState<R::Session>>,
    wakeups: Condvar,
}

impl<R: EmbeddingRuntime, C: MonotonicClock> PoolInner<R, C> {
    fn lock_state(&self) -> MutexGuard<'_, PoolState<R::Session>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Bounded pool of warmed embedding sessions over one [`EmbeddingRuntime`]
/// (Plan 31: cold and warm sessions, OOM, cancellation, and offline startup
/// are exercised against this pool with the deterministic fake runtime).
pub struct SessionPool<R: EmbeddingRuntime, C: MonotonicClock> {
    inner: Arc<PoolInner<R, C>>,
}

impl<R: EmbeddingRuntime, C: MonotonicClock> Clone for SessionPool<R, C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<R, C> SessionPool<R, C>
where
    R: EmbeddingRuntime + Send + Sync + 'static,
    R::Session: 'static,
    C: MonotonicClock + 'static,
{
    pub fn new(
        runtime: R,
        clock: C,
        config: SessionPoolConfigV1,
    ) -> Result<Self, SessionPoolConfigError> {
        Self::with_resident_sampler(
            runtime,
            clock,
            config,
            Arc::new(sampled_process_resident_bytes_v1),
        )
    }

    /// [`Self::new`] with an injected process-resident sampler, so tests can
    /// drive the cold-load resident bound with a scripted RSS series instead
    /// of the kernel surface.
    pub fn with_resident_sampler(
        runtime: R,
        clock: C,
        config: SessionPoolConfigV1,
        resident_sampler: ResidentBytesSamplerV1,
    ) -> Result<Self, SessionPoolConfigError> {
        config.validate()?;
        Ok(Self {
            inner: Arc::new(PoolInner {
                runtime,
                clock,
                config,
                resident_sampler,
                state: Mutex::new(PoolState::default()),
                wakeups: Condvar::new(),
            }),
        })
    }

    /// Non-blocking acquisition. Reuses an idle session with an exactly
    /// matching identity or opens a new one within the bounds; otherwise
    /// fails with a typed error. Never blocks and never substitutes a
    /// session from another identity.
    #[hotpath::measure(label = "semantic.session_pool.acquire")]
    pub fn acquire(
        &self,
        authority: &AdmittedProjectionArtifactV1,
    ) -> Result<PooledSession<R, C>, SessionAcquireError> {
        self.verify_artifact(authority)?;
        self.acquire_verified(authority, None)
            .map_err(session_acquire_failed)
    }

    fn verify_artifact(
        &self,
        authority: &AdmittedProjectionArtifactV1,
    ) -> Result<(), SessionAcquireError> {
        self.inner
            .runtime
            .verify_artifact_compatibility(authority)
            .map_err(SessionAcquireError::Open)
    }

    fn acquire_verified(
        &self,
        authority: &AdmittedProjectionArtifactV1,
        waiter_id: Option<u64>,
    ) -> Result<PooledSession<R, C>, SessionAcquireError> {
        let identity = SessionIdentityV1::from_authority(authority);
        let now = self.inner.clock.now();
        let mut state = self.inner.lock_state();
        if state.closed {
            return Err(SessionAcquireError::Closed);
        }
        if !state.allows_acquire(waiter_id) {
            let active = state.active;
            drop(state);
            return Err(SessionAcquireError::Exhausted {
                active,
                max: self.inner.config.max_sessions,
            });
        }
        let reusable = state.idle.get_mut(&identity).and_then(|entries| {
            let index = entries
                .iter()
                .rposition(|entry| entry.session.authority() == authority)?;
            Some(entries.swap_remove(index))
        });
        if let Some(entry) = reusable {
            let reaped = reap_expired_locked(&mut state, now, self.inner.config.idle_timeout);
            if reaped != 0 {
                state.mark_availability_changed();
            }
            state.active += 1;
            drop(state);
            if reaped != 0 {
                self.inner.wakeups.notify_all();
            }
            crate::hotpath_observe::record_session_acquire("warm");
            return Ok(self.make_guard(identity, entry.session, entry.resident_bytes));
        }
        // A request for the exact admitted projection is the authority that
        // makes its idle session useful again. Reaping that session first and
        // immediately reopening the same model turns every post-idle query
        // into a cold model load. Only reclaim expired sessions after proving
        // none can serve this acquisition; explicit maintenance may still
        // call `reap_idle` when it needs to release all expired ownership.
        let reaped = reap_expired_locked(&mut state, now, self.inner.config.idle_timeout);
        if reaped != 0 {
            state.mark_availability_changed();
        }
        if state.live_sessions() >= self.inner.config.max_sessions {
            let active = state.active;
            drop(state);
            if reaped != 0 {
                self.inner.wakeups.notify_all();
            }
            return Err(SessionAcquireError::Exhausted {
                active,
                max: self.inner.config.max_sessions,
            });
        }
        let tracked_resident_before_open = state.resident_bytes;
        // Reserve both the slot and a conservative resident-byte bound before
        // opening. FastEmbed model loading is itself memory-intensive, so a
        // post-open check would allow concurrent opens to transiently exceed
        // the configured ceiling.
        let reserved_bytes = self.inner.runtime.resident_bytes_reservation(authority);
        let projected_resident = state.resident_bytes.checked_add(reserved_bytes);
        let violated_ceiling = if reserved_bytes > authority.resident_byte_ceiling() {
            Some(authority.resident_byte_ceiling())
        } else if projected_resident
            .is_none_or(|bytes| bytes > self.inner.config.memory_ceiling_bytes)
        {
            Some(self.inner.config.memory_ceiling_bytes)
        } else {
            None
        };
        if let Some(ceiling_bytes) = violated_ceiling {
            let used_bytes = state.resident_bytes;
            drop(state);
            if reaped != 0 {
                self.inner.wakeups.notify_all();
            }
            return Err(SessionAcquireError::MemoryCeilingExceeded {
                used_bytes,
                requested_bytes: reserved_bytes,
                ceiling_bytes,
            });
        }
        state.active += 1;
        state.resident_bytes =
            projected_resident.unwrap_or_else(|| panic!("resident reservation checked above"));
        drop(state);

        let load_started = self.inner.clock.now();
        let load_deadline = Duration::from_millis(authority.load_deadline_ms());
        let session = self.open_session_bounded(
            authority,
            load_deadline,
            reserved_bytes,
            tracked_resident_before_open,
        )?;
        // Injected-clock recheck after a bounded open: a manual test clock can
        // report a longer load than the wall-time bound observed, and the
        // deadline verdict must follow the injected clock in that case too.
        let load_elapsed = self.inner.clock.now().saturating_sub(load_started);
        if load_elapsed > load_deadline {
            let mut state = self.inner.lock_state();
            state.active -= 1;
            state.resident_bytes = state.resident_bytes.saturating_sub(reserved_bytes);
            state.sessions_opened += 1;
            state.sessions_closed += 1;
            state.last_cold_load_micros = duration_micros(load_elapsed);
            state.mark_availability_changed();
            drop(state);
            self.inner.wakeups.notify_all();
            drop(session);
            return Err(SessionAcquireError::LoadDeadlineExceeded {
                elapsed: load_elapsed,
                deadline: load_deadline,
            });
        }
        let resident_bytes = session.resident_bytes_estimate();
        let mut state = self.inner.lock_state();
        if state.closed {
            state.active -= 1;
            state.resident_bytes = state.resident_bytes.saturating_sub(reserved_bytes);
            state.sessions_opened += 1;
            state.sessions_closed += 1;
            state.mark_availability_changed();
            drop(state);
            self.inner.wakeups.notify_all();
            drop(session);
            return Err(SessionAcquireError::Closed);
        }
        let resident_without_reservation = state.resident_bytes.saturating_sub(reserved_bytes);
        let projected_resident = resident_without_reservation.checked_add(resident_bytes);
        let violated_ceiling = if resident_bytes > reserved_bytes {
            Some(reserved_bytes)
        } else {
            projected_resident
                .is_none_or(|bytes| bytes > self.inner.config.memory_ceiling_bytes)
                .then_some(self.inner.config.memory_ceiling_bytes)
        };
        if let Some(ceiling_bytes) = violated_ceiling {
            let used = resident_without_reservation;
            state.active -= 1;
            state.resident_bytes = resident_without_reservation;
            state.sessions_opened += 1;
            state.sessions_closed += 1;
            state.mark_availability_changed();
            drop(state);
            self.inner.wakeups.notify_all();
            drop(session);
            return Err(SessionAcquireError::MemoryCeilingExceeded {
                used_bytes: used,
                requested_bytes: resident_bytes,
                ceiling_bytes,
            });
        }
        state.resident_bytes =
            projected_resident.unwrap_or_else(|| panic!("resident total checked above"));
        state.sessions_opened += 1;
        state.last_cold_load_micros = duration_micros(load_elapsed);
        crate::hotpath_observe::record_session_acquire("cold");
        Ok(self.make_guard(identity, session, resident_bytes))
    }

    /// Bounded blocking acquisition with FIFO-fair waiter accounting.
    /// Waits on resource, cancellation, and deadline wakeups until the caller
    /// reaches the head of the FIFO queue and a resource is available.
    #[hotpath::measure(label = "semantic.session_pool.acquire_blocking")]
    pub fn acquire_blocking(
        &self,
        authority: &AdmittedProjectionArtifactV1,
        budget: Duration,
        cancel: &dyn SemanticExecutionAuthority,
    ) -> Result<PooledSession<R, C>, SessionAcquireError> {
        match cancel.interruption() {
            Some(SemanticExecutionInterruptionV1::Cancelled) => {
                return Err(session_acquire_failed(SessionAcquireError::Cancelled));
            }
            Some(SemanticExecutionInterruptionV1::DeadlineExceeded) => {
                return Err(session_acquire_failed(
                    SessionAcquireError::DeadlineExceeded {
                        waited: Duration::ZERO,
                        budget,
                    },
                ));
            }
            None => {}
        }
        let start = self.inner.clock.now();
        let (waiter_id, mut observed_epoch) = {
            let mut state = self.inner.lock_state();
            if state.closed {
                return Err(session_acquire_failed(SessionAcquireError::Closed));
            }
            if state.waiters.len() >= self.inner.config.max_queued_waiters {
                return Err(session_acquire_failed(SessionAcquireError::QueueFull {
                    queued: state.waiters.len(),
                    max: self.inner.config.max_queued_waiters,
                }));
            }
            let waiter_id = state.next_waiter_id();
            state.waiters.push_back(waiter_id);
            hotpath::gauge!("semantic_session_waiters").set(state.waiters.len());
            (waiter_id, state.availability_epoch.wrapping_sub(1))
        };
        let _permit = WaiterPermit {
            inner: Arc::clone(&self.inner),
            waiter_id,
        };
        self.verify_artifact(authority)
            .map_err(session_acquire_failed)?;
        loop {
            let waited = self.inner.clock.now().saturating_sub(start);
            match cancel.interruption() {
                Some(SemanticExecutionInterruptionV1::Cancelled) => {
                    return Err(session_acquire_failed(SessionAcquireError::Cancelled));
                }
                Some(SemanticExecutionInterruptionV1::DeadlineExceeded) => {
                    return Err(session_acquire_failed(
                        SessionAcquireError::DeadlineExceeded { waited, budget },
                    ));
                }
                None => {}
            }
            if waited >= budget {
                return Err(session_acquire_failed(
                    SessionAcquireError::DeadlineExceeded { waited, budget },
                ));
            }

            let should_attempt = {
                let state = self.inner.lock_state();
                if state.closed {
                    return Err(session_acquire_failed(SessionAcquireError::Closed));
                }
                let attempt = state.allows_acquire(Some(waiter_id))
                    && state.availability_epoch != observed_epoch;
                if attempt {
                    // Capture the epoch BEFORE the attempt, under the same lock
                    // that admitted it. A release that lands while the attempt
                    // is in flight bumps the epoch past this value, so a
                    // retryable failure re-attempts immediately instead of
                    // waiting on an epoch whose availability signal it already
                    // consumed (that lost wakeup deadlocked waiters whose
                    // deadline clock never advances).
                    observed_epoch = state.availability_epoch;
                }
                attempt
            };
            if should_attempt {
                match self.acquire_verified(authority, Some(waiter_id)) {
                    Ok(guard) => return Ok(guard),
                    Err(
                        permanent @ SessionAcquireError::MemoryCeilingExceeded {
                            requested_bytes,
                            ceiling_bytes,
                            ..
                        },
                    ) if requested_bytes > ceiling_bytes => {
                        return Err(session_acquire_failed(permanent));
                    }
                    Err(
                        retryable @ (SessionAcquireError::Exhausted { .. }
                        | SessionAcquireError::MemoryCeilingExceeded { .. }),
                    ) => {
                        drop(retryable);
                        continue;
                    }
                    Err(err) => return Err(session_acquire_failed(err)),
                }
            }

            let remaining = budget.saturating_sub(waited);
            let timeout = remaining.min(WAITER_WAKEUP_INTERVAL);
            crate::hotpath_observe::record_session_acquire("wait");
            let state = self.inner.lock_state();
            let (state, _) = hotpath::measure_block!("semantic.session_pool.wait", {
                self.inner
                    .wakeups
                    .wait_timeout(state, timeout)
                    .unwrap_or_else(PoisonError::into_inner)
            });
            drop(state);
        }
    }

    /// Reap every idle session whose idle age exceeds the configured idle
    /// timeout. Returns the number of sessions reaped. The daemon/service
    /// layer drives this; the pool itself spawns no background thread.
    pub fn reap_idle(&self) -> usize {
        let now = self.inner.clock.now();
        let mut state = self.inner.lock_state();
        let reaped = reap_expired_locked(&mut state, now, self.inner.config.idle_timeout);
        if reaped != 0 {
            state.mark_availability_changed();
        }
        drop(state);
        if reaped != 0 {
            self.inner.wakeups.notify_all();
        }
        reaped
    }

    pub fn stats(&self) -> SessionPoolStats {
        let state = self.inner.lock_state();
        let idle = state.idle_sessions();
        SessionPoolStats {
            active: state.active,
            idle,
            live_sessions: state.active.saturating_add(idle),
            queued_waiters: state.waiters.len(),
            resident_bytes: state.resident_bytes,
            sessions_opened: state.sessions_opened,
            sessions_closed: state.sessions_closed,
            sessions_reaped: state.sessions_reaped,
            last_cold_load_micros: state.last_cold_load_micros,
            closed: state.closed,
        }
    }

    /// Close the pool: close every idle session immediately and every active
    /// session on release. Later acquisitions fail with
    /// [`SessionAcquireError::Closed`]. Returns the number of idle sessions
    /// closed.
    pub fn close(&self) -> usize {
        let mut state = self.inner.lock_state();
        if state.closed {
            return 0;
        }
        state.closed = true;
        let drained: usize = state.idle_sessions();
        let drained_bytes = state
            .idle
            .values()
            .flatten()
            .map(|entry| entry.resident_bytes)
            .sum::<u64>();
        state.idle.clear();
        state.resident_bytes = state.resident_bytes.saturating_sub(drained_bytes);
        state.sessions_closed += drained;
        state.mark_availability_changed();
        drop(state);
        self.inner.wakeups.notify_all();
        drained
    }

    /// Run the runtime's cold `open_session` on a dedicated loader thread and
    /// observe it in bounded slices: the typed `LoadDeadlineExceeded` fires
    /// while the load is still executing, and measured process resident
    /// growth is enforced against the artifact's resident-byte ceiling
    /// between slices instead of trusting the manifest estimate alone. Both
    /// verdicts fire the load's interruption signal so the runtime abandons
    /// the open at its next stage boundary. An abandoned loader keeps the
    /// slot and byte reservation until the runtime actually returns (its
    /// memory is genuinely in use until then), then releases both and
    /// discards the session.
    #[hotpath::measure(label = "semantic.session_pool.open_bounded")]
    fn open_session_bounded(
        &self,
        authority: &AdmittedProjectionArtifactV1,
        load_deadline: Duration,
        reserved_bytes: u64,
        tracked_resident_before_open: u64,
    ) -> Result<R::Session, SessionAcquireError> {
        let (result_tx, result_rx) = channel::<Result<R::Session, EmbedError>>();
        let inner = Arc::clone(&self.inner);
        let loader_authority = authority.clone();
        let interruption = Arc::new(LoadInterruptionSignalV1::default());
        let loader_interruption = Arc::clone(&interruption);
        let wait_started = self.inner.clock.now();
        // Growth is measured from the moment this load begins. Concurrent
        // allocations by other work inflate the delta, so the bound is
        // conservative under exactly the pathological overlap it guards.
        let baseline_resident_bytes = (self.inner.resident_sampler)();
        let spawned = thread::Builder::new()
            .name("td-semantic-model-load".to_owned())
            .spawn(move || {
                let load_started = inner.clock.now();
                let result = hotpath::measure_block!("semantic.model.load", {
                    inner
                        .runtime
                        .open_session(&loader_authority, loader_interruption.as_ref())
                });
                let opened = result.is_ok();
                if let Err(SendError(result)) = result_tx.send(result) {
                    // The acquisition abandoned this open at its deadline or
                    // resident bound. Release the slot and reservation now
                    // that the runtime returned, and account the completed-
                    // but-discarded open.
                    let load_elapsed = inner.clock.now().saturating_sub(load_started);
                    let mut state = inner.lock_state();
                    state.active -= 1;
                    state.resident_bytes = state.resident_bytes.saturating_sub(reserved_bytes);
                    if opened {
                        state.sessions_opened += 1;
                        state.sessions_closed += 1;
                        state.last_cold_load_micros = duration_micros(load_elapsed);
                    }
                    state.mark_availability_changed();
                    drop(state);
                    inner.wakeups.notify_all();
                    drop(result);
                }
            });
        if spawned.is_err() {
            self.release_reserved_slot(reserved_bytes);
            return Err(SessionAcquireError::Open(EmbedError::Runtime(
                RuntimeFailureV1 {
                    kind: RuntimeFailureKindV1::LoadFailed,
                    detail: "the model load thread could not be spawned".to_owned(),
                },
            )));
        }
        let resident_ceiling_bytes = authority.resident_byte_ceiling();
        let wall_started = Instant::now();
        loop {
            let remaining = load_deadline.saturating_sub(wall_started.elapsed());
            if remaining.is_zero() {
                let elapsed = self.inner.clock.now().saturating_sub(wait_started);
                interruption.fire(SemanticExecutionInterruptionV1::DeadlineExceeded);
                self.settle_abandoned_open(result_rx, reserved_bytes, elapsed);
                return Err(SessionAcquireError::LoadDeadlineExceeded {
                    elapsed,
                    deadline: load_deadline,
                });
            }
            match result_rx.recv_timeout(remaining.min(COLD_LOAD_OBSERVATION_INTERVAL)) {
                Ok(Ok(session)) => return Ok(session),
                Ok(Err(err)) => {
                    self.release_reserved_slot(reserved_bytes);
                    return Err(SessionAcquireError::Open(err));
                }
                Err(RecvTimeoutError::Timeout) => {
                    // An expired deadline outranks a same-slice resident
                    // verdict; the loop head returns the typed deadline error.
                    if wall_started.elapsed() >= load_deadline {
                        continue;
                    }
                    // Enforce the actual-resident bound only when both the
                    // baseline and the current sample were observed; an
                    // unavailable measurement surface abstains rather than
                    // fabricating growth.
                    let observed_growth_bytes = baseline_resident_bytes
                        .zip((self.inner.resident_sampler)())
                        .map(|(baseline, now)| now.saturating_sub(baseline));
                    let Some(observed_growth_bytes) = observed_growth_bytes else {
                        continue;
                    };
                    hotpath::gauge!("semantic_cold_load_resident_growth_bytes")
                        .set(observed_growth_bytes);
                    let effective_resident_bytes =
                        tracked_resident_before_open.checked_add(observed_growth_bytes);
                    let violated_ceiling = if observed_growth_bytes > resident_ceiling_bytes {
                        Some(resident_ceiling_bytes)
                    } else {
                        effective_resident_bytes
                            .is_none_or(|bytes| bytes > self.inner.config.memory_ceiling_bytes)
                            .then_some(self.inner.config.memory_ceiling_bytes)
                    };
                    let Some(ceiling_bytes) = violated_ceiling else {
                        continue;
                    };
                    let elapsed = self.inner.clock.now().saturating_sub(wait_started);
                    interruption.fire(SemanticExecutionInterruptionV1::Cancelled);
                    self.settle_abandoned_open(result_rx, reserved_bytes, elapsed);
                    return Err(SessionAcquireError::ResidentCeilingExceeded {
                        tracked_resident_bytes: tracked_resident_before_open,
                        observed_growth_bytes,
                        ceiling_bytes,
                    });
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.release_reserved_slot(reserved_bytes);
                    return Err(SessionAcquireError::Open(EmbedError::Runtime(
                        RuntimeFailureV1 {
                            kind: RuntimeFailureKindV1::LoadFailed,
                            detail: "the model load thread terminated without a result".to_owned(),
                        },
                    )));
                }
            }
        }
    }

    /// Close the race where the loader's send lands between an abandonment
    /// verdict and the receiver drop: a result present now is an abandoned
    /// load this caller must release, exactly like the loader-side
    /// abandonment path. An empty channel means the load is genuinely still
    /// executing, and the loader thread releases the slot and reservation
    /// when the runtime returns.
    fn settle_abandoned_open(
        &self,
        result_rx: Receiver<Result<R::Session, EmbedError>>,
        reserved_bytes: u64,
        elapsed: Duration,
    ) {
        match result_rx.try_recv() {
            Ok(result) => {
                let mut state = self.inner.lock_state();
                state.active -= 1;
                state.resident_bytes = state.resident_bytes.saturating_sub(reserved_bytes);
                if result.is_ok() {
                    state.sessions_opened += 1;
                    state.sessions_closed += 1;
                    state.last_cold_load_micros = duration_micros(elapsed);
                }
                state.mark_availability_changed();
                drop(state);
                self.inner.wakeups.notify_all();
                drop(result);
            }
            Err(TryRecvError::Disconnected) => {
                self.release_reserved_slot(reserved_bytes);
            }
            // Taking the receiver by value closes the race where the loader
            // sends after this empty observation but before abandonment
            // returns. Dropping it here makes that send fail, so the loader
            // owns reservation settlement in every future-send case.
            Err(TryRecvError::Empty) => drop(result_rx),
        }
    }

    fn release_reserved_slot(&self, reserved_bytes: u64) {
        let mut state = self.inner.lock_state();
        state.active -= 1;
        state.resident_bytes = state.resident_bytes.saturating_sub(reserved_bytes);
        state.mark_availability_changed();
        drop(state);
        self.inner.wakeups.notify_all();
    }

    fn make_guard(
        &self,
        identity: SessionIdentityV1,
        session: R::Session,
        resident_bytes: u64,
    ) -> PooledSession<R, C> {
        PooledSession {
            inner: Arc::clone(&self.inner),
            identity,
            session: Some(session),
            resident_bytes,
        }
    }
}

fn reap_expired_locked<S>(
    state: &mut PoolState<S>,
    now: Duration,
    idle_timeout: Duration,
) -> usize {
    let mut reaped = 0usize;
    let mut reaped_bytes = 0u64;
    state.idle.retain(|_identity, entries| {
        let mut kept = Vec::with_capacity(entries.len());
        for entry in entries.drain(..) {
            let idle_for = now.saturating_sub(entry.released_at);
            if idle_for >= idle_timeout {
                reaped += 1;
                reaped_bytes += entry.resident_bytes;
                // `entry.session` drops here, closing the session.
            } else {
                kept.push(entry);
            }
        }
        *entries = kept;
        !entries.is_empty()
    });
    state.resident_bytes = state.resident_bytes.saturating_sub(reaped_bytes);
    state.sessions_closed += reaped;
    state.sessions_reaped += reaped;
    reaped
}

/// RAII checkout guard. Dereferences to the warmed session; dropping the
/// guard returns the session to the idle pool (or closes it when the pool
/// is closed).
pub struct PooledSession<R: EmbeddingRuntime, C: MonotonicClock> {
    inner: Arc<PoolInner<R, C>>,
    identity: SessionIdentityV1,
    session: Option<R::Session>,
    resident_bytes: u64,
}

impl<R: EmbeddingRuntime, C: MonotonicClock> PooledSession<R, C> {
    pub fn identity(&self) -> &SessionIdentityV1 {
        &self.identity
    }
}

impl<R: EmbeddingRuntime, C: MonotonicClock> Deref for PooledSession<R, C> {
    type Target = R::Session;

    fn deref(&self) -> &Self::Target {
        self.session
            .as_ref()
            .unwrap_or_else(|| panic!("pooled session present until drop"))
    }
}

impl<R: EmbeddingRuntime, C: MonotonicClock> DerefMut for PooledSession<R, C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.session
            .as_mut()
            .unwrap_or_else(|| panic!("pooled session present until drop"))
    }
}

impl<R: EmbeddingRuntime, C: MonotonicClock> Drop for PooledSession<R, C> {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        let now = self.inner.clock.now();
        let mut state = self.inner.lock_state();
        state.active = state.active.saturating_sub(1);
        if state.closed {
            state.resident_bytes = state.resident_bytes.saturating_sub(self.resident_bytes);
            state.sessions_closed += 1;
            state.mark_availability_changed();
            drop(state);
            self.inner.wakeups.notify_all();
            drop(session);
        } else {
            state
                .idle
                .entry(self.identity.clone())
                .or_default()
                .push(IdleEntry {
                    session,
                    released_at: now,
                    resident_bytes: self.resident_bytes,
                });
            state.mark_availability_changed();
            drop(state);
            self.inner.wakeups.notify_all();
        }
    }
}

/// Removes a waiter from the FIFO queue when a blocking acquisition exits for
/// any reason, waking the next waiter when it was at the head.
struct WaiterPermit<R: EmbeddingRuntime, C: MonotonicClock> {
    inner: Arc<PoolInner<R, C>>,
    waiter_id: u64,
}

impl<R: EmbeddingRuntime, C: MonotonicClock> Drop for WaiterPermit<R, C> {
    fn drop(&mut self) {
        let mut state = self.inner.lock_state();
        if let Some(index) = state
            .waiters
            .iter()
            .position(|waiter_id| *waiter_id == self.waiter_id)
        {
            state.waiters.remove(index);
            state.mark_availability_changed();
        }
        hotpath::gauge!("semantic_session_waiters").set(state.waiters.len());
        drop(state);
        self.inner.wakeups.notify_all();
    }
}

/// Canonical pooled-session fixtures shared by this crate's tests and, through
/// the `test-helpers` feature, by dependent crates' test builds. Nothing here
/// opens a real embedding runtime or touches the filesystem.
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_support {
    use super::*;
    // Two `super` steps resolve to the directory module in every layout
    // (scratch `#[path]` crate root and the integrated crate-root module
    // alike).
    use super::super::artifact_store::AdmittedArtifactV1;
    use super::super::fastembed_adapter::AdmittedProjectionArtifactV1;
    use tracedecay_domain::{
        ChunkerRevision, EmbeddingDeviceClassV1, EmbeddingDeviceClassV1 as DeviceClassV1,
        EmbeddingMetricV1, EmbeddingMetricV1 as SemanticMetricV1, EmbeddingNormalizationV1,
        EmbeddingNormalizationV1 as ManifestNormalizationV1, EmbeddingPoolingV1,
        EmbeddingPoolingV1 as ManifestPoolingV1, EmbeddingPrecisionV1,
        EmbeddingPrecisionV1 as ManifestPrecisionV1, EmbeddingProjectionKeyV1,
        EmbeddingTruncationSideV1, EmbeddingTruncationSideV1 as TruncationSideV1, ManifestDigest,
        PrivacyDomainId,
    };
    use tracedecay_semantic_contracts::{
        ArtifactMemberPinV1, ArtifactMemberRoleV1, ArtifactPackageMemberV1, ArtifactProfileKindV1,
        MODEL_ARTIFACT_MANIFEST_SCHEMA_V1, ModelArtifactManifestPayloadV1, ModelArtifactManifestV1,
        PlatformTargetV1, ResourceCeilingV1, RuntimeCompatibilityV1, Sha256DigestHex,
        TruncationPolicyV1, UpstreamSourceV1,
    };

    pub fn domain_id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("canonical domain fixture identity")
    }

    pub fn domain_digest(label: u8) -> ManifestDigest {
        domain_id(&format!("sha256:{}", format!("{label:02x}").repeat(32)))
    }

    pub fn manifest_digest(digest: &Sha256DigestHex) -> ManifestDigest {
        domain_id(&format!("sha256:{}", digest.as_str()))
    }

    pub fn admitted_artifact() -> AdmittedArtifactV1 {
        admitted_artifact_sized(5, 9, 64 * 1024 * 1024)
    }

    /// Same fixture with caller-chosen member sizes and process ceiling, so a
    /// test can reproduce production-scale resident arithmetic (a ~600 MB
    /// model under a 2 GiB process budget) without a real artifact.
    pub fn admitted_artifact_sized(
        model_bytes: u64,
        tokenizer_bytes: u64,
        max_resident_bytes: u64,
    ) -> AdmittedArtifactV1 {
        admitted_artifact_limits(model_bytes, tokenizer_bytes, max_resident_bytes, 30_000)
    }

    /// Same fixture with every caller-chosen resource limit, so a test can
    /// exercise the load deadline against a real clock without waiting the
    /// production 30 s.
    pub fn admitted_artifact_limits(
        model_bytes: u64,
        tokenizer_bytes: u64,
        max_resident_bytes: u64,
        load_deadline_ms: u64,
    ) -> AdmittedArtifactV1 {
        let model_digest = Sha256DigestHex::of_bytes(b"model");
        let tokenizer_digest = Sha256DigestHex::of_bytes(b"tokenizer");
        let config_digest = Sha256DigestHex::of_bytes(b"config");
        let special_tokens_map_digest = Sha256DigestHex::of_bytes(b"special-tokens-map");
        let tokenizer_config_digest = Sha256DigestHex::of_bytes(b"tokenizer-config");
        let query_digest = Sha256DigestHex::of_bytes(b"query");
        let document_digest = Sha256DigestHex::of_bytes(b"document");
        let manifest = ModelArtifactManifestV1 {
            payload: ModelArtifactManifestPayloadV1 {
                schema: MODEL_ARTIFACT_MANIFEST_SCHEMA_V1.to_owned(),
                artifact_id: "fixture-embedding".to_owned(),
                profile_kind: ArtifactProfileKindV1::Embedding,
                spdx_license: "MIT".to_owned(),
                model_member: ArtifactMemberPinV1 {
                    digest: model_digest.clone(),
                    byte_length: model_bytes,
                },
                tokenizer_digest: tokenizer_digest.clone(),
                config_digest: config_digest.clone(),
                query_instruction_digest: Some(query_digest.clone()),
                document_instruction_digest: Some(document_digest.clone()),
                members: vec![
                    ArtifactPackageMemberV1 {
                        role: ArtifactMemberRoleV1::Model,
                        path: "model.onnx".to_owned(),
                        digest: model_digest,
                        byte_length: model_bytes,
                    },
                    ArtifactPackageMemberV1 {
                        role: ArtifactMemberRoleV1::Tokenizer,
                        path: "tokenizer.json".to_owned(),
                        digest: tokenizer_digest,
                        byte_length: tokenizer_bytes,
                    },
                    ArtifactPackageMemberV1 {
                        role: ArtifactMemberRoleV1::Config,
                        path: "config.json".to_owned(),
                        digest: config_digest,
                        byte_length: 6,
                    },
                    ArtifactPackageMemberV1 {
                        role: ArtifactMemberRoleV1::SpecialTokensMap,
                        path: "special_tokens_map.json".to_owned(),
                        digest: special_tokens_map_digest,
                        byte_length: 18,
                    },
                    ArtifactPackageMemberV1 {
                        role: ArtifactMemberRoleV1::TokenizerConfig,
                        path: "tokenizer_config.json".to_owned(),
                        digest: tokenizer_config_digest,
                        byte_length: 16,
                    },
                    ArtifactPackageMemberV1 {
                        role: ArtifactMemberRoleV1::QueryInstruction,
                        path: "query.txt".to_owned(),
                        digest: query_digest,
                        byte_length: 5,
                    },
                    ArtifactPackageMemberV1 {
                        role: ArtifactMemberRoleV1::DocumentInstruction,
                        path: "document.txt".to_owned(),
                        digest: document_digest,
                        byte_length: 8,
                    },
                ],
                dimensions: 8,
                metric: SemanticMetricV1::Cosine,
                normalization: ManifestNormalizationV1::L2,
                pooling: ManifestPoolingV1::Mean,
                truncation: TruncationPolicyV1 {
                    side: TruncationSideV1::Right,
                    max_length: 512,
                },
                precision: ManifestPrecisionV1::Fp32,
                runtime: RuntimeCompatibilityV1 {
                    runtime: "fastembed-ort".to_owned(),
                    build_revision: "ort-test-rev-1".to_owned(),
                    platforms: vec![PlatformTargetV1 {
                        os: "linux".to_owned(),
                        arch: "x86_64".to_owned(),
                    }],
                },
                device: DeviceClassV1::Cpu,
                resource_ceiling: ResourceCeilingV1 {
                    max_model_bytes: model_bytes.max(1024),
                    max_tokenizer_bytes: tokenizer_bytes.max(1024),
                    max_resident_bytes,
                    max_threads: 4,
                    max_batch_size: 8,
                    max_sequence_length: 512,
                    load_deadline_ms,
                },
                upstream: UpstreamSourceV1 {
                    name: "fixture/model".to_owned(),
                    version: "1".to_owned(),
                    revision: "fixture-revision".to_owned(),
                },
            },
        };
        AdmittedArtifactV1::test_fixture(manifest)
    }

    pub fn projection_for(artifact: &AdmittedArtifactV1) -> EmbeddingProjectionKeyV1 {
        let payload = &artifact.manifest().payload;
        EmbeddingProjectionKeyV1 {
            model_artifact_digest: manifest_digest(artifact.artifact_digest()),
            tokenizer_digest: manifest_digest(&payload.tokenizer_digest),
            config_digest: manifest_digest(&payload.config_digest),
            query_instruction_digest: payload
                .query_instruction_digest
                .as_ref()
                .map(manifest_digest),
            document_instruction_digest: payload
                .document_instruction_digest
                .as_ref()
                .map(manifest_digest),
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: 512,
            inference_batch_size: payload.resource_ceiling.max_batch_size,
            inference_batch_bytes: payload
                .resource_ceiling
                .max_batch_size
                .saturating_mul(payload.resource_ceiling.max_sequence_length)
                .saturating_mul(4),
            runtime_backend: "fastembed-ort".to_owned(),
            runtime_build_revision: "ort-test-rev-1".to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            dimensions: 8,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision: domain_id::<ChunkerRevision>("chunker.v1"),
            privacy_domain: domain_id::<PrivacyDomainId>("privacy.domain-a"),
            privacy_key_epoch: 7,
        }
    }

    pub fn authority() -> AdmittedProjectionArtifactV1 {
        authority_with_privacy("domain-a", 7)
    }

    pub fn authority_with_load_deadline_ms(load_deadline_ms: u64) -> AdmittedProjectionArtifactV1 {
        let artifact = admitted_artifact_limits(5, 9, 64 * 1024 * 1024, load_deadline_ms);
        let projection = projection_for(&artifact)
            .admit()
            .expect("valid projection fixture");
        AdmittedProjectionArtifactV1::admit(&artifact, &projection)
            .expect("matching projection and artifact")
    }

    pub fn authority_with_privacy(domain: &str, key_epoch: u64) -> AdmittedProjectionArtifactV1 {
        let artifact = admitted_artifact();
        let mut projection = projection_for(&artifact);
        projection.privacy_domain = domain_id::<PrivacyDomainId>(&format!("privacy.{domain}"));
        projection.privacy_key_epoch = key_epoch;
        let projection = projection.admit().expect("valid projection fixture");
        AdmittedProjectionArtifactV1::admit(&artifact, &projection)
            .expect("matching projection and artifact")
    }

    pub fn identity_with_epoch(domain: &str, key_epoch: u64) -> SessionIdentityV1 {
        SessionIdentityV1::from_authority(&authority_with_privacy(domain, key_epoch))
    }

    pub fn config(
        max_sessions: usize,
        idle_timeout: Duration,
        ceiling: u64,
    ) -> SessionPoolConfigV1 {
        SessionPoolConfigV1 {
            max_sessions,
            max_queued_waiters: 4,
            idle_timeout,
            memory_ceiling_bytes: ceiling,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::mpsc;
    use std::thread;

    use super::super::artifact_store::AdmittedArtifactV1;
    use super::super::fastembed_adapter::{
        BoundedSanitizedTextBatchV1, EmbedError, EmbeddingRuntime, FakeEmbeddingRuntime,
        FakeEmbeddingSession, ManualCancellation, ProjectionArtifactPinV1, RuntimeFailureKindV1,
    };
    use super::test_support::*;
    use super::*;
    use tracedecay_domain::{
        EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
        EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, PrivacyDomainId,
    };
    use tracedecay_semantic_contracts::{ArtifactProfileKindV1, Sha256DigestHex};

    fn fake_pool(
        max_sessions: usize,
        idle_timeout: Duration,
        ceiling: u64,
    ) -> SessionPool<FakeEmbeddingRuntime, ManualClock> {
        SessionPool::new(
            FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
            ManualClock::new(),
            config(max_sessions, idle_timeout, ceiling),
        )
        .expect("valid config")
    }

    struct TimedOpenRuntime {
        inner: FakeEmbeddingRuntime,
        clock: Arc<ManualClock>,
        load_time: Duration,
    }

    impl EmbeddingRuntime for TimedOpenRuntime {
        type Session = FakeEmbeddingSession;

        fn resident_bytes_reservation(&self, authority: &AdmittedProjectionArtifactV1) -> u64 {
            self.inner.resident_bytes_reservation(authority)
        }

        fn verify_artifact_compatibility(
            &self,
            authority: &AdmittedProjectionArtifactV1,
        ) -> Result<(), EmbedError> {
            self.inner.verify_artifact_compatibility(authority)
        }

        fn open_session(
            &self,
            authority: &AdmittedProjectionArtifactV1,
            interruption: &dyn SemanticExecutionAuthority,
        ) -> Result<Self::Session, EmbedError> {
            self.clock.advance(self.load_time);
            self.inner.open_session(authority, interruption)
        }
    }

    #[test]
    fn cold_open_beyond_the_artifact_deadline_is_discarded() {
        let clock = Arc::new(ManualClock::new());
        let pool = SessionPool::new(
            TimedOpenRuntime {
                inner: FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
                clock: Arc::clone(&clock),
                load_time: Duration::from_millis(30_001),
            },
            Arc::clone(&clock),
            config(1, Duration::from_mins(1), 1 << 20),
        )
        .expect("valid config");

        assert_eq!(
            pool.acquire(&authority()).err(),
            Some(SessionAcquireError::LoadDeadlineExceeded {
                elapsed: Duration::from_millis(30_001),
                deadline: Duration::from_millis(30_000),
            })
        );
        assert_eq!(pool.stats().last_cold_load_micros, Some(30_001_000));
        assert_eq!(pool.stats().live_sessions, 0);
    }

    #[test]
    fn config_validation_rejects_zero_bounds() {
        let mut c = config(0, Duration::from_secs(1), 1024);
        assert_eq!(c.validate(), Err(SessionPoolConfigError::ZeroMaxSessions));
        c.max_sessions = 1;
        c.memory_ceiling_bytes = 0;
        assert_eq!(c.validate(), Err(SessionPoolConfigError::ZeroMemoryCeiling));
    }

    #[test]
    fn acquire_release_reuses_warmed_session() {
        let pool = fake_pool(2, Duration::from_mins(1), 1 << 20);
        let authority = authority();
        {
            let _guard = pool.acquire(&authority).expect("first acquire");
            assert_eq!(pool.stats().active, 1);
        }
        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.sessions_opened, 1);
        {
            let _guard = pool.acquire(&authority).expect("second acquire");
            let stats = pool.stats();
            assert_eq!(stats.active, 1);
            assert_eq!(stats.idle, 0);
            assert_eq!(
                stats.sessions_opened, 1,
                "release/acquire reuses the warmed session"
            );
        }
    }

    #[test]
    fn pool_bound_exhaustion_is_typed_not_blocking() {
        let pool = fake_pool(1, Duration::from_mins(1), 1 << 20);
        let authority = authority();
        let held = pool.acquire(&authority).expect("first acquire");
        let result = pool.acquire(&authority);
        assert_eq!(
            result.err(),
            Some(SessionAcquireError::Exhausted { active: 1, max: 1 })
        );
        drop(held);
        pool.acquire(&authority)
            .expect("acquire succeeds after release");
    }

    #[test]
    fn memory_ceiling_is_enforced_with_typed_error() {
        // Each fake session reports 1024 resident bytes; ceiling allows one.
        let pool = fake_pool(4, Duration::from_mins(1), 1536);
        let authority = authority();
        let _held = pool.acquire(&authority).expect("first acquire");
        let result = pool.acquire(&authority);
        assert_eq!(
            result.err(),
            Some(SessionAcquireError::MemoryCeilingExceeded {
                used_bytes: 1024,
                requested_bytes: 1024,
                ceiling_bytes: 1536,
            })
        );
        let stats = pool.stats();
        assert_eq!(stats.active, 1, "failed acquisition reserves no slot");
        assert_eq!(stats.resident_bytes, 1024);
        assert_eq!(
            (stats.sessions_opened, stats.sessions_closed),
            (1, 0),
            "memory admission rejects the second session before model loading"
        );
    }

    #[test]
    fn blocking_acquire_does_not_wait_on_an_impossible_memory_request() {
        let pool = SessionPool::new(
            FakeEmbeddingRuntime::new().with_resident_bytes_per_session(2048),
            ManualClock::new(),
            config(1, Duration::from_mins(1), 1024),
        )
        .expect("valid pool");
        let error = pool
            .acquire_blocking(
                &authority(),
                Duration::from_mins(1),
                &ManualCancellation::new(),
            )
            .err()
            .expect("one session can never fit");

        assert_eq!(
            error,
            SessionAcquireError::MemoryCeilingExceeded {
                used_bytes: 0,
                requested_bytes: 2048,
                ceiling_bytes: 1024,
            }
        );
    }

    #[test]
    fn identity_separation_blocks_cross_privacy_reuse() {
        let pool = fake_pool(4, Duration::from_mins(1), 1 << 20);
        let domain_a = authority_with_privacy("domain-a", 7);
        let domain_b = authority_with_privacy("domain-b", 7);
        {
            let _guard = pool.acquire(&domain_a).expect("a");
        }
        let _b = pool.acquire(&domain_b).expect("distinct domain");
        let stats = pool.stats();
        assert_eq!(
            stats.sessions_opened, 2,
            "a privacy-domain change never reuses the other domain's session"
        );
        // Same domain, different key epoch also misses.
        let epoch_shifted = authority_with_privacy("domain-a", 8);
        let _c = pool.acquire(&epoch_shifted).expect("epoch");
        assert_eq!(pool.stats().sessions_opened, 3);
        // Same identity as the first still hits its warmed session.
        let _d = pool.acquire(&domain_a).expect("hit");
        assert_eq!(pool.stats().sessions_opened, 3);
    }

    #[test]
    fn pool_identity_derives_projection_and_privacy_from_admission() {
        let identity = identity_with_epoch("domain-a", 7);
        let same = identity_with_epoch("domain-a", 7);
        let different_domain = identity_with_epoch("domain-b", 7);
        let different_epoch = identity_with_epoch("domain-a", 8);

        assert_eq!(identity, same);
        assert_ne!(identity, different_domain);
        assert_ne!(identity, different_epoch);
        assert_eq!(identity.projection_key(), same.projection_key());
        assert_eq!(
            identity.privacy_domain(),
            &domain_id::<PrivacyDomainId>("privacy.domain-a")
        );
        assert_eq!(identity.privacy_key_epoch(), 7);
    }

    #[test]
    fn projection_artifact_admission_rejects_every_mismatched_pin_before_open() {
        let artifact = admitted_artifact();
        let base = projection_for(&artifact);
        let runtime = FakeEmbeddingRuntime::new();
        let counters = runtime.counters();

        let cases = [
            (
                ProjectionArtifactPinV1::ArtifactDigest,
                (|key: &mut EmbeddingProjectionKeyV1| key.model_artifact_digest = domain_digest(9))
                    as fn(&mut EmbeddingProjectionKeyV1),
            ),
            (ProjectionArtifactPinV1::TokenizerDigest, |key| {
                key.tokenizer_digest = domain_digest(9);
            }),
            (ProjectionArtifactPinV1::ConfigDigest, |key| {
                key.config_digest = domain_digest(9);
            }),
            (ProjectionArtifactPinV1::QueryInstructionDigest, |key| {
                key.query_instruction_digest = None;
            }),
            (ProjectionArtifactPinV1::DocumentInstructionDigest, |key| {
                key.document_instruction_digest = None;
            }),
            (ProjectionArtifactPinV1::Pooling, |key| {
                key.pooling = EmbeddingPoolingV1::Cls;
            }),
            (ProjectionArtifactPinV1::TruncationSide, |key| {
                key.truncation_side = EmbeddingTruncationSideV1::Left;
            }),
            (ProjectionArtifactPinV1::TruncationLength, |key| {
                key.truncation_length = 256;
            }),
            (ProjectionArtifactPinV1::InferenceBatchSize, |key| {
                key.inference_batch_size = 1;
            }),
            (ProjectionArtifactPinV1::InferenceBatchBytes, |key| {
                key.inference_batch_bytes = 1;
            }),
            (ProjectionArtifactPinV1::RuntimeBackend, |key| {
                key.runtime_backend = "other-runtime".to_owned();
            }),
            (ProjectionArtifactPinV1::RuntimeBuildRevision, |key| {
                key.runtime_build_revision = "other-revision".to_owned();
            }),
            (ProjectionArtifactPinV1::Dimensions, |key| {
                key.dimensions += 1;
            }),
            (ProjectionArtifactPinV1::Metric, |key| {
                key.metric = EmbeddingMetricV1::DotProduct;
            }),
            (ProjectionArtifactPinV1::Normalization, |key| {
                key.normalization = EmbeddingNormalizationV1::None;
            }),
            (ProjectionArtifactPinV1::Precision, |key| {
                key.precision = EmbeddingPrecisionV1::Fp16;
            }),
        ];

        for (expected, mutate) in cases {
            let mut key = base.clone();
            mutate(&mut key);
            let admitted = key.admit().expect("mutated key remains structurally valid");
            assert_eq!(
                AdmittedProjectionArtifactV1::admit(&artifact, &admitted),
                Err(expected),
                "mismatch must identify its exact pin"
            );
        }
        assert_eq!(
            counters.compatibility_checks.load(AtomicOrdering::SeqCst),
            0,
            "pin mismatch is rejected before runtime compatibility"
        );
        assert_eq!(counters.sessions_opened.load(AtomicOrdering::SeqCst), 0);
        drop(runtime);
    }

    #[test]
    fn projection_artifact_admission_rejects_inference_batch_size_mismatch() {
        let artifact = admitted_artifact();
        let mut projection = projection_for(&artifact);
        projection.inference_batch_size += 1;
        let projection = projection
            .admit()
            .expect("batch-size mutation remains structurally valid");

        assert_eq!(
            AdmittedProjectionArtifactV1::admit(&artifact, &projection),
            Err(ProjectionArtifactPinV1::InferenceBatchSize),
            "admission must identify a projection inference batch that differs from the manifest ceiling"
        );
    }

    #[test]
    fn projection_artifact_admission_rejects_inference_batch_byte_ceiling_mismatch() {
        let artifact = admitted_artifact();
        let mut projection = projection_for(&artifact);
        projection.inference_batch_bytes -= 1;
        let projection = projection
            .admit()
            .expect("byte-ceiling mutation remains structurally valid");

        assert_eq!(
            AdmittedProjectionArtifactV1::admit(&artifact, &projection),
            Err(ProjectionArtifactPinV1::InferenceBatchBytes),
            "admission must identify a projection byte ceiling that differs from the manifest ceiling"
        );
    }

    #[test]
    fn projection_artifact_admission_rejects_artifact_authority_mismatches() {
        let valid = admitted_artifact();
        let projection = projection_for(&valid)
            .admit()
            .expect("valid projection fixture");
        let manifest = valid.manifest().clone();
        let wrong_artifact = AdmittedArtifactV1::test_fixture_with_identities(
            manifest.clone(),
            Sha256DigestHex::of_bytes(b"wrong-artifact"),
            manifest.canonical_digest(),
        );
        assert_eq!(
            AdmittedProjectionArtifactV1::admit(&wrong_artifact, &projection),
            Err(ProjectionArtifactPinV1::ArtifactIdentity)
        );

        let wrong_manifest = AdmittedArtifactV1::test_fixture_with_identities(
            manifest.clone(),
            manifest.artifact_identity_digest(),
            Sha256DigestHex::of_bytes(b"wrong-manifest"),
        );
        assert_eq!(
            AdmittedProjectionArtifactV1::admit(&wrong_manifest, &projection),
            Err(ProjectionArtifactPinV1::ManifestIdentity)
        );

        let mut reranker_manifest = manifest;
        reranker_manifest.payload.profile_kind = ArtifactProfileKindV1::Reranker;
        let reranker = AdmittedArtifactV1::test_fixture(reranker_manifest);
        let reranker_projection = projection_for(&reranker)
            .admit()
            .expect("valid projection fixture");
        assert_eq!(
            AdmittedProjectionArtifactV1::admit(&reranker, &reranker_projection),
            Err(ProjectionArtifactPinV1::ProfileKind)
        );
    }

    #[test]
    fn projection_artifact_authority_owns_privacy_domain_and_epoch() {
        let first = authority_with_privacy("domain-a", 7);
        let different_domain = authority_with_privacy("domain-b", 7);
        let different_epoch = authority_with_privacy("domain-a", 8);

        assert_ne!(
            SessionIdentityV1::from_authority(&first),
            SessionIdentityV1::from_authority(&different_domain)
        );
        assert_ne!(
            SessionIdentityV1::from_authority(&first),
            SessionIdentityV1::from_authority(&different_epoch)
        );
    }

    #[test]
    fn compatibility_failure_prevents_session_open() {
        let runtime = FakeEmbeddingRuntime::new()
            .with_compatibility_failure(RuntimeFailureKindV1::IncompatibleRuntime);
        let counters = runtime.counters();
        let pool = SessionPool::new(
            runtime,
            ManualClock::new(),
            config(2, Duration::from_mins(1), 1 << 30),
        )
        .expect("valid config");
        let err = pool
            .acquire(&authority())
            .err()
            .expect("compatibility failure");
        assert!(matches!(
            err,
            SessionAcquireError::Open(EmbedError::Runtime(ref failure))
                if failure.kind == RuntimeFailureKindV1::IncompatibleRuntime
        ));
        assert_eq!(
            counters.compatibility_checks.load(AtomicOrdering::SeqCst),
            1
        );
        assert_eq!(counters.sessions_opened.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn manifest_resident_ceiling_bounds_opened_session() {
        let artifact = admitted_artifact();
        let mut manifest = artifact.manifest().clone();
        manifest.payload.resource_ceiling.max_resident_bytes = 1024;
        let artifact = AdmittedArtifactV1::test_fixture(manifest);
        let projection = projection_for(&artifact)
            .admit()
            .expect("valid projection fixture");
        let authority = AdmittedProjectionArtifactV1::admit(&artifact, &projection)
            .expect("matching authority");
        let pool = SessionPool::new(
            FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1025),
            ManualClock::new(),
            config(2, Duration::from_mins(1), 1 << 30),
        )
        .expect("valid config");
        let err = pool
            .acquire(&authority)
            .err()
            .expect("resident ceiling failure");
        assert_eq!(
            err,
            SessionAcquireError::MemoryCeilingExceeded {
                used_bytes: 0,
                requested_bytes: 1025,
                ceiling_bytes: 1024,
            }
        );
        assert_eq!(pool.stats().resident_bytes, 0);
        assert_eq!(pool.stats().active, 0);
    }

    #[test]
    fn idle_sessions_reap_only_after_timeout_on_injected_clock() {
        let clock = ManualClock::new();
        let pool = SessionPool::new(
            FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
            clock,
            config(2, Duration::from_secs(30), 1 << 20),
        )
        .expect("valid config");
        let authority = authority();
        {
            let _guard = pool.acquire(&authority).expect("acquire");
        }
        assert_eq!(pool.stats().idle, 1);

        pool.inner.clock.advance(Duration::from_secs(29));
        assert_eq!(pool.reap_idle(), 0, "under the timeout nothing reaps");
        assert_eq!(pool.stats().idle, 1);

        pool.inner.clock.advance(Duration::from_secs(2));
        assert_eq!(pool.reap_idle(), 1, "past the timeout the session reaps");
        let stats = pool.stats();
        assert_eq!(stats.idle, 0);
        assert_eq!(stats.resident_bytes, 0);
        assert_eq!(stats.sessions_reaped, 1);
        assert_eq!(stats.sessions_closed, 1);
    }

    #[test]
    fn acquire_reuses_expired_exact_identity_instead_of_reopening() {
        let pool = fake_pool(2, Duration::from_secs(10), 1 << 20);
        let authority = authority();
        {
            let _guard = pool.acquire(&authority).expect("acquire");
        }
        pool.inner.clock.advance(Duration::from_secs(11));
        let _guard = pool.acquire(&authority).expect("second acquire");
        let stats = pool.stats();
        assert_eq!(
            stats.sessions_reaped, 0,
            "on-demand acquisition must not discard the exact session it needs"
        );
        assert_eq!(
            stats.sessions_opened, 1,
            "the already-warmed exact session must be reused after idle"
        );
    }

    #[test]
    fn warm_acquire_reaps_expired_sibling_sessions() {
        let pool = fake_pool(2, Duration::from_secs(10), 1 << 20);
        let authority = authority();
        let first = pool.acquire(&authority).expect("first acquire");
        let second = pool.acquire(&authority).expect("second acquire");
        drop(first);
        drop(second);
        assert_eq!(pool.stats().idle, 2, "both exact sessions are idle");

        pool.inner.clock.advance(Duration::from_secs(11));
        let _reused = pool.acquire(&authority).expect("reuse exact session");
        let stats = pool.stats();
        assert_eq!(
            stats.sessions_opened, 2,
            "the selected exact session stays warm"
        );
        assert_eq!(
            stats.sessions_reaped, 1,
            "the expired exact sibling is reclaimed"
        );
        assert_eq!(stats.idle, 0, "no expired sibling remains resident");
    }

    #[test]
    fn runtime_open_failure_surfaces_as_typed_acquire_error() {
        let pool = SessionPool::new(
            FakeEmbeddingRuntime::new().with_open_failure(RuntimeFailureKindV1::OutOfMemory),
            ManualClock::new(),
            config(2, Duration::from_mins(1), 1 << 20),
        )
        .expect("valid config");
        let result = pool.acquire(&authority());
        match result.err() {
            Some(SessionAcquireError::Open(EmbedError::Runtime(failure))) => {
                assert_eq!(failure.kind, RuntimeFailureKindV1::OutOfMemory);
            }
            other => panic!("expected typed open failure, got {other:?}"),
        }
        assert_eq!(
            pool.stats().active,
            0,
            "failed open releases the reserved slot"
        );
    }

    #[test]
    fn blocking_acquire_succeeds_after_a_release() {
        let pool = SessionPool::new(
            FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
            ManualClock::new(),
            config(1, Duration::from_mins(1), 1 << 20),
        )
        .expect("valid config");
        let authority = authority();
        let held = pool.acquire(&authority).expect("held");
        let cancel = ManualCancellation::new();
        thread::scope(|scope| {
            let waiting =
                scope.spawn(|| pool.acquire_blocking(&authority, Duration::from_secs(5), &cancel));
            while pool.stats().queued_waiters == 0 {
                thread::yield_now();
            }
            drop(held);
            waiting
                .join()
                .expect("no panic")
                .expect("waiter acquires after release");
        });
    }

    #[test]
    fn blocking_acquire_waits_without_repeated_runtime_admission() {
        let runtime = FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024);
        let counters = runtime.counters();
        let pool = SessionPool::new(
            runtime,
            SystemMonotonicClock::default(),
            config(1, Duration::from_mins(1), 1 << 20),
        )
        .expect("valid config");
        let authority = authority();
        let held = pool.acquire(&authority).expect("held");
        let cancel = ManualCancellation::new();
        let (done_tx, done_rx) = mpsc::channel();

        thread::scope(|scope| {
            scope.spawn(|| {
                let result = pool
                    .acquire_blocking(&authority, Duration::from_secs(5), &cancel)
                    .map(drop);
                done_tx.send(result).expect("send waiter result");
            });
            while pool.stats().queued_waiters == 0 {
                thread::yield_now();
            }
            thread::sleep(Duration::from_millis(25));
            assert_eq!(
                counters.compatibility_checks.load(AtomicOrdering::SeqCst),
                2,
                "the queued waiter performs one admission check, not a retry spin"
            );

            drop(held);
            assert!(
                done_rx
                    .recv_timeout(Duration::from_secs(1))
                    .expect("waiter completes after release")
                    .is_ok()
            );
        });
    }

    #[test]
    fn blocking_acquire_serves_waiters_in_fifo_order() {
        let pool = SessionPool::new(
            FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
            SystemMonotonicClock::default(),
            config(1, Duration::from_mins(1), 1 << 20),
        )
        .expect("valid config");
        let authority = authority();
        let held = pool.acquire(&authority).expect("held");
        let cancel = ManualCancellation::new();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();

        thread::scope(|scope| {
            let first_acquired_tx = acquired_tx.clone();
            let first_pool = &pool;
            let first_authority = &authority;
            let first_cancel = &cancel;
            scope.spawn(move || {
                let guard = first_pool
                    .acquire_blocking(first_authority, Duration::from_secs(5), first_cancel)
                    .expect("first waiter acquires");
                first_acquired_tx
                    .send("first")
                    .expect("report first waiter");
                release_first_rx.recv().expect("release first waiter");
                drop(guard);
            });
            while pool.stats().queued_waiters < 1 {
                thread::yield_now();
            }

            let second_acquired_tx = acquired_tx.clone();
            let second_pool = &pool;
            let second_authority = &authority;
            let second_cancel = &cancel;
            scope.spawn(move || {
                let guard = second_pool
                    .acquire_blocking(second_authority, Duration::from_secs(5), second_cancel)
                    .expect("second waiter acquires");
                second_acquired_tx
                    .send("second")
                    .expect("report second waiter");
                drop(guard);
            });
            while pool.stats().queued_waiters < 2 {
                thread::yield_now();
            }

            drop(held);
            assert_eq!(
                acquired_rx
                    .recv_timeout(Duration::from_secs(1))
                    .expect("first acquisition result"),
                "first"
            );
            assert!(
                acquired_rx.recv_timeout(Duration::from_millis(25)).is_err(),
                "the second waiter cannot bypass the first checked-out session"
            );
            release_first_tx.send(()).expect("release first waiter");
            assert_eq!(
                acquired_rx
                    .recv_timeout(Duration::from_secs(1))
                    .expect("second acquisition result"),
                "second"
            );
        });
    }

    #[test]
    fn blocking_acquire_reports_deadline_on_injected_clock() {
        let pool = fake_pool(1, Duration::from_mins(1), 1 << 20);
        let authority = authority();
        let _held = pool.acquire(&authority).expect("held");
        let cancel = ManualCancellation::new();
        let (done_tx, done_rx) = mpsc::channel();
        thread::scope(|scope| {
            scope.spawn(|| {
                done_tx
                    .send(pool.acquire_blocking(&authority, Duration::from_secs(10), &cancel))
                    .expect("send deadline result");
            });
            while pool.stats().queued_waiters == 0 {
                thread::yield_now();
            }
            pool.inner.clock.advance(Duration::from_secs(11));
            let result = done_rx.recv_timeout(Duration::from_secs(1));
            if result.is_err() {
                cancel.cancel();
            }
            let err = result.expect("deadline wakes queued waiter").err();
            assert!(
                matches!(
                    err,
                    Some(SessionAcquireError::DeadlineExceeded { budget, .. })
                    if budget == Duration::from_secs(10)
                ),
                "expected typed deadline, got {err:?}"
            );
        });
        assert_eq!(pool.stats().queued_waiters, 0, "waiter deregistered");
    }

    #[test]
    fn blocking_acquire_honors_cancellation() {
        let pool = fake_pool(1, Duration::from_mins(1), 1 << 20);
        let authority = authority();
        let _held = pool.acquire(&authority).expect("held");
        let cancel = ManualCancellation::new();
        let (done_tx, done_rx) = mpsc::channel();
        thread::scope(|scope| {
            scope.spawn(|| {
                done_tx
                    .send(pool.acquire_blocking(&authority, Duration::from_mins(10), &cancel))
                    .expect("send cancellation result");
            });
            while pool.stats().queued_waiters == 0 {
                thread::yield_now();
            }
            cancel.cancel();
            let err = done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("cancellation wakes queued waiter")
                .err();
            assert_eq!(err, Some(SessionAcquireError::Cancelled));
        });
        assert_eq!(pool.stats().queued_waiters, 0);
    }

    #[test]
    fn waiter_queue_overflow_is_typed() {
        let pool = SessionPool::new(
            FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
            SystemMonotonicClock::default(),
            SessionPoolConfigV1 {
                max_sessions: 1,
                max_queued_waiters: 1,
                idle_timeout: Duration::from_mins(1),
                memory_ceiling_bytes: 1 << 20,
            },
        )
        .expect("valid config");
        let authority = authority();
        let _held = pool.acquire(&authority).expect("held");
        let cancel = ManualCancellation::new();
        thread::scope(|scope| {
            let waiting =
                scope.spawn(|| pool.acquire_blocking(&authority, Duration::from_secs(5), &cancel));
            while pool.stats().queued_waiters == 0 {
                thread::yield_now();
            }
            let err = pool
                .acquire_blocking(&authority, Duration::from_secs(5), &cancel)
                .err();
            assert_eq!(
                err,
                Some(SessionAcquireError::QueueFull { queued: 1, max: 1 }),
                "second waiter gets a typed queue-full error"
            );
            cancel.cancel();
            assert_eq!(
                waiting.join().expect("no panic").err(),
                Some(SessionAcquireError::Cancelled)
            );
        });
    }

    #[test]
    fn close_closes_idle_and_rejects_new_acquisitions() {
        let pool = fake_pool(2, Duration::from_mins(1), 1 << 20);
        let authority = authority();
        {
            let _guard = pool.acquire(&authority).expect("acquire");
        }
        assert_eq!(pool.stats().idle, 1);
        assert_eq!(pool.close(), 1, "one idle session closed");
        let stats = pool.stats();
        assert!(stats.closed);
        assert_eq!(stats.sessions_closed, 1);
        assert_eq!(stats.resident_bytes, 0);
        assert_eq!(
            pool.acquire(&authority).err(),
            Some(SessionAcquireError::Closed)
        );
        assert_eq!(pool.close(), 0, "close is idempotent");
    }

    #[test]
    fn active_session_closes_on_release_after_pool_close() {
        let pool = fake_pool(2, Duration::from_mins(1), 1 << 20);
        let authority = authority();
        let guard = pool.acquire(&authority).expect("acquire");
        assert_eq!(pool.close(), 0);
        assert_eq!(
            pool.stats().resident_bytes,
            1024,
            "closing idle sessions must not erase active-session accounting"
        );
        drop(guard);
        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 0);
        assert_eq!(stats.sessions_closed, 1);
        assert_eq!(stats.resident_bytes, 0);
    }

    #[test]
    fn pooled_guard_derefs_to_session_and_embeds() {
        let pool = fake_pool(1, Duration::from_mins(1), 1 << 20);
        let authority = authority();
        let id = SessionIdentityV1::from_authority(&authority);
        let mut guard = pool.acquire(&authority).expect("acquire");
        assert_eq!(guard.identity(), &id);
        assert_eq!(
            guard.authority(),
            &authority,
            "session echoes its admitted projection-artifact authority"
        );
        let batch = BoundedSanitizedTextBatchV1::try_new(vec!["fn main()".to_string()], 8, 1024)
            .expect("batch");
        let cancel = ManualCancellation::new();
        let vectors = guard.embed_batch(&batch, &cancel).expect("embed");
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].dimensions, 8);
    }

    #[test]
    fn stats_track_lifecycle_counters() {
        let runtime = FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024);
        let counters = runtime.counters();
        let pool = SessionPool::new(
            runtime,
            ManualClock::new(),
            config(2, Duration::from_secs(5), 1 << 20),
        )
        .expect("valid config");
        let authority = authority();
        {
            let _g = pool.acquire(&authority).expect("one");
        }
        pool.inner.clock.advance(Duration::from_secs(6));
        assert_eq!(pool.reap_idle(), 1);
        let stats = pool.stats();
        assert_eq!(stats.sessions_opened, 1);
        assert_eq!(stats.sessions_closed, 1);
        assert_eq!(stats.sessions_reaped, 1);
        assert_eq!(
            counters.sessions_opened.load(AtomicOrdering::SeqCst),
            1,
            "pool stats agree with runtime counters"
        );
        assert_eq!(counters.sessions_closed.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn hard_session_bound_counts_idle_sessions_from_other_identities() {
        let pool = fake_pool(1, Duration::from_mins(1), 1 << 20);
        {
            let _domain_a = pool
                .acquire(&authority_with_privacy("domain-a", 7))
                .expect("first identity");
        }
        assert_eq!(pool.stats().idle, 1);

        let error = pool
            .acquire(&authority_with_privacy("domain-b", 7))
            .err()
            .expect("an idle foreign identity still occupies the only live session slot");

        assert_eq!(error, SessionAcquireError::Exhausted { active: 0, max: 1 });
        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.sessions_opened, 1);
    }

    #[test]
    fn owned_runtime_factory_restarts_without_exposing_a_half_reloaded_pool() {
        use super::super::runtime_service::{
            SemanticRuntimeService, SharedEmbeddingRuntimeFactory,
        };

        let opens = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&opens);
        let factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> = Arc::new(move || {
            observed.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024))
        });
        let service = SemanticRuntimeService::new_owned(
            Arc::new(authority()),
            factory,
            config(1, Duration::from_mins(1), 1 << 20),
        )
        .expect("runtime service");
        {
            let _session = service.acquire().expect("warm the original pool");
        }

        let report = service.restart().expect("restart atomically");

        assert_eq!(report.prior_generation, 1);
        assert_eq!(report.current_generation, 2);
        assert_eq!(report.closed_idle_sessions, 1);
        assert_eq!(opens.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(service.stats().sessions_opened, 0);
        service.acquire().expect("replacement pool is usable");
    }

    #[test]
    fn failed_reload_preserves_the_published_runtime_generation() {
        use super::super::runtime_service::{
            SemanticRuntimeService, SharedEmbeddingRuntimeFactory,
        };

        let initial: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024)));
        let service = SemanticRuntimeService::new_owned(
            Arc::new(authority()),
            initial,
            config(1, Duration::from_mins(1), 1 << 20),
        )
        .expect("runtime service");
        {
            let _session = service.acquire().expect("warm original");
        }
        let failing: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> = Arc::new(|| {
            Ok(FakeEmbeddingRuntime::new()
                .with_compatibility_failure(RuntimeFailureKindV1::IncompatibleRuntime))
        });

        assert!(
            service.reload(Arc::new(authority()), failing).is_err(),
            "an incompatible replacement is never published"
        );
        assert_eq!(service.generation(), 1);
        assert_eq!(service.stats().idle, 1);
        service.acquire().expect("the original pool remains usable");
    }
}
