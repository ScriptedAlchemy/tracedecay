//! PR10 quarantined preparation packet `pr10/prep-runtime-adapter`
//! (Plan 31, `docs/plans/tracedecay-v2/31-native-fastembed-semantic-code-search.md`).
//!
//! Bounded embedding session pool. Sessions are keyed by the complete
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
//! QUARANTINE STATUS: temporarily unlinked. Registration happens at
//! integration by the Sol coordinator; this file must not be referenced from
//! `src/lib.rs` or `src/semantic_code/mod.rs` in this packet. It is std-only
//! (plus its sibling `fastembed_adapter` port surface) so it compiles
//! standalone via `#[path]` inclusion.
//!
//! See `fastembed_adapter.rs` for the ESCALATION list; ESCALATION-3
//! (projection identity as opaque digest + privacy domain + key epoch,
//! bridged from `EmbeddingProjectionKeyV1` at integration) and ESCALATION-4
//! (deadlines as `Duration` against the injected clock, bridged from PR9
//! `RetrievalBudget`) shape this file directly.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::fastembed_adapter::{
    CancellationSignal, EmbedError, EmbeddingRuntime, EmbeddingSession, VerifiedEmbeddingArtifactV1,
};

/// Monotonic time source. Reaping and blocking-acquire deadlines are driven
/// entirely by this clock, so tests inject [`ManualClock`] and never depend
/// on wall time.
pub trait MonotonicClock: Send + Sync {
    /// Time since an arbitrary, fixed epoch. Must be monotonically
    /// non-decreasing per clock instance.
    fn now(&self) -> Duration;
}

/// Wall-clock driver for production wiring.
#[derive(Debug)]
pub struct SystemMonotonicClock {
    start: Instant,
}

impl Default for SystemMonotonicClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl MonotonicClock for SystemMonotonicClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Deterministic test clock; advances only when told to.
#[derive(Debug, Default)]
pub struct ManualClock {
    micros: AtomicU64,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&self, delta: Duration) {
        self.micros
            .fetch_add(delta.as_micros() as u64, Ordering::SeqCst);
    }
}

impl MonotonicClock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_micros(self.micros.load(Ordering::SeqCst))
    }
}

/// The complete projection/privacy identity of a warmed session (Plan 31:
/// sessions are keyed by embedding projection key; privacy-domain/key-epoch
/// changes produce zero session cache hits).
///
/// `projection_profile_digest` is the canonical digest of the semantic
/// projection profile (at integration: Plan 25 `ProjectionKeyV1.profile_digest`
/// carrying Plan 31's `EmbeddingProjectionKeyV1`). It is opaque here because
/// the domain value is owned by a different packet.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionIdentityV1 {
    pub projection_profile_digest: String,
    pub privacy_domain: String,
    pub key_epoch: u64,
}

impl SessionIdentityV1 {
    pub fn new(
        projection_profile_digest: impl Into<String>,
        privacy_domain: impl Into<String>,
        key_epoch: u64,
    ) -> Self {
        Self {
            projection_profile_digest: projection_profile_digest.into(),
            privacy_domain: privacy_domain.into(),
            key_epoch,
        }
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
    pub queued_waiters: usize,
    pub resident_bytes: u64,
    pub sessions_opened: usize,
    pub sessions_closed: usize,
    pub sessions_reaped: usize,
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
    queued_waiters: usize,
    resident_bytes: u64,
    sessions_opened: usize,
    sessions_closed: usize,
    sessions_reaped: usize,
    closed: bool,
}

impl<S> Default for PoolState<S> {
    fn default() -> Self {
        Self {
            idle: HashMap::new(),
            active: 0,
            queued_waiters: 0,
            resident_bytes: 0,
            sessions_opened: 0,
            sessions_closed: 0,
            sessions_reaped: 0,
            closed: false,
        }
    }
}

struct PoolInner<R: EmbeddingRuntime, C: MonotonicClock> {
    runtime: R,
    clock: C,
    config: SessionPoolConfigV1,
    state: Mutex<PoolState<R::Session>>,
}

impl<R: EmbeddingRuntime, C: MonotonicClock> PoolInner<R, C> {
    fn lock_state(&self) -> MutexGuard<'_, PoolState<R::Session>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Bounded pool of warmed embedding sessions over one [`EmbeddingRuntime`]
/// (Plan 31: cold and warm sessions, OOM, cancellation, and offline startup
/// are exercised against this pool with the deterministic fake runtime).
pub struct SessionPool<R: EmbeddingRuntime, C: MonotonicClock> {
    inner: Arc<PoolInner<R, C>>,
}

impl<R: EmbeddingRuntime, C: MonotonicClock> SessionPool<R, C> {
    pub fn new(
        runtime: R,
        clock: C,
        config: SessionPoolConfigV1,
    ) -> Result<Self, SessionPoolConfigError> {
        config.validate()?;
        Ok(Self {
            inner: Arc::new(PoolInner {
                runtime,
                clock,
                config,
                state: Mutex::new(PoolState::default()),
            }),
        })
    }

