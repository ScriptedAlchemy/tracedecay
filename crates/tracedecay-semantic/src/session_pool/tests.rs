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
