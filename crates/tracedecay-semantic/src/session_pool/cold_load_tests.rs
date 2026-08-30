use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::fastembed_adapter::{
    AdmittedProjectionArtifactV1, EmbedError, EmbeddingRuntime, FakeEmbeddingRuntime,
    FakeEmbeddingSession,
};
use crate::session_pool::test_support::{authority, authority_with_load_deadline_ms, config};
use crate::session_pool::{
    ManualClock, SessionAcquireError, SessionPool, SessionPoolStats, SystemMonotonicClock,
};

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
    ) -> Result<Self::Session, EmbedError> {
        self.clock.advance(self.load_time);
        self.inner.open_session(authority)
    }
}

#[test]
fn cold_session_open_is_measured_before_warm_reuse() {
    let clock = Arc::new(ManualClock::new());
    let pool = SessionPool::new(
        AdvancingOpenRuntime {
            inner: FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
            clock: Arc::clone(&clock),
            load_time: Duration::from_millis(25),
        },
        Arc::clone(&clock),
        config(1, Duration::from_mins(1), 1 << 20),
    )
    .expect("valid config");

    let session = pool.acquire(&authority()).expect("cold session");
    drop(session);
    let session = pool.acquire(&authority()).expect("warm session");
    drop(session);

    assert_eq!(pool.stats().sessions_opened, 1);
    assert_eq!(pool.stats().last_cold_load_micros, Some(25_000));
}

/// Fake runtime whose `open_session` blocks until the test releases it, so a
/// test can hold a real cold load in flight across the artifact deadline.
struct GatedOpenRuntime {
    inner: FakeEmbeddingRuntime,
    gate: Mutex<Receiver<()>>,
}

impl EmbeddingRuntime for GatedOpenRuntime {
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
    ) -> Result<Self::Session, EmbedError> {
        self.gate
            .lock()
            .expect("gate lock")
            .recv()
            .expect("gate release signal");
        self.inner.open_session(authority)
    }
}

#[test]
fn load_deadline_fires_while_the_open_is_still_running() {
    let (release, gate) = channel();
    let pool = SessionPool::new(
        GatedOpenRuntime {
            inner: FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024),
            gate: Mutex::new(gate),
        },
        SystemMonotonicClock::default(),
        config(1, Duration::from_mins(1), 1 << 20),
    )
    .expect("valid config");
    let authority = authority_with_load_deadline_ms(50);

    let releaser = thread::spawn(move || {
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