    /// Non-blocking acquisition. Reuses an idle session with an exactly
    /// matching identity or opens a new one within the bounds; otherwise
    /// fails with a typed error. Never blocks and never substitutes a
    /// session from another identity.
    pub fn acquire(
        &self,
        identity: &SessionIdentityV1,
        artifact: &VerifiedEmbeddingArtifactV1,
    ) -> Result<PooledSession<R, C>, SessionAcquireError> {
        self.inner
            .runtime
            .verify_artifact_compatibility(artifact)
            .map_err(SessionAcquireError::Open)?;
        let now = self.inner.clock.now();
        let mut state = self.inner.lock_state();
        if state.closed {
            return Err(SessionAcquireError::Closed);
        }
        reap_expired_locked(&mut state, now, self.inner.config.idle_timeout);
        let reusable = state.idle.get_mut(identity).and_then(|entries| {
            let index = entries
                .iter()
                .rposition(|entry| entry.session.descriptor() == artifact)?;
            Some(entries.swap_remove(index))
        });
        if let Some(entry) = reusable {
            state.active += 1;
            return Ok(self.make_guard(identity.clone(), entry.session, entry.resident_bytes));
        }
        if state.active >= self.inner.config.max_sessions {
            return Err(SessionAcquireError::Exhausted {
                active: state.active,
                max: self.inner.config.max_sessions,
            });
        }
        // Reserve the slot before opening so concurrent acquirers observe
        // the bound, then open outside the lock.
        state.active += 1;
        drop(state);

        let session = match self.inner.runtime.open_session(artifact) {
            Ok(session) => session,
            Err(err) => {
                let mut state = self.inner.lock_state();
                state.active -= 1;
                return Err(SessionAcquireError::Open(err));
            }
        };
        let resident_bytes = session.resident_bytes_estimate();
        let mut state = self.inner.lock_state();
        if state.closed {
            state.active -= 1;
            drop(state);
            drop(session);
            return Err(SessionAcquireError::Closed);
        }
        let effective_ceiling = self
            .inner
            .config
            .memory_ceiling_bytes
            .min(artifact.resident_byte_ceiling);
        if state.resident_bytes + resident_bytes > effective_ceiling {
            let used = state.resident_bytes;
            state.active -= 1;
            drop(state);
            drop(session);
            return Err(SessionAcquireError::MemoryCeilingExceeded {
                used_bytes: used,
                requested_bytes: resident_bytes,
                ceiling_bytes: effective_ceiling,
            });
        }
        state.resident_bytes += resident_bytes;
        state.sessions_opened += 1;
        Ok(self.make_guard(identity.clone(), session, resident_bytes))
    }

