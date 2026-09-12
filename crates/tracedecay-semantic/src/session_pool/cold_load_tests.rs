#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use tracedecay_runtime_core::resident_memory::sampled_process_resident_bytes_v1;

use crate::fastembed_adapter::{
    AdmittedProjectionArtifactV1, EmbedError, EmbeddingRuntime, FakeEmbeddingRuntime,
    FakeEmbeddingSession, SemanticExecutionAuthority, SemanticExecutionInterruptionV1,
};
use crate::session_pool::test_support::{
    admitted_artifact_limits, authority, authority_with_load_deadline_ms, config, projection_for,
};
use crate::session_pool::{
    ManualClock, ResidentBytesSamplerV1, SessionAcquireError, SessionPool, SessionPoolStats,
    SystemMonotonicClock,
};

fn authority_with_resident_ceiling(
    ceiling_bytes: u64,
    load_deadline_ms: u64,
) -> AdmittedProjectionArtifactV1 {
    let artifact = admitted_artifact_limits(5, 9, ceiling_bytes, load_deadline_ms);
    let projection = projection_for(&artifact)
        .admit()
        .expect("valid projection fixture");
    AdmittedProjectionArtifactV1::admit(&artifact, &projection)
        .expect("matching projection and artifact")
}

/// Wait until the abandoned loader observes the fired interruption, aborts,
/// and releases its slot and reservation, or fail with the stuck stats.
fn expect_aborted_load_release(pool_stats: impl Fn() -> SessionPoolStats) {
    let give_up_at = Instant::now() + Duration::from_secs(10);
    loop {
        let stats = pool_stats();
        if stats.active == 0 {
            assert_eq!(stats.resident_bytes, 0);
            assert_eq!(
                stats.sessions_opened, 0,
                "an aborted open must never produce a session"
            );
            return;
        }
        assert!(
            Instant::now() < give_up_at,
            "abandoned load never released its slot: {stats:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

struct AdvancingOpenRuntime {
    inner: FakeEmbeddingRuntime,
    clock: Arc<ManualClock>,
    load_time: Duration,
}

impl EmbeddingRuntime for AdvancingOpenRuntime {
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
fn load_deadline_fires_while_the_open_is_still_running() {
    let (entered_tx, entered_rx) = channel();
    let (release, gate) = channel();
    let pool = SessionPool::new(
        ReportingGatedOpenRuntime {
            inner: FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
            entered: entered_tx,
            label: "deadline",
            gate: Mutex::new(gate),
        },
        SystemMonotonicClock::default(),
        config(1, Duration::from_mins(1), 1 << 20),
    )
    .expect("valid config");
    let authority = authority_with_load_deadline_ms(50);

    // Release two seconds into the load itself: the open may first wait for
    // another cold open's turn, and that wait is not load time.
    let releaser = thread::spawn(move || {
        entered_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the gated open starts");
        thread::sleep(Duration::from_millis(2_000));
        release.send(()).expect("release the gated open");
    });

    let error = match pool.acquire(&authority) {
        Err(error) => error,
        Ok(_session) => panic!("deadline must fire during the load"),
    };
    let SessionAcquireError::LoadDeadlineExceeded { elapsed, deadline } = error else {
        panic!("expected LoadDeadlineExceeded, got {error:?}");
    };
    assert_eq!(deadline, Duration::from_millis(50));
    // The prior implementation only checked the deadline after `open_session`
    // returned (~2 s here); the bound must fire at the deadline instead.
    assert!(
        elapsed < Duration::from_millis(1_500),
        "deadline fired only after the load returned: {elapsed:?}"
    );

    // The abandoned load still holds its slot and byte reservation while the
    // runtime genuinely occupies memory.
    let held = pool.stats();
    assert_eq!(
        held.active, 1,
        "abandoned load must keep its slot: {held:?}"
    );
    assert_eq!(held.resident_bytes, 1024);
    assert_eq!(held.sessions_opened, 0);

    releaser.join().expect("releaser thread");
    let give_up_at = Instant::now() + Duration::from_secs(10);
    loop {
        let stats = pool.stats();
        if stats.active == 0 {
            assert_eq!(stats.sessions_opened, 1);
            assert_eq!(stats.sessions_closed, 1);
            assert_eq!(stats.resident_bytes, 0);
            let micros = stats
                .last_cold_load_micros
                .expect("completed discarded open recorded");
            assert!(micros >= 1_500_000, "expected a ~2 s load, got {micros}");
            break;
        }
        assert!(
            Instant::now() < give_up_at,
            "abandoned load never released its slot: {stats:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn cold_session_exceeding_artifact_deadline_is_discarded() {
    let clock = Arc::new(ManualClock::new());
    let pool = SessionPool::new(
        AdvancingOpenRuntime {
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
    assert_eq!(
        pool.stats(),
        SessionPoolStats {
            sessions_opened: 1,
            sessions_closed: 1,
            last_cold_load_micros: Some(30_001_000),
            ..SessionPoolStats::default()
        }
    );
}

/// Fake runtime whose `open_session` waits at a stage boundary: it polls the
/// pool's interruption signal and returns the typed error the moment the
/// signal fires, exactly like the production adapter's between-stage checks.
struct InterruptionPollingOpenRuntime {
    inner: FakeEmbeddingRuntime,
}

impl EmbeddingRuntime for InterruptionPollingOpenRuntime {
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
        _authority: &AdmittedProjectionArtifactV1,
        interruption: &dyn SemanticExecutionAuthority,
    ) -> Result<Self::Session, EmbedError> {
        let give_up_at = Instant::now() + Duration::from_secs(30);
        loop {
            match interruption.interruption() {
                Some(SemanticExecutionInterruptionV1::Cancelled) => {
                    return Err(EmbedError::Cancelled);
                }
                Some(SemanticExecutionInterruptionV1::DeadlineExceeded) => {
                    return Err(EmbedError::DeadlineExceeded);
                }
                None => {
                    assert!(
                        Instant::now() < give_up_at,
                        "the pool never fired the load interruption signal"
                    );
                    thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }
}

#[test]
fn deadline_abandoned_load_aborts_at_the_runtime_stage_boundary() {
    let pool = SessionPool::new(
        InterruptionPollingOpenRuntime {
            inner: FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
        },
        SystemMonotonicClock::default(),
        config(1, Duration::from_mins(1), 1 << 20),
    )
    .expect("valid config");
    let authority = authority_with_load_deadline_ms(50);

    let error = pool.acquire(&authority).err().expect("deadline fires");
    assert!(
        matches!(error, SessionAcquireError::LoadDeadlineExceeded { .. }),
        "expected LoadDeadlineExceeded, got {error:?}"
    );
    // The fired signal reaches the loader at its next stage boundary, which
    // releases the slot and reservation without an uncancellable load ever
    // running to completion.
    expect_aborted_load_release(|| pool.stats());
}

#[test]
fn resident_ceiling_breach_fails_typed_while_the_load_is_still_running() {
    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;
    let sampler_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&sampler_calls);
    // Baseline 1 GiB at load start; every in-flight sample reports 128 MiB of
    // growth against a 64 MiB declared resident ceiling.
    let sampler: ResidentBytesSamplerV1 = Arc::new(move || {
        let call = calls.fetch_add(1, Ordering::SeqCst);
        Some(if call == 0 { GIB } else { GIB + 128 * MIB })
    });
    let pool = SessionPool::with_resident_sampler(
        InterruptionPollingOpenRuntime {
            inner: FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
        },
        SystemMonotonicClock::default(),
        config(1, Duration::from_mins(1), 1 << 30),
        sampler,
    )
    .expect("valid config");
    let authority = authority_with_resident_ceiling(64 * MIB, 30_000);

    let started = Instant::now();
    let error = pool.acquire(&authority).err().expect("resident breach");
    assert_eq!(
        error,
        SessionAcquireError::ResidentCeilingExceeded {
            tracked_resident_bytes: 0,
            observed_growth_bytes: 128 * MIB,
            ceiling_bytes: 64 * MIB,
        }
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the measured bound must fire from in-flight sampling, not the 30 s deadline"
    );
    assert!(
        sampler_calls.load(Ordering::SeqCst) >= 2,
        "the breach verdict requires a baseline and at least one in-flight sample"
    );
    // The same signal aborts the loader at its next stage boundary.
    expect_aborted_load_release(|| pool.stats());
}

/// Runtime whose first session opens normally while the second waits for the
/// pool's interruption. This models a cold open overlapping a retained session
/// so actual load growth must be charged against the remaining pool budget.
struct SecondOpenPollingRuntime {
    inner: FakeEmbeddingRuntime,
    opens: AtomicUsize,
    resident_bytes_per_session: u64,
}

impl EmbeddingRuntime for SecondOpenPollingRuntime {
    type Session = FakeEmbeddingSession;

    fn resident_bytes_reservation(&self, _authority: &AdmittedProjectionArtifactV1) -> u64 {
        self.resident_bytes_per_session
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
        if self.opens.fetch_add(1, Ordering::SeqCst) == 0 {
            return self.inner.open_session(authority, interruption);
        }
        let give_up_at = Instant::now() + Duration::from_secs(5);
        loop {
            match interruption.interruption() {
                Some(SemanticExecutionInterruptionV1::Cancelled) => {
                    return Err(EmbedError::Cancelled);
                }
                Some(SemanticExecutionInterruptionV1::DeadlineExceeded) => {
                    return Err(EmbedError::DeadlineExceeded);
                }
                None => {
                    assert!(
                        Instant::now() < give_up_at,
                        "second open never received pool interruption"
                    );
                    thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }
}

#[test]
fn measured_growth_is_charged_against_already_retained_sessions() {
    const MIB: u64 = 1 << 20;
    const POOL_CEILING: u64 = 128 * MIB;
    const RETAINED_SESSION: u64 = 64 * MIB;
    const SECOND_LOAD_GROWTH: u64 = 96 * MIB;

    let sampler_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&sampler_calls);
    let sampler: ResidentBytesSamplerV1 = Arc::new(move || {
        let call = calls.fetch_add(1, Ordering::SeqCst);
        Some(if call < 2 {
            1 << 30
        } else {
            (1 << 30) + SECOND_LOAD_GROWTH
        })
    });
    let pool = SessionPool::with_resident_sampler(
        SecondOpenPollingRuntime {
            inner: FakeEmbeddingRuntime::new().with_resident_bytes_per_session(RETAINED_SESSION),
            opens: AtomicUsize::new(0),
            resident_bytes_per_session: RETAINED_SESSION,
        },
        SystemMonotonicClock::default(),
        config(2, Duration::from_mins(1), POOL_CEILING),
        sampler,
    )
    .expect("valid config");
    let authority = authority_with_resident_ceiling(POOL_CEILING, 500);

    let held = pool.acquire(&authority).expect("first retained session");
    let error = pool
        .acquire(&authority)
        .err()
        .expect("overlapping growth exceeds the remaining pool budget");
    assert!(
        matches!(
            error,
            SessionAcquireError::ResidentCeilingExceeded {
                tracked_resident_bytes: RETAINED_SESSION,
                observed_growth_bytes: SECOND_LOAD_GROWTH,
                ceiling_bytes: POOL_CEILING,
            }
        ),
        "expected measured resident enforcement, got {error:?}"
    );
    let give_up_at = Instant::now() + Duration::from_secs(5);
    loop {
        let stats = pool.stats();
        if stats.active == 1 && stats.resident_bytes == RETAINED_SESSION {
            break;
        }
        assert!(
            Instant::now() < give_up_at,
            "interrupted second load did not release its reservation: {stats:?}"
        );
        thread::sleep(Duration::from_millis(2));
    }
    drop(held);
    assert_eq!(pool.close(), 1, "the retained first session is drained");
    assert_eq!(pool.stats().resident_bytes, 0);
}

/// Gated runtime that first allocates and touches a synthetic corpus-sized
/// buffer — the stand-in for ORT's transient graph parse/optimization/arena
/// growth — and then holds it live until released.
struct AllocatingOpenRuntime {
    inner: FakeEmbeddingRuntime,
    gate: Mutex<Receiver<()>>,
    allocation_bytes: usize,
}

impl EmbeddingRuntime for AllocatingOpenRuntime {
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
        // Non-zero fill so every page is written and resident, not a shared
        // zero page.
        let synthetic_corpus = vec![0xA5_u8; self.allocation_bytes];
        std::hint::black_box(&synthetic_corpus);
        self.gate
            .lock()
            .expect("gate lock")
            .recv()
            .expect("gate release signal");
        drop(synthetic_corpus);
        self.inner.open_session(authority, interruption)
    }
}

/// The production kernel sampler observes a synthetic corpus-sized transient
/// allocation inside the load and fails typed against the declared ceiling.
#[cfg(target_os = "linux")]
#[test]
fn measured_resident_growth_beyond_the_ceiling_fails_typed_under_a_synthetic_corpus() {
    const MIB: u64 = 1 << 20;
    let (release, gate) = channel();
    // The kernel sampler reads the whole test process. Cold opens take turns,
    // so this load starts right after another test's real model load returned
    // — and that test drops its session inside this window, which would mask
    // the synthetic growth behind a larger foreign release. Report growth from
    // the lowest RSS seen instead, so only this load's own allocation moves
    // the verdict while the kernel surface stays the one being read.
    let baseline = Arc::new(AtomicU64::new(0));
    let floor = Arc::new(AtomicU64::new(u64::MAX));
    let sampler: ResidentBytesSamplerV1 = Arc::new(move || {
        let now = sampled_process_resident_bytes_v1()?;
        let _ = baseline.compare_exchange(0, now, Ordering::SeqCst, Ordering::SeqCst);
        let floor = floor.fetch_min(now, Ordering::SeqCst).min(now);
        Some(
            baseline
                .load(Ordering::SeqCst)
                .saturating_add(now.saturating_sub(floor)),
        )
    });
    let pool = SessionPool::with_resident_sampler(
        AllocatingOpenRuntime {
            inner: FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
            gate: Mutex::new(gate),
            allocation_bytes: 96 * 1024 * 1024,
        },
        SystemMonotonicClock::default(),
        config(1, Duration::from_mins(1), 1 << 30),
        sampler,
    )
    .expect("valid config");
    // 8 MiB declared resident ceiling; the synthetic build grows ~96 MiB.
    let authority = authority_with_resident_ceiling(8 * MIB, 30_000);

    let started = Instant::now();
    let error = pool.acquire(&authority).err().expect("measured breach");
    let SessionAcquireError::ResidentCeilingExceeded {
        tracked_resident_bytes,
        observed_growth_bytes,
        ceiling_bytes,
    } = error
    else {
        panic!("expected ResidentCeilingExceeded, got {error:?}");
    };
    assert_eq!(tracked_resident_bytes, 0);
    assert_eq!(ceiling_bytes, 8 * MIB);
    assert!(
        observed_growth_bytes > ceiling_bytes,
        "observed growth {observed_growth_bytes} must exceed the {ceiling_bytes} ceiling"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the measured bound must fire well before the 30 s deadline"
    );
    release.send(()).expect("release the gated open");
    // Past its uncancellable stage, the loader observes the fired signal at
    // the next boundary, aborts, and releases the slot and reservation.
    expect_aborted_load_release(|| pool.stats());
}

/// Gated runtime that reports the moment its `open_session` starts, so a
/// test can prove which cold opens are in flight at once.
struct ReportingGatedOpenRuntime {
    inner: FakeEmbeddingRuntime,
    entered: Sender<&'static str>,
    label: &'static str,
    gate: Mutex<Receiver<()>>,
}

impl EmbeddingRuntime for ReportingGatedOpenRuntime {
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
        _interruption: &dyn SemanticExecutionAuthority,
    ) -> Result<Self::Session, EmbedError> {
        self.entered.send(self.label).expect("report open entry");
        self.gate
            .lock()
            .expect("gate lock")
            .recv()
            .expect("gate release signal");
        self.inner.open_session(
            authority,
            &crate::fastembed_adapter::ManualCancellation::new(),
        )
    }
}

/// Cold opens take turns across pools. The resident bound is measured on the
/// whole process, so a second open that ran alongside the first would inflate
/// the first's observed growth (and its own) with a load that is not its own;
/// two routes of one project loading the same model both breached a ceiling
/// each fits under alone.
#[test]
fn cold_opens_across_pools_take_turns() {
    let (entered_tx, entered_rx) = channel();
    let (release_first, first_gate) = channel();
    let (release_second, second_gate) = channel();
    // Flat RSS series: turn-taking, not measured growth, is under test.
    let pool = |label, gate| {
        SessionPool::with_resident_sampler(
            ReportingGatedOpenRuntime {
                inner: FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
                entered: entered_tx.clone(),
                label,
                gate: Mutex::new(gate),
            },
            SystemMonotonicClock::default(),
            config(1, Duration::from_mins(1), 1 << 30),
            Arc::new(|| Some(1 << 30)),
        )
        .expect("valid config")
    };
    let first = pool("first", first_gate);
    let second = pool("second", second_gate);
    let authority = authority_with_resident_ceiling(1 << 30, 30_000);

    thread::scope(|scope| {
        let first_open = scope.spawn(|| first.acquire(&authority));
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(90))
                .expect("the first cold open starts"),
            "first"
        );
        let second_open = scope.spawn(|| second.acquire(&authority));
        assert!(
            entered_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "a second cold open must wait for the in-flight one, not run beside it"
        );
        release_first.send(()).expect("release the first open");
        first_open
            .join()
            .expect("first acquisition thread")
            .expect("first cold session");
        // Another test's cold open may take the freed turn first and hold it
        // for up to its own load deadline; the second open follows it.
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(90))
                .expect("the second cold open starts once the first returned"),
            "second"
        );
        release_second.send(()).expect("release the second open");
        second_open
            .join()
            .expect("second acquisition thread")
            .expect("second cold session");
    });
    assert_eq!(first.stats().sessions_opened, 1);
    assert_eq!(second.stats().sessions_opened, 1);
}

/// The published FastEmbed constructor cannot be interrupted mid-load, so a
/// caller's typed deadline returns while ORT is still building the graph.
/// The process-wide cold-load turn must stay with that native load until it
/// exits: a second cold open that started beside it would double the
/// transient peak the turn exists to cap.
#[test]
fn abandoned_native_load_keeps_the_cold_load_turn_until_it_exits() {
    let (entered_tx, entered_rx) = channel();
    let (release_first, first_gate) = channel();
    let (release_second, second_gate) = channel();
    let pool = |label, gate| {
        SessionPool::with_resident_sampler(
            ReportingGatedOpenRuntime {
                inner: FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
                entered: entered_tx.clone(),
                label,
                gate: Mutex::new(gate),
            },
            SystemMonotonicClock::default(),
            config(1, Duration::from_mins(1), 1 << 30),
            Arc::new(|| Some(1 << 30)),
        )
        .expect("valid config")
    };
    let first = pool("first", first_gate);
    let second = pool("second", second_gate);
    let abandoned_authority = authority_with_resident_ceiling(1 << 30, 50);
    let authority = authority_with_resident_ceiling(1 << 30, 30_000);

    thread::scope(|scope| {
        // The first open blocks inside its "constructor" past its 50 ms
        // deadline; the caller gets the typed deadline while the load runs.
        let error = first
            .acquire(&abandoned_authority)
            .err()
            .expect("the first cold open exceeds its load deadline");
        assert!(
            matches!(error, SessionAcquireError::LoadDeadlineExceeded { .. }),
            "expected LoadDeadlineExceeded, got {error:?}"
        );
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the first cold open started"),
            "first"
        );
        let held = first.stats();
        assert_eq!(held.active, 1, "abandoned load keeps its slot: {held:?}");
        assert_eq!(held.resident_bytes, 1024);

        // A second cold open must wait for the abandoned native load to exit,
        // not run beside it because the caller's deadline already returned.
        let second_open = scope.spawn(|| second.acquire(&authority));
        assert!(
            entered_rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "a second cold open began while an abandoned native load was still running"
        );

        release_first
            .send(())
            .expect("release the abandoned first open");
        // Another test's cold open may take the freed turn first; the second
        // open follows once the abandoned loader has genuinely exited.
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(90))
                .expect("the second cold open starts once the abandoned load exited"),
            "second"
        );
        let released = first.stats();
        assert_eq!(
            released.active, 0,
            "abandoned load released its slot before the next turn: {released:?}"
        );
        assert_eq!(released.resident_bytes, 0);
        assert_eq!(released.sessions_opened, 1);
        assert_eq!(released.sessions_closed, 1);

        release_second.send(()).expect("release the second open");
        second_open
            .join()
            .expect("second acquisition thread")
            .expect("second cold session");
    });
    assert_eq!(second.stats().sessions_opened, 1);
}
