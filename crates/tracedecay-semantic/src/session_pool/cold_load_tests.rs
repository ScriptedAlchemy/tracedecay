use std::sync::Arc;
use std::time::Duration;

use crate::fastembed_adapter::{
    AdmittedProjectionArtifactV1, EmbedError, EmbeddingRuntime, FakeEmbeddingRuntime,
    FakeEmbeddingSession,
};
use crate::session_pool::test_support::{authority, config};
use crate::session_pool::{ManualClock, SessionAcquireError, SessionPool, SessionPoolStats};

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