    /// Bounded blocking acquisition with FIFO-fair waiter accounting.
    /// Retries while the pool is exhausted or over the memory ceiling, until
    /// the cancellation signal fires or `budget` (measured on the injected
    /// clock) elapses. Returns typed errors; never waits past the budget.
    pub fn acquire_blocking(
        &self,
        identity: &SessionIdentityV1,
        artifact: &VerifiedEmbeddingArtifactV1,
        budget: Duration,
        cancel: &dyn CancellationSignal,
    ) -> Result<PooledSession<R, C>, SessionAcquireError> {
        {
            let mut state = self.inner.lock_state();
            if state.closed {
                return Err(SessionAcquireError::Closed);
            }
            if state.queued_waiters >= self.inner.config.max_queued_waiters {
                return Err(SessionAcquireError::QueueFull {
                    queued: state.queued_waiters,
                    max: self.inner.config.max_queued_waiters,
                });
            }
            state.queued_waiters += 1;
        }
        let _permit = WaiterPermit {
            inner: Arc::clone(&self.inner),
        };
        let start = self.inner.clock.now();
        loop {
            match self.acquire(identity, artifact) {
                Ok(guard) => return Ok(guard),
                Err(
                    retryable @ (SessionAcquireError::Exhausted { .. }
                    | SessionAcquireError::MemoryCeilingExceeded { .. }),
                ) => {
                    drop(retryable);
                    if cancel.cancelled() {
                        return Err(SessionAcquireError::Cancelled);
                    }
                    let waited = self.inner.clock.now().saturating_sub(start);
                    if waited >= budget {
                        return Err(SessionAcquireError::DeadlineExceeded { waited, budget });
                    }
                    std::thread::yield_now();
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Reap every idle session whose idle age exceeds the configured idle
    /// timeout. Returns the number of sessions reaped. The daemon/service
    /// layer drives this; the pool itself spawns no background thread.
    pub fn reap_idle(&self) -> usize {
        let now = self.inner.clock.now();
        let mut state = self.inner.lock_state();
        reap_expired_locked(&mut state, now, self.inner.config.idle_timeout)
    }

    pub fn stats(&self) -> SessionPoolStats {
        let state = self.inner.lock_state();
        SessionPoolStats {
            active: state.active,
            idle: state.idle.values().map(Vec::len).sum(),
            queued_waiters: state.queued_waiters,
            resident_bytes: state.resident_bytes,
            sessions_opened: state.sessions_opened,
            sessions_closed: state.sessions_closed,
            sessions_reaped: state.sessions_reaped,
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
        let drained: usize = state.idle.values().map(Vec::len).sum();
        state.idle.clear();
        state.resident_bytes = 0;
        state.sessions_closed += drained;
        drained
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
            .expect("pooled session present until drop")
    }
}

impl<R: EmbeddingRuntime, C: MonotonicClock> DerefMut for PooledSession<R, C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.session
            .as_mut()
            .expect("pooled session present until drop")
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
            drop(state);
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
        }
    }
}

/// Decrements the queued-waiter count when a blocking acquisition exits for
/// any reason.
struct WaiterPermit<R: EmbeddingRuntime, C: MonotonicClock> {
    inner: Arc<PoolInner<R, C>>,
}

impl<R: EmbeddingRuntime, C: MonotonicClock> Drop for WaiterPermit<R, C> {
    fn drop(&mut self) {
        let mut state = self.inner.lock_state();
        state.queued_waiters = state.queued_waiters.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::Ordering as AtomicOrdering;
    use std::thread;

    use super::*;
    // Two `super` steps resolve to the directory module that holds both
    // packet files in every layout (scratch `#[path]` crate root and the
    // integrated `src/semantic_code/` module alike).
    use super::super::fastembed_adapter::{
        BoundedSanitizedTextBatchV1, EmbeddingMetricV1, EmbeddingNormalizationV1,
        EmbeddingPrecisionV1, FakeEmbeddingRuntime, ManualCancellation, RuntimeFailureKindV1,
    };

    fn artifact() -> VerifiedEmbeddingArtifactV1 {
        VerifiedEmbeddingArtifactV1 {
            artifact_digest: "aa55aa55aa55aa55".to_string(),
            model_root: PathBuf::from("/plan02-store/artifacts/aa55"),
            model_file: "model.onnx".to_string(),
            tokenizer_file: "tokenizer.json".to_string(),
            config_file: "config.json".to_string(),
            dimensions: 8,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::F32,
            query_instruction: None,
            document_instruction: None,
            max_batch_texts: 8,
            max_batch_bytes: 16 * 1024,
            resident_byte_ceiling: 64 * 1024 * 1024,
            runtime_build_revision: "fastembed-test-rev-1".to_string(),
        }
    }

    fn identity(domain: &str) -> SessionIdentityV1 {
        SessionIdentityV1::new("profile-digest-1", domain, 7)
    }

    fn config(max_sessions: usize, idle_timeout: Duration, ceiling: u64) -> SessionPoolConfigV1 {
        SessionPoolConfigV1 {
            max_sessions,
            max_queued_waiters: 4,
            idle_timeout,
            memory_ceiling_bytes: ceiling,
        }
    }

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
        let pool = fake_pool(2, Duration::from_secs(60), 1 << 20);
        let id = identity("domain-a");
        {
            let _guard = pool.acquire(&id, &artifact()).expect("first acquire");
            assert_eq!(pool.stats().active, 1);
        }
        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 1);
        assert_eq!(stats.sessions_opened, 1);
        {
            let _guard = pool.acquire(&id, &artifact()).expect("second acquire");
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
        let pool = fake_pool(1, Duration::from_secs(60), 1 << 20);
        let id = identity("domain-a");
        let _held = pool.acquire(&id, &artifact()).expect("first acquire");
        let result = pool.acquire(&id, &artifact());
        assert_eq!(
            result.err(),
            Some(SessionAcquireError::Exhausted { active: 1, max: 1 })
        );
        drop(_held);
        pool.acquire(&id, &artifact())
            .expect("acquire succeeds after release");
    }

    #[test]
    fn memory_ceiling_is_enforced_with_typed_error() {
        // Each fake session reports 1024 resident bytes; ceiling allows one.
        let pool = fake_pool(4, Duration::from_secs(60), 1536);
        let id = identity("domain-a");
        let _held = pool.acquire(&id, &artifact()).expect("first acquire");
        let result = pool.acquire(&id, &artifact());
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
    }

    #[test]
    fn identity_separation_blocks_cross_privacy_reuse() {
        let pool = fake_pool(4, Duration::from_secs(60), 1 << 20);
        let artifact = artifact();
        {
            let _guard = pool.acquire(&identity("domain-a"), &artifact).expect("a");
        }
        let _b = pool
            .acquire(&identity("domain-b"), &artifact)
            .expect("distinct domain");
        let stats = pool.stats();
        assert_eq!(
            stats.sessions_opened, 2,
            "a privacy-domain change never reuses the other domain's session"
        );
        // Same domain, different key epoch also misses.
        let mut epoch_shifted = identity("domain-a");
        epoch_shifted.key_epoch = 8;
        let _c = pool.acquire(&epoch_shifted, &artifact).expect("epoch");
        assert_eq!(pool.stats().sessions_opened, 3);
        // Same identity as the first still hits its warmed session.
        let _d = pool.acquire(&identity("domain-a"), &artifact).expect("hit");
        assert_eq!(pool.stats().sessions_opened, 3);
    }

    #[test]
    fn artifact_descriptor_change_never_reuses_a_warmed_session() {
        let pool = fake_pool(2, Duration::from_secs(60), 1 << 20);
        let id = identity("domain-a");
        {
            let _guard = pool.acquire(&id, &artifact()).expect("first artifact");
        }
        let mut replacement = artifact();
        replacement.artifact_digest = "replacement-artifact-digest".to_string();
        let guard = pool
            .acquire(&id, &replacement)
            .expect("replacement artifact");
        assert_eq!(guard.descriptor(), &replacement);
        assert_eq!(
            pool.stats().sessions_opened,
            2,
            "artifact descriptor changes cannot hit a stale warmed session"
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
            config(2, Duration::from_secs(60), 1 << 30),
        )
        .expect("valid config");
        let err = pool
            .acquire(&identity("domain-a"), &artifact())
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
        let mut artifact = artifact();
        artifact.resident_byte_ceiling = 1024;
        let pool = SessionPool::new(
            FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1025),
            ManualClock::new(),
            config(2, Duration::from_secs(60), 1 << 30),
        )
        .expect("valid config");
        let err = pool
            .acquire(&identity("domain-a"), &artifact)
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
        let id = identity("domain-a");
        {
            let _guard = pool.acquire(&id, &artifact()).expect("acquire");
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
    fn acquire_reaps_expired_idle_before_reuse() {
        let pool = fake_pool(2, Duration::from_secs(10), 1 << 20);
        let id = identity("domain-a");
        {
            let _guard = pool.acquire(&id, &artifact()).expect("acquire");
        }
        pool.inner.clock.advance(Duration::from_secs(11));
        let _guard = pool.acquire(&id, &artifact()).expect("second acquire");
        let stats = pool.stats();
        assert_eq!(stats.sessions_reaped, 1, "expired idle reaped on acquire");
        assert_eq!(stats.sessions_opened, 2, "a fresh session was opened");
    }

    #[test]
    fn runtime_open_failure_surfaces_as_typed_acquire_error() {
        let pool = SessionPool::new(
            FakeEmbeddingRuntime::new().with_open_failure(RuntimeFailureKindV1::OutOfMemory),
            ManualClock::new(),
            config(2, Duration::from_secs(60), 1 << 20),
        )
        .expect("valid config");
        let result = pool.acquire(&identity("domain-a"), &artifact());
        match result.err() {
            Some(SessionAcquireError::Open(EmbedError::Runtime(failure))) => {
                assert_eq!(failure.kind, RuntimeFailureKindV1::OutOfMemory)
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
            SystemMonotonicClock::default(),
            config(1, Duration::from_secs(60), 1 << 20),
        )
        .expect("valid config");
        let id = identity("domain-a");
        let held = pool.acquire(&id, &artifact()).expect("held");
        let cancel = ManualCancellation::new();
        thread::scope(|scope| {
            let waiting = scope
                .spawn(|| pool.acquire_blocking(&id, &artifact(), Duration::from_secs(5), &cancel));
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
    fn blocking_acquire_reports_deadline_on_injected_clock() {
        let pool = fake_pool(1, Duration::from_secs(60), 1 << 20);
        let id = identity("domain-a");
        let _held = pool.acquire(&id, &artifact()).expect("held");
        let cancel = ManualCancellation::new();
        thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                pool.acquire_blocking(&id, &artifact(), Duration::from_secs(10), &cancel)
            });
            while pool.stats().queued_waiters == 0 {
                thread::yield_now();
            }
            pool.inner.clock.advance(Duration::from_secs(11));
            let err = waiting.join().expect("no panic").err();
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
        let pool = fake_pool(1, Duration::from_secs(60), 1 << 20);
        let id = identity("domain-a");
        let _held = pool.acquire(&id, &artifact()).expect("held");
        let cancel = ManualCancellation::new();
        thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                pool.acquire_blocking(&id, &artifact(), Duration::from_secs(600), &cancel)
            });
            while pool.stats().queued_waiters == 0 {
                thread::yield_now();
            }
            cancel.cancel();
            let err = waiting.join().expect("no panic").err();
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
                idle_timeout: Duration::from_secs(60),
                memory_ceiling_bytes: 1 << 20,
            },
        )
        .expect("valid config");
        let id = identity("domain-a");
        let _held = pool.acquire(&id, &artifact()).expect("held");
        let cancel = ManualCancellation::new();
        thread::scope(|scope| {
            let waiting = scope
                .spawn(|| pool.acquire_blocking(&id, &artifact(), Duration::from_secs(5), &cancel));
            while pool.stats().queued_waiters == 0 {
                thread::yield_now();
            }
            let err = pool
                .acquire_blocking(&id, &artifact(), Duration::from_secs(5), &cancel)
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
        let pool = fake_pool(2, Duration::from_secs(60), 1 << 20);
        let id = identity("domain-a");
        {
            let _guard = pool.acquire(&id, &artifact()).expect("acquire");
        }
        assert_eq!(pool.stats().idle, 1);
        assert_eq!(pool.close(), 1, "one idle session closed");
        let stats = pool.stats();
        assert!(stats.closed);
        assert_eq!(stats.sessions_closed, 1);
        assert_eq!(stats.resident_bytes, 0);
        assert_eq!(
            pool.acquire(&id, &artifact()).err(),
            Some(SessionAcquireError::Closed)
        );
        assert_eq!(pool.close(), 0, "close is idempotent");
    }

    #[test]
    fn active_session_closes_on_release_after_pool_close() {
        let pool = fake_pool(2, Duration::from_secs(60), 1 << 20);
        let id = identity("domain-a");
        let guard = pool.acquire(&id, &artifact()).expect("acquire");
        assert_eq!(pool.close(), 0);
        drop(guard);
        let stats = pool.stats();
        assert_eq!(stats.active, 0);
        assert_eq!(stats.idle, 0);
        assert_eq!(stats.sessions_closed, 1);
        assert_eq!(stats.resident_bytes, 0);
    }

    #[test]
    fn pooled_guard_derefs_to_session_and_embeds() {
        let pool = fake_pool(1, Duration::from_secs(60), 1 << 20);
        let id = identity("domain-a");
        let mut guard = pool.acquire(&id, &artifact()).expect("acquire");
        assert_eq!(guard.identity(), &id);
        assert_eq!(
            guard.descriptor().artifact_digest,
            artifact().artifact_digest,
            "session echoes its verified-artifact descriptor"
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
        let id = identity("domain-a");
        {
            let _g = pool.acquire(&id, &artifact()).expect("one");
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
}
