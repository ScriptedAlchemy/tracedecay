//! Cover-past contract tests for the shared JSONL admission seam.
//!
//! The seam has exactly two dispositions for an admitted frame that fails:
//!
//! 1. A deterministic content refusal covers past the frame with a durable
//!    typed reason (`ObservationIdentityCollision` for that exact refusal,
//!    otherwise `AdmissionRefused`) so the stream converges.
//! 2. Everything else — store commit/read-back failures, unbound authorities,
//!    retryable races — is a typed [`TranscriptIngestError::HostAdmission`]
//!    block: the frontier does not advance and no coverage is written over a
//!    record whose durable fate is unknown.
//!
//! Exact duplicates (same identity + digest) are idempotent no-op receipts on
//! the persist path and never reach either disposition.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceIdentityV1, ProjectId, ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::observation::{
    CursorAdvanceOutcome, ObservationCoverageReason, ObservationCursorAdvance,
    ObservationIdentityCollisionDispositionV1,
};
use tracedecay_store::{ObservationBatchFallbackCause, ParseOffset};

use crate::admission::test_support::MemoryHostAdmission;
use crate::admission::{
    AdmissionFuture, HostAdmission, HostAdmissionOutcome, HostProjectionDrainOutcome,
};
use crate::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::runtime::codex::{
    try_admit_codex_jsonl_observations_for_profile_with_admission,
    try_admit_codex_jsonl_observations_for_project_with_admission,
};
use crate::runtime::shared::StoredCursor;
use crate::runtime::source::{JsonlResumeState, TranscriptIngestError};

/// Wraps [`MemoryHostAdmission`] so a test can script the capture verdict and
/// observe every cover-past cursor write the seam attempts.
#[derive(Default)]
struct SeamSpyAdmission {
    inner: MemoryHostAdmission,
    scripted_capture_error: Mutex<Option<HostAdmissionOutcome>>,
    scripted_capture_error_once: Mutex<Option<HostAdmissionOutcome>>,
    scripted_batch_error: Mutex<Option<HostAdmissionOutcome>>,
    report_no_cursor: AtomicBool,
    capture_calls: AtomicU64,
    capture_collision_dispositions: Mutex<Vec<ObservationIdentityCollisionDispositionV1>>,
    cover_past_advances: Mutex<Vec<ObservationCursorAdvance>>,
}

#[test]
fn install_shared_jsonl_preparation_authority_is_idempotent_across_memory_arcs() {
    use std::num::NonZeroU64;
    use tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1;

    super::install_test_shared_jsonl_preparation_authority();
    let other = std::sync::Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(64 * 1024 * 1024).expect("nonzero JSONL fixture budget"),
    ));
    super::install_shared_jsonl_preparation_authority(other).expect(
        "a second installer with a distinct memory Arc must not poison the process-wide authority",
    );
}

#[tokio::test]
async fn shared_jsonl_page_reuses_one_bounded_scan() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::TempDir::new().expect("temp directory");
    let path = temp.path().join("shared.jsonl");
    std::fs::write(&path, b"{}\n").expect("JSONL fixture");
    let _pin = super::pin_shared_jsonl_paths(std::slice::from_ref(&path));

    let (first, first_hit) =
        super::shared_jsonl_page(&path, StoredCursor::default(), Some(1024), None, true)
            .await
            .expect("initial shared page");
    let (second, second_hit) =
        super::shared_jsonl_page(&path, StoredCursor::default(), Some(1024), None, true)
            .await
            .expect("cached shared page");

    assert!(!first_hit);
    assert!(second_hit);
    assert!(std::sync::Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn shared_jsonl_page_precomputes_codex_context_hints_once() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::TempDir::new().expect("temp directory");
    let path = temp.path().join("context-hints.jsonl");
    std::fs::write(
        &path,
        b"{\"type\":\"event_msg\"}\n{\"type\":\"turn_context\"}\n",
    )
    .expect("JSONL fixture");
    let _pin = super::pin_shared_jsonl_paths(std::slice::from_ref(&path));

    let (first, _) =
        super::shared_jsonl_page(&path, StoredCursor::default(), Some(1024), None, true)
            .await
            .expect("initial shared page");
    let (second, hit) =
        super::shared_jsonl_page(&path, StoredCursor::default(), Some(1024), None, true)
            .await
            .expect("second shared consumer");

    assert!(hit);
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    assert_eq!(
        first
            .frames
            .iter()
            .map(|frame| frame.hints.may_change_codex_context)
            .collect::<Vec<_>>(),
        vec![false, true],
        "every consumer must reuse the page's one-sided O(1) context hint"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn shared_jsonl_page_keys_symlinks_by_canonical_source() {
    super::install_test_shared_jsonl_preparation_authority();
    use std::os::unix::fs::symlink;

    let temp = tempfile::TempDir::new().expect("temp directory");
    let path = temp.path().join("source.jsonl");
    let alias = temp.path().join("alias.jsonl");
    std::fs::write(&path, b"{}\n").expect("JSONL fixture");
    symlink(&path, &alias).expect("symlink fixture");
    let _pin = super::pin_shared_jsonl_paths(std::slice::from_ref(&path));

    let (first, _) =
        super::shared_jsonl_page(&path, StoredCursor::default(), Some(1024), None, true)
            .await
            .expect("source page");
    let (second, hit) =
        super::shared_jsonl_page(&alias, StoredCursor::default(), Some(1024), None, true)
            .await
            .expect("symlink page");

    assert!(hit);
    assert!(std::sync::Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn shared_jsonl_page_waiters_share_one_async_in_flight_read() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::TempDir::new().expect("temp directory");
    let path = temp.path().join("concurrent.jsonl");
    std::fs::write(&path, b"{}\n").expect("JSONL fixture");
    let _pin = super::pin_shared_jsonl_paths(std::slice::from_ref(&path));

    let first_path = path.clone();
    let second_path = path.clone();
    let (first, second) = tokio::join!(
        super::shared_jsonl_page(&first_path, StoredCursor::default(), Some(1024), None, true,),
        super::shared_jsonl_page(
            &second_path,
            StoredCursor::default(),
            Some(1024),
            None,
            true,
        )
    );
    let (first, first_hit) = first.expect("first concurrent page");
    let (second, second_hit) = second.expect("second concurrent page");

    assert_ne!(first_hit, second_hit);
    assert!(std::sync::Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn shared_jsonl_page_wait_is_operation_cancellable() {
    let temp = tempfile::TempDir::new().expect("temp directory");
    let path = temp.path().join("cancel-wait.jsonl");
    std::fs::write(&path, b"{}\n").expect("JSONL fixture");
    let key = super::SharedJsonlPageKey {
        path: std::fs::canonicalize(&path).expect("canonical fixture"),
        position: 0,
        generation: 0,
        max_new_bytes: Some(1024),
        resume: None,
        preparation: false.into(),
    };
    let cache = super::SHARED_JSONL_PAGE_CACHE.get_or_init(tokio::sync::Mutex::default);
    cache.lock().await.in_flight.insert(
        key.clone(),
        std::sync::Arc::new(super::SharedJsonlInFlight::new()),
    );
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let result = super::shared_jsonl_page_with_cancellation(
        &path,
        StoredCursor::default(),
        Some(1024),
        None,
        false,
        super::SharedJsonlCancellation {
            blocking: None,
            operation: Some(cancellation),
        },
        false,
    )
    .await;
    cache.lock().await.in_flight.remove(&key);

    assert!(matches!(
        result,
        Err(TranscriptIngestError::Cancelled { .. })
    ));
}

#[tokio::test]
async fn lazy_preparation_mutex_wait_is_operation_cancellable() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().expect("temp directory");
    let path = temp.path().join("cancel-lazy-mutex.jsonl");
    std::fs::write(&path, b"{}\n").expect("JSONL fixture");
    let (page, _) = super::shared_jsonl_page(
        &path,
        StoredCursor::default(),
        Some(1024),
        None,
        super::SharedJsonlFramePreparation::Lazy,
    )
    .await
    .expect("lazy page");
    let preparations_before = super::shared_jsonl_frame_preparations_for_test(page.file_identity);
    let held = page.lazy_preparation.lock().await;
    let task_page = Arc::clone(&page);
    let cancellation = ObservationCancellation::default();
    let task_cancellation = cancellation.clone();
    let preparation = tokio::spawn(async move {
        super::prepare_shared_jsonl_window(task_page.as_ref(), 0, &task_cancellation, "codex").await
    });
    tokio::task::yield_now().await;
    cancellation.cancel();

    let result = tokio::time::timeout(std::time::Duration::from_millis(100), preparation)
        .await
        .expect("cancelled mutex wait must settle before the holder releases")
        .expect("preparation task must join");
    drop(held);

    assert!(matches!(
        result,
        Err(TranscriptIngestError::Cancelled { provider: "codex" })
    ));
    assert_eq!(
        super::shared_jsonl_frame_preparations_for_test(page.file_identity),
        preparations_before,
        "a cancelled mutex waiter must never start frame preparation"
    );
}

#[tokio::test]
async fn lazy_preparation_cpu_wait_is_operation_cancellable() {
    use std::num::NonZeroUsize;

    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().expect("temp directory");
    let path = temp.path().join("cancel-lazy-cpu.jsonl");
    std::fs::write(&path, b"{}\n").expect("JSONL fixture");
    let (page, _) = super::shared_jsonl_page(
        &path,
        StoredCursor::default(),
        Some(1024),
        None,
        super::SharedJsonlFramePreparation::Lazy,
    )
    .await
    .expect("lazy page");
    let preparations_before = super::shared_jsonl_frame_preparations_for_test(page.file_identity);
    let background_cpu = tracedecay_private_fs::background_cpu::test_process_background_cpu(
        NonZeroUsize::new(1).expect("nonzero CPU width"),
    );
    let held = background_cpu.acquire();
    let task_page = Arc::clone(&page);
    let task_cpu = Arc::clone(&background_cpu);
    let cancellation = ObservationCancellation::default();
    let task_cancellation = cancellation.clone();
    let preparation = tokio::spawn(async move {
        super::prepare_shared_jsonl_window_with_background_cpu(
            task_page.as_ref(),
            0,
            &task_cancellation,
            "codex",
            task_cpu,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while background_cpu.waiting_work_units() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lazy preparation reached saturated CPU authority");
    cancellation.cancel();

    let result = tokio::time::timeout(std::time::Duration::from_millis(100), preparation)
        .await
        .expect("cancelled CPU wait must settle before capacity is released")
        .expect("preparation task must join");
    drop(held);

    assert!(matches!(
        result,
        Err(TranscriptIngestError::Cancelled { provider: "codex" })
    ));
    assert_eq!(background_cpu.waiting_work_units(), 0);
    assert_eq!(
        super::shared_jsonl_frame_preparations_for_test(page.file_identity),
        preparations_before,
        "a cancelled CPU waiter must never start frame preparation"
    );
}

#[tokio::test]
async fn aborting_a_prefetch_build_releases_waiters_and_speculative_capacity() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::TempDir::new().expect("temp directory");
    let path = temp.path().join("aborted-prefetch.jsonl");
    std::fs::write(&path, b"{}\n").expect("JSONL fixture");
    let canonical = std::fs::canonicalize(&path).expect("canonical fixture");
    let build_gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    super::SHARED_JSONL_BUILD_GATES
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap()
        .insert(path.clone(), std::sync::Arc::clone(&build_gate));
    let pin = super::pin_shared_jsonl_paths(std::slice::from_ref(&path));

    super::SHARED_JSONL_ACTIVE_BUILDS.store(0, Ordering::Release);
    pin.start_prefetches(std::slice::from_ref(&path));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while super::SHARED_JSONL_ACTIVE_BUILDS.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("prefetch reached the blocking build");

    let demand_path = path.clone();
    let demand = tokio::spawn(async move {
        super::shared_jsonl_page(
            &demand_path,
            StoredCursor::default(),
            Some(super::SHARED_JSONL_PAGE_MAX_NEW_BYTES),
            None,
            super::SharedJsonlFramePreparation::Lazy,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let joined = super::SHARED_JSONL_WAITER_REGISTRATIONS
                .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
                .lock()
                .unwrap()
                .get(&canonical)
                .copied()
                .unwrap_or_default();
            if joined != 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("same-key demand registered its waiter before producer abort");

    drop(pin);
    super::SHARED_JSONL_BUILD_GATES
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap()
        .remove(&path);
    tokio::task::spawn_blocking(move || build_gate.wait())
        .await
        .expect("release aborted build gate");
    tokio::time::timeout(std::time::Duration::from_secs(10), demand)
        .await
        .expect("joined same-key waiter must be notified after producer abort")
        .expect("same-key demand task")
        .expect("same-key demand rebuilds after prefetch cancellation");

    let cache = super::SHARED_JSONL_PAGE_CACHE
        .get()
        .expect("shared page cache")
        .lock()
        .await;
    assert!(
        cache.in_flight.keys().all(|key| key.path != canonical),
        "an aborted producer must not leave a stale waiter key"
    );
    assert!(
        cache
            .speculative_in_flight
            .iter()
            .all(|key| key.path != canonical),
        "an aborted producer must release the global speculative quota"
    );
}

#[tokio::test]
async fn unpinned_admission_page_defers_decode_to_the_scope_gate() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::TempDir::new().expect("temp directory");
    let path = temp.path().join("scope-first.jsonl");
    std::fs::write(&path, b"{\"type\":\"event_msg\"}\n").expect("JSONL fixture");

    let (page, _) =
        super::shared_jsonl_page(&path, StoredCursor::default(), Some(1024), None, true)
            .await
            .expect("standalone admission page");

    assert!(
        page.frames
            .iter()
            .all(|frame| frame.prepared.get().is_none()),
        "an unpinned standalone/replay page must not decode before its scope gate"
    );
    assert!(
        page._memory.is_some(),
        "raw replay pages must retain a process-memory reservation until their final Arc drops"
    );
}

#[tokio::test]
async fn generation_pin_prevents_slow_consumer_page_eviction() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::TempDir::new().expect("temp directory");
    let pinned_path = temp.path().join("pinned.jsonl");
    std::fs::write(&pinned_path, b"{}\n").expect("pinned JSONL fixture");
    let _pin = super::pin_shared_jsonl_paths(std::slice::from_ref(&pinned_path));
    let (pinned, initial_hit) = super::shared_jsonl_page(
        &pinned_path,
        StoredCursor::default(),
        Some(1024),
        None,
        true,
    )
    .await
    .expect("initial pinned page");
    assert!(!initial_hit);

    for index in 0..super::shared_jsonl_preparation_workers() + 2 {
        let path = temp.path().join(format!("eviction-{index:04}.jsonl"));
        std::fs::write(&path, b"{}\n").expect("eviction JSONL fixture");
        super::shared_jsonl_page(&path, StoredCursor::default(), Some(1024), None, true)
            .await
            .expect("eviction page");
    }

    let (replayed, hit) = super::shared_jsonl_page(
        &pinned_path,
        StoredCursor::default(),
        Some(1024),
        None,
        true,
    )
    .await
    .expect("replayed pinned page");
    assert!(hit);
    assert!(std::sync::Arc::ptr_eq(&pinned, &replayed));
}

#[tokio::test]
async fn exact_append_cursor_replaces_a_superseded_speculative_page() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::TempDir::new().expect("temp directory");
    let path = temp.path().join("appended.jsonl");
    std::fs::write(&path, b"{}\n").expect("initial JSONL fixture");
    let _pin = super::pin_shared_jsonl_paths(std::slice::from_ref(&path));

    let (prefetched, _) = super::shared_jsonl_page_with_cancellation(
        &path,
        StoredCursor::default(),
        Some(super::SHARED_JSONL_PAGE_MAX_NEW_BYTES),
        None,
        true,
        super::SharedJsonlCancellation::default(),
        true,
    )
    .await
    .expect("speculative page");
    let checkpoint = prefetched.frames.last().expect("initial frame");
    let previous = prefetched.new_cursor;
    let resume = JsonlResumeState {
        generation: previous.file_id,
        file_identity: prefetched.file_identity,
        fingerprint: checkpoint.resume_fingerprint,
    };
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append fixture");
    file.write_all(b"{\"type\":\"event_msg\"}\n")
        .expect("append JSONL frame");

    let (exact, first_hit) = super::shared_jsonl_page_with_cancellation(
        &path,
        previous,
        Some(super::SHARED_JSONL_PAGE_MAX_NEW_BYTES),
        Some(resume),
        true,
        super::SharedJsonlCancellation::default(),
        false,
    )
    .await
    .expect("exact append page");
    assert!(!first_hit);
    let (replayed, replay_hit) = super::shared_jsonl_page_with_cancellation(
        &path,
        previous,
        Some(super::SHARED_JSONL_PAGE_MAX_NEW_BYTES),
        Some(resume),
        true,
        super::SharedJsonlCancellation::default(),
        false,
    )
    .await
    .expect("replayed exact append page");
    assert!(replay_hit);
    assert!(Arc::ptr_eq(&exact, &replayed));
    assert!(!Arc::ptr_eq(&prefetched, &exact));
}

#[tokio::test]
async fn prepared_generation_uses_bounded_parallelism_and_retained_bytes() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::TempDir::new().expect("temp directory");
    let mut encoded = serde_json::to_vec(&json!({
        "payload": "x".repeat(768 * 1024),
    }))
    .expect("large JSON record");
    encoded.push(b'\n');
    let path_count = super::shared_jsonl_preparation_workers();
    let mut paths = Vec::new();
    for index in 0..path_count {
        let path = temp.path().join(format!("parallel-{index}.jsonl"));
        std::fs::write(&path, &encoded).expect("parallel JSONL fixture");
        paths.push(path);
    }
    let _pin = super::pin_shared_jsonl_paths(&paths);

    super::SHARED_JSONL_PEAK_BUILDS.store(0, Ordering::Release);
    let _prefetches = super::start_shared_jsonl_page_prefetch(&paths);
    for path in &paths {
        super::shared_jsonl_page(
            path,
            StoredCursor::default(),
            Some(super::SHARED_JSONL_PAGE_MAX_NEW_BYTES),
            None,
            super::SharedJsonlFramePreparation::Lazy,
        )
        .await
        .expect("prepared generation page");
    }

    assert_eq!(
        super::SHARED_JSONL_ACTIVE_BUILDS.load(Ordering::Acquire),
        0,
        "every completed page build must release exactly one active-build slot"
    );

    if std::thread::available_parallelism().is_ok_and(|cores| cores.get() > 8) {
        assert!(super::shared_jsonl_peak_builds_for_test() > 8);
    }
    let retained = super::SHARED_JSONL_PAGE_CACHE
        .get()
        .expect("shared page cache")
        .lock()
        .await
        .retained_bytes;
    assert!(
        retained
            <= u64::try_from(super::shared_jsonl_preparation_workers())
                .unwrap_or(u64::MAX)
                .saturating_mul(super::SHARED_JSONL_WORKER_RESERVATION_BYTES)
    );
}

#[test]
fn preparation_uses_the_daemon_installed_worker_width() {
    super::install_test_shared_jsonl_preparation_authority();
    assert_eq!(super::shared_jsonl_preparation_workers(), 48);
}

#[test]
fn preparation_preserves_configured_widths_above_sixty_four() {
    assert_eq!(super::shared_jsonl_preparation_workers_from(96), 96);
}

#[test]
fn preparation_width_backs_down_under_memory_pressure() {
    let reservation = super::SHARED_JSONL_WORKER_RESERVATION_BYTES;
    assert_eq!(
        super::shared_jsonl_preparation_capacity_from(48, reservation * 64, 0),
        48
    );
    assert_eq!(
        super::shared_jsonl_preparation_capacity_from(48, reservation * 8, reservation * 6),
        2
    );
}

#[test]
fn speculative_preparation_reserves_capacity_for_exact_cursor_demand() {
    assert_eq!(super::shared_jsonl_speculative_capacity_from(48), 47);
    assert_eq!(super::shared_jsonl_speculative_capacity_from(2), 1);
    assert_eq!(super::shared_jsonl_speculative_capacity_from(1), 0);
}

#[test]
fn small_lazy_pages_release_build_headroom_for_exact_demand() {
    use std::num::NonZeroU64;

    use tracedecay_runtime_core::resident_memory::{
        ProcessResidentMemoryV1, ResidentMemoryComponentIdV1,
    };

    super::install_test_shared_jsonl_preparation_authority();
    let reservation_bytes = super::SHARED_JSONL_WORKER_RESERVATION_BYTES;
    let memory = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(reservation_bytes * 2).expect("nonzero memory limit"),
    ));
    let component = ResidentMemoryComponentIdV1::new("sessions.codex.test-small-pages")
        .expect("canonical component id");
    let background_cpu = super::shared_jsonl_background_cpu().expect("background CPU authority");
    let temp = tempfile::tempdir().expect("temp directory");
    let mut pages = Vec::new();

    for ordinal in 0..8 {
        let path = temp.path().join(format!("small-{ordinal}.jsonl"));
        std::fs::write(&path, b"{}\n").expect("small JSONL page");
        let reservation = memory
            .reserve_process_shared(
                component,
                NonZeroU64::new(reservation_bytes).expect("nonzero reservation"),
            )
            .expect("small lazy pages must not exhaust demand headroom");
        pages.push(
            super::build_shared_jsonl_page(
                path,
                StoredCursor::default(),
                Some(1024),
                None,
                super::SharedJsonlBuildOptions {
                    prepare_frames: false,
                    background_cpu: Arc::clone(&background_cpu),
                    memory: Some(reservation),
                    cancellation: None,
                },
            )
            .expect("small lazy page"),
        );
    }

    assert!(
        memory.snapshot().used_bytes < 1024 * 1024,
        "raw page authority must retain measured bytes, not build reservations"
    );
    let exact_demand = memory
        .reserve_process_shared(
            component,
            NonZeroU64::new(reservation_bytes).expect("nonzero reservation"),
        )
        .expect("one exact-demand build reservation must remain available");
    drop(exact_demand);
    drop(pages);
    assert_eq!(memory.snapshot().used_bytes, 0);
}

#[tokio::test]
async fn lazy_preparation_retains_only_its_measured_memory_charge() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().expect("temp directory");
    let path = temp.path().join("lazy-memory.jsonl");
    std::fs::write(
        &path,
        b"{\"timestamp\":\"2026-01-01T00:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"measured\"}}\n",
    )
    .expect("JSONL fixture");
    let page = super::build_shared_jsonl_page(
        path,
        StoredCursor::default(),
        Some(1024),
        None,
        super::SharedJsonlBuildOptions {
            prepare_frames: false,
            background_cpu: super::shared_jsonl_background_cpu().expect("background CPU authority"),
            memory: super::reserve_shared_jsonl_page().expect("page reservation"),
            cancellation: None,
        },
    )
    .expect("lazy page");
    super::prepare_shared_jsonl_window(
        page.as_ref(),
        0,
        &ObservationCancellation::default(),
        "codex",
    )
    .await
    .expect("lazy preparation");

    let lazy_charge = page
        .lazy_memory
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(tracedecay_runtime_core::resident_memory::ProcessSharedMemoryReservationV1::reserved_bytes)
        .sum::<u64>();
    assert!(
        lazy_charge > 0,
        "prepared values require an authority charge"
    );
    assert!(
        lazy_charge < 1024 * 1024,
        "lazy preparation must shrink its bounded reservation to measured bytes"
    );
}

#[test]
fn speculative_capacity_is_one_global_quota_across_prefetch_generations() {
    let key = |name: &str| super::SharedJsonlPageKey {
        path: PathBuf::from(name),
        position: 0,
        generation: 0,
        max_new_bytes: Some(super::SHARED_JSONL_PAGE_MAX_NEW_BYTES),
        resume: None,
        preparation: true.into(),
    };
    let first = key("/generation-a.jsonl");
    let second = key("/generation-b.jsonl");
    let demand_slot = key("/exact-demand.jsonl");
    let mut cache = super::SharedJsonlPageCache::default();

    assert!(super::reserve_shared_jsonl_speculative_slot(
        &mut cache, &first, 2
    ));
    assert!(super::reserve_shared_jsonl_speculative_slot(
        &mut cache, &second, 2
    ));
    assert!(
        !super::reserve_shared_jsonl_speculative_slot(&mut cache, &demand_slot, 2),
        "a second prefetch generation cannot recompute and reuse occupied speculative slots"
    );
    assert_eq!(cache.speculative_in_flight.len(), 2);
}

impl SeamSpyAdmission {
    fn script_capture_error(&self, outcome: HostAdmissionOutcome) {
        *self.scripted_capture_error.lock().unwrap() = Some(outcome);
    }

    fn script_batch_error(&self, outcome: HostAdmissionOutcome) {
        *self.scripted_batch_error.lock().unwrap() = Some(outcome);
    }

    fn script_capture_error_once(&self, outcome: HostAdmissionOutcome) {
        *self.scripted_capture_error_once.lock().unwrap() = Some(outcome);
    }

    fn cover_past_advances(&self) -> Vec<ObservationCursorAdvance> {
        self.cover_past_advances.lock().unwrap().clone()
    }

    fn capture_count(&self) -> u64 {
        self.capture_calls.load(Ordering::Relaxed)
    }

    fn capture_collision_dispositions(&self) -> Vec<ObservationIdentityCollisionDispositionV1> {
        self.capture_collision_dispositions.lock().unwrap().clone()
    }
}

impl HostAdmission for SeamSpyAdmission {
    fn capture_observation<'a>(
        &'a self,
        request: CaptureObservationRequest,
    ) -> AdmissionFuture<'a, CaptureObservationOutcome> {
        Box::pin(async move {
            self.capture_calls.fetch_add(1, Ordering::Relaxed);
            self.capture_collision_dispositions
                .lock()
                .unwrap()
                .push(request.identity_collision_disposition());
            if let Some(outcome) = self.scripted_capture_error_once.lock().unwrap().take() {
                return Err(outcome);
            }
            if let Some(outcome) = self.scripted_capture_error.lock().unwrap().clone() {
                return Err(outcome);
            }
            self.inner.capture_observation(request).await
        })
    }

    fn capture_observations<'a>(
        &'a self,
        requests: Vec<CaptureObservationRequest>,
    ) -> AdmissionFuture<'a, Vec<CaptureObservationOutcome>> {
        Box::pin(async move {
            self.capture_collision_dispositions.lock().unwrap().extend(
                requests
                    .iter()
                    .map(CaptureObservationRequest::identity_collision_disposition),
            );
            if let Some(outcome) = self.scripted_batch_error.lock().unwrap().take() {
                return Err(outcome);
            }
            if let Some(outcome) = self.scripted_capture_error.lock().unwrap().clone() {
                // Fail the window without counting here so sequential
                // fallback still visits each frame exactly once.
                return Err(outcome);
            }
            self.inner.capture_observations(requests).await
        })
    }

    fn advance_non_durable_source_cursor<'a>(
        &'a self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> AdmissionFuture<'a, CursorAdvanceOutcome> {
        self.cover_past_advances
            .lock()
            .unwrap()
            .push(advance.clone());
        self.inner
            .advance_non_durable_source_cursor(advance, cancellation)
    }

    fn get_source_cursor<'a>(
        &'a self,
        source: &'a ObservationSourceIdentityV1,
        scope: &'a ObservationScopeV1,
    ) -> AdmissionFuture<'a, Option<ObservationSourceCursorV1>> {
        if self.report_no_cursor.load(Ordering::SeqCst) {
            return Box::pin(async { Ok(None) });
        }
        self.inner.get_source_cursor(source, scope)
    }

    fn drain_projection_queue<'a>(
        &'a self,
        provider: &'a str,
        scope: &'a ObservationScopeV1,
        cancellation: &'a ObservationCancellation,
        max: usize,
    ) -> AdmissionFuture<'a, HostProjectionDrainOutcome> {
        self.inner
            .drain_projection_queue(provider, scope, cancellation, max)
    }

    fn has_session_message<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        message_id: &'a str,
    ) -> AdmissionFuture<'a, bool> {
        self.inner.has_session_message(scope, provider, message_id)
    }

    fn get_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
    ) -> AdmissionFuture<'a, Option<ParseOffset>> {
        self.inner.get_parse_offset(scope, path)
    }

    fn advance_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
        offset: ParseOffset,
    ) -> AdmissionFuture<'a, ()> {
        self.inner.advance_parse_offset(scope, path, offset)
    }
}

const SESSION_ID: &str = "seam-contract-session";

/// Two durable Codex records: the session meta and one user message.
fn write_rollout(path: &Path, cwd: &Path) -> u64 {
    let lines = [
        json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": SESSION_ID,
                "cwd": cwd,
                "model": "gpt-5.5"
            }
        }),
        json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "seam contract message"
            }
        }),
    ];
    let contents = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(path, &contents).unwrap();
    u64::try_from(contents.len()).unwrap()
}

fn rollout_fixture() -> (tempfile::TempDir, PathBuf, u64) {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let path = temp.path().join("rollout.jsonl");
    let len = write_rollout(&path, &cwd);
    (temp, path, len)
}

async fn stored_cursor(spy: &SeamSpyAdmission) -> Option<ObservationSourceCursorV1> {
    let source = crate::runtime::codex::codex_observation_source_v2(SESSION_ID).unwrap();
    spy.get_source_cursor(&source, &ObservationScopeV1::Profile)
        .await
        .unwrap()
}

#[tokio::test]
async fn commit_failures_block_typed_and_never_cover_past() {
    for reason in [
        "observation_commit_failed",
        "authority_write_failed",
        "observation_persisted_value_unavailable",
    ] {
        let (_temp, path, _len) = rollout_fixture();
        let spy = SeamSpyAdmission::default();
        spy.script_capture_error(HostAdmissionOutcome::degraded(reason));

        let error = try_admit_codex_jsonl_observations_for_profile_with_admission(
            &path,
            None,
            &[],
            &spy,
            None,
        )
        .await
        .expect_err("a commit failure must block the source instead of covering past it");

        match error {
            TranscriptIngestError::HostAdmission {
                provider: "codex",
                reason: surfaced,
                retryable: false,
                ..
            } => assert_eq!(surfaced, reason),
            other => panic!("commit failure must stay a typed admission block, got {other:?}"),
        }
        assert!(
            spy.cover_past_advances().is_empty(),
            "{reason}: no coverage may be written over an uncommitted record"
        );
        assert!(spy.inner.observations().is_empty());
        assert!(
            stored_cursor(&spy).await.is_none(),
            "{reason}: the source frontier must not advance"
        );
    }
}

#[tokio::test]
async fn retryable_admission_failures_keep_their_own_verdict() {
    let (_temp, path, _len) = rollout_fixture();
    let spy = SeamSpyAdmission::default();
    spy.script_capture_error(HostAdmissionOutcome::retained_backpressured(
        "cursor_conflict",
    ));

    let error =
        try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &spy, None)
            .await
            .expect_err("a retryable race must surface for another pass");

    assert!(
        matches!(
            error,
            TranscriptIngestError::HostAdmission {
                provider: "codex",
                reason: "cursor_conflict",
                retryable: true,
                ..
            }
        ),
        "retryable races must not be laundered into a terminal record verdict: {error:?}"
    );
    assert!(spy.cover_past_advances().is_empty());
    assert!(stored_cursor(&spy).await.is_none());
}

#[tokio::test]
async fn eligible_identity_collision_retries_once_with_normalizer_fallback() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cursor-repeated-no-id.jsonl");
    let bytes = b"{\"role\":\"user\",\"content\":\"repeated\"}\n";
    std::fs::write(&path, bytes).unwrap();
    let spy = SeamSpyAdmission::default();
    spy.script_batch_error(HostAdmissionOutcome::batch_requires_scalar_fallback(
        ObservationBatchFallbackCause::IntraBatchIdentityCollision,
    ));
    spy.script_capture_error_once(HostAdmissionOutcome::deterministic_content_refusal(
        "observation_identity_collision",
    ));
    let normalized_ids = Arc::new(Mutex::new(Vec::new()));
    let observed_ids = Arc::clone(&normalized_ids);
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("cursor").unwrap(),
        SessionId::new("session.cursor-repeated-no-id").unwrap(),
    )
    .unwrap();
    let request = super::JsonlObservationAdmissionRequest::new(
        "cursor",
        &path,
        &spy,
        source,
        ObservationScopeV1::Profile,
        RetentionClass::new("test").unwrap(),
    );

    let progress = super::admit_jsonl_observations(
        request,
        |_| (),
        move |_, bytes, range, _, _, hints| {
            let id = if hints.identity_collision_retry {
                "record.positional-fallback"
            } else {
                "record.legacy-content-hash"
            };
            observed_ids.lock().unwrap().push(id);
            let native_record_id = ObservationId::new(id).unwrap();
            let parsed = tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1(
                bytes,
                range,
                ObservationOrderingDomainV1::FileBytes,
                |native| {
                    CanonicalObservationEnvelopeV1::new(
                        ProviderId::new("cursor").unwrap(),
                        "message",
                        native_record_id.clone(),
                        CanonicalObservationRelationsV1::new(
                            SessionId::new("session.cursor-repeated-no-id").unwrap(),
                        )
                        .with_message_id(native_record_id.clone()),
                        vec![CanonicalObservationFactV1::Message {
                            role: CanonicalMessageRoleV1::User,
                            content: native,
                            model: None,
                            timestamp: None,
                        }],
                        CanonicalObservationEvidenceV1::new(
                            ObservationOrderingDomainV1::FileBytes,
                            range,
                        ),
                    )
                    .map_err(|_| {
                        tracedecay_runtime_core::privacy::ObservationRecordParseErrorV1::NormalizationFailed
                    })
                },
            )
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "cursor" })?;
            if hints.identity_collision_retry {
                Ok(super::JsonlFrameAdmission::durable(
                    parsed,
                    native_record_id,
                ))
            } else {
                Ok(
                    super::JsonlFrameAdmission::durable_with_identity_collision_retry(
                        parsed,
                        native_record_id,
                    ),
                )
            }
        },
    )
    .await
    .expect("a validated no-ID collision must retry with positional identity");

    assert_eq!(progress.frames_persisted, 1);
    assert_eq!(progress.frames_refused, 0);
    assert_eq!(
        spy.capture_count(),
        2,
        "one primary plus one fallback write"
    );
    assert_eq!(
        spy.capture_collision_dispositions(),
        [
            ObservationIdentityCollisionDispositionV1::RetryWithAlternateIdentity,
            ObservationIdentityCollisionDispositionV1::RetryWithAlternateIdentity,
            ObservationIdentityCollisionDispositionV1::SettleTerminal,
        ],
        "the batch and scalar primary may probe, while the alternate is terminal"
    );
    assert_eq!(
        normalized_ids.lock().unwrap().as_slice(),
        [
            "record.legacy-content-hash",
            "record.legacy-content-hash",
            "record.positional-fallback"
        ],
        "the batch parse, scalar primary, and one positional retry are the only normalizations"
    );
    assert!(spy.cover_past_advances().is_empty());
    assert_eq!(spy.inner.observations().len(), 1);
}

#[tokio::test]
async fn exhausted_identity_collision_retry_uses_its_exact_terminal_coverage_reason() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cursor-repeated-no-id-terminal.jsonl");
    let bytes = b"{\"role\":\"user\",\"content\":\"repeated\"}\n";
    std::fs::write(&path, bytes).unwrap();
    let spy = SeamSpyAdmission::default();
    spy.script_batch_error(HostAdmissionOutcome::batch_requires_scalar_fallback(
        ObservationBatchFallbackCause::IntraBatchIdentityCollision,
    ));
    spy.script_capture_error(HostAdmissionOutcome::deterministic_content_refusal(
        "observation_identity_collision",
    ));
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("cursor").unwrap(),
        SessionId::new("session.cursor-repeated-no-id-terminal").unwrap(),
    )
    .unwrap();
    let request = super::JsonlObservationAdmissionRequest::new(
        "cursor",
        &path,
        &spy,
        source,
        ObservationScopeV1::Profile,
        RetentionClass::new("test").unwrap(),
    );

    let progress = super::admit_jsonl_observations(
        request,
        |_| (),
        move |_, bytes, range, _, _, hints| {
            let native_record_id = ObservationId::new(if hints.identity_collision_retry {
                "record.positional-fallback-terminal"
            } else {
                "record.legacy-content-hash-terminal"
            })
            .unwrap();
            let parsed = tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1(
                bytes,
                range,
                ObservationOrderingDomainV1::FileBytes,
                |native| {
                    CanonicalObservationEnvelopeV1::new(
                        ProviderId::new("cursor").unwrap(),
                        "message",
                        native_record_id.clone(),
                        CanonicalObservationRelationsV1::new(
                            SessionId::new("session.cursor-repeated-no-id-terminal").unwrap(),
                        )
                        .with_message_id(native_record_id.clone()),
                        vec![CanonicalObservationFactV1::Message {
                            role: CanonicalMessageRoleV1::User,
                            content: native,
                            model: None,
                            timestamp: None,
                        }],
                        CanonicalObservationEvidenceV1::new(
                            ObservationOrderingDomainV1::FileBytes,
                            range,
                        ),
                    )
                    .map_err(|_| {
                        tracedecay_runtime_core::privacy::ObservationRecordParseErrorV1::NormalizationFailed
                    })
                },
            )
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "cursor" })?;
            if hints.identity_collision_retry {
                Ok(super::JsonlFrameAdmission::durable(
                    parsed,
                    native_record_id,
                ))
            } else {
                Ok(
                    super::JsonlFrameAdmission::durable_with_identity_collision_retry(
                        parsed,
                        native_record_id,
                    ),
                )
            }
        },
    )
    .await
    .expect("the exhausted deterministic collision must settle terminal coverage");

    assert_eq!(progress.frames_persisted, 0);
    assert_eq!(progress.frames_refused, 1);
    assert_eq!(spy.capture_count(), 2, "primary plus one fallback only");
    assert_eq!(
        spy.capture_collision_dispositions(),
        [
            ObservationIdentityCollisionDispositionV1::RetryWithAlternateIdentity,
            ObservationIdentityCollisionDispositionV1::RetryWithAlternateIdentity,
            ObservationIdentityCollisionDispositionV1::SettleTerminal,
        ],
        "the exhausted alternate must restore terminal collision semantics"
    );
    let advances = spy.cover_past_advances();
    assert_eq!(advances.len(), 1);
    assert_eq!(
        advances[0].reason(),
        ObservationCoverageReason::ObservationIdentityCollision
    );
}

#[tokio::test]
async fn content_refusals_cover_past_so_the_stream_converges() {
    let (_temp, path, len) = rollout_fixture();
    let spy = SeamSpyAdmission::default();
    spy.script_capture_error(HostAdmissionOutcome::deterministic_content_refusal(
        "invalid_observation_contract",
    ));

    let progress =
        try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &spy, None)
            .await
            .expect("deterministic content refusals must not block the source");

    assert_eq!(progress.bytes_consumed, len);
    assert_eq!(progress.frames_refused, 2);
    assert_eq!(progress.frames_persisted, 0);
    assert_eq!(
        spy.capture_count(),
        2,
        "both source frames reached the fully materialized admission boundary exactly once"
    );
    let advances = spy.cover_past_advances();
    assert_eq!(advances.len(), 2, "both refused frames must be covered");
    for advance in &advances {
        assert_eq!(
            advance.reason(),
            ObservationCoverageReason::AdmissionRefused
        );
    }
    let cursor = stored_cursor(&spy)
        .await
        .expect("coverage must advance the frontier");
    assert_eq!(cursor.position(), len);

    let replay =
        try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &spy, None)
            .await
            .expect("a covered stream must converge");
    assert_eq!(replay.bytes_consumed, 0);
    assert_eq!(
        spy.capture_count(),
        2,
        "a settled FileBytes cursor must prevent subsequent deserialize, native/canonical ID derivation, and payload hashing"
    );
    assert_eq!(
        spy.cover_past_advances().len(),
        2,
        "converged coverage must not be re-written"
    );
}

#[tokio::test]
async fn codex_session_meta_prefix_is_decoded_once_across_consumers() {
    let (_temp, path, _) = rollout_fixture();
    let first = SeamSpyAdmission::default();
    let second = SeamSpyAdmission::default();
    let before = crate::runtime::codex::session_meta_read_count_for_test(&path);

    try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &first, None)
        .await
        .expect("first profile consumer");
    try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &second, None)
        .await
        .expect("second profile consumer");

    assert_eq!(
        crate::runtime::codex::session_meta_read_count_for_test(&path) - before,
        1,
        "canonical path+native identity must share one bounded prefix decode"
    );
}

#[tokio::test]
async fn batch_refusal_reuses_pre_context_switch_frames() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().unwrap();
    let project_a = temp.path().join("project-a");
    let project_b = temp.path().join("project-b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    let path = temp.path().join("context-switch.jsonl");
    let lines = [
        json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": { "id": SESSION_ID, "cwd": project_a, "model": "gpt-5.5" }
        }),
        json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": { "type": "user_message", "message": "belongs to A" }
        }),
        json!({
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "turn_context",
            "payload": { "cwd": project_b }
        }),
    ]
    .into_iter()
    .map(|line| line.to_string())
    .collect::<Vec<_>>();
    let a_message_start = u64::try_from(lines[0].len() + 1).unwrap();
    let a_message_end = a_message_start + u64::try_from(lines[1].len() + 1).unwrap();
    let contents = lines.join("\n") + "\n";
    std::fs::write(&path, contents).unwrap();
    let spy = SeamSpyAdmission::default();
    spy.script_batch_error(HostAdmissionOutcome::batch_requires_scalar_fallback(
        ObservationBatchFallbackCause::IntraBatchIdentityCollision,
    ));
    spy.script_capture_error(HostAdmissionOutcome::deterministic_content_refusal(
        "observation_identity_collision",
    ));

    try_admit_codex_jsonl_observations_for_project_with_admission(
        &path,
        &project_a,
        ProjectId::new("project-a").unwrap(),
        &spy,
        None,
    )
    .await
    .expect("a deterministic refusal must cover the original A-scoped frame");

    let a_message_advance = spy
        .cover_past_advances()
        .into_iter()
        .find(|advance| {
            advance.covered().start() == a_message_start && advance.covered().end() == a_message_end
        })
        .expect("the A-scoped message must receive a durable disposition");
    assert_eq!(
        a_message_advance.reason(),
        ObservationCoverageReason::ObservationIdentityCollision,
        "the A-scoped frame must keep its pre-window disposition after the B context switch"
    );
}

#[tokio::test]
async fn exact_duplicates_are_idempotent_no_op_receipts() {
    let (_temp, path, len) = rollout_fixture();
    let spy = SeamSpyAdmission::default();

    let first =
        try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &spy, None)
            .await
            .expect("initial admission must persist both records");
    assert_eq!(spy.inner.observations().len(), 2);
    assert_eq!(first.frames_decoded, 2);
    assert_eq!(first.frames_persisted, first.frames_decoded);
    assert_eq!(first.frames_refused, 0);
    assert!(spy.cover_past_advances().is_empty());
    let committed = stored_cursor(&spy)
        .await
        .expect("persist must advance the frontier");
    assert_eq!(committed.position(), len);

    // Replay the whole file against the already-durable rows, the state a
    // lost or stale frontier read produces: every frame is an exact
    // duplicate (same identity + digest) and must be a silent no-op receipt.
    spy.report_no_cursor.store(true, Ordering::SeqCst);
    let replay =
        try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &spy, None)
            .await
            .expect("exact duplicates must be idempotent no-op receipts");
    spy.report_no_cursor.store(false, Ordering::SeqCst);

    assert_eq!(replay.bytes_consumed, len);
    assert_eq!(spy.inner.observations().len(), 2, "no duplicate rows");
    assert!(
        spy.cover_past_advances().is_empty(),
        "an exact duplicate is not an admission refusal and writes no coverage"
    );
    let unchanged = stored_cursor(&spy)
        .await
        .expect("frontier must remain durable");
    assert_eq!(
        unchanged, committed,
        "an exact duplicate replay performs no extra cursor write"
    );
}

/// A rollout whose third line is not JSON at all, so the two classifications
/// are distinguishable: only a decode can call it `MalformedFrame`.
fn write_undecodable_tail_rollout(path: &Path, cwd: &Path) -> u64 {
    let contents = format!(
        "{}\n{}\n{}\n",
        json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": { "id": SESSION_ID, "cwd": cwd, "model": "gpt-5.5" }
        }),
        json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": { "type": "user_message", "message": "scope gate message" }
        }),
        // Complete line, valid UTF-8, unbalanced JSON. It names neither
        // `session_meta` nor `turn_context`, so it cannot move the rollout cwd.
        r#"{"type":"event_msg","payload":"#,
    );
    std::fs::write(path, &contents).unwrap();
    u64::try_from(contents.len()).unwrap()
}

/// Out-of-scope frames are rejected before their bytes are decoded.
///
/// The assertion is a classification, not a duration. The rollout's last frame
/// cannot be decoded: any path that decodes it reports `MalformedFrame`, and
/// only a scope test that runs *ahead* of the decode can report `OutOfScope`.
/// The reason on the persisted coverage row is therefore a direct observation
/// of whether the parse was paid for a frame whose scope already excluded it.
#[tokio::test]
async fn out_of_scope_frames_are_rejected_before_the_decode() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let path = temp.path().join("rollout.jsonl");
    let len = write_undecodable_tail_rollout(&path, &cwd);
    let _pin = super::pin_shared_jsonl_paths(std::slice::from_ref(&path));
    let spy = SeamSpyAdmission::default();

    // Profile scope owns exactly the records no registered project claims, so
    // registering the rollout's own cwd puts every one of its frames out of
    // scope for this pass.
    let progress = try_admit_codex_jsonl_observations_for_profile_with_admission(
        &path,
        None,
        std::slice::from_ref(&cwd),
        &spy,
        None,
    )
    .await
    .expect("an out-of-scope rollout is covered past, not an error");
    let (page, hit) = super::shared_jsonl_page(
        &path,
        StoredCursor::default(),
        None,
        None,
        super::SharedJsonlFramePreparation::Lazy,
    )
    .await
    .expect("the retained admission page remains available");

    let reasons = spy
        .cover_past_advances()
        .iter()
        .map(ObservationCursorAdvance::reason)
        .collect::<Vec<_>>();
    assert_eq!(
        reasons,
        vec![ObservationCoverageReason::OutOfScope; 3],
        "an out-of-scope frame that was never decoded cannot be reported as malformed"
    );
    assert_eq!(progress.frames_skipped, 3);
    assert_eq!(progress.frames_decoded, 1);
    // The coverage reason proves the decode was skipped; this proves the seam
    // *reports* it. `session_meta` names itself, so it is still decoded before
    // it is judged — only the two frames that cannot move the cwd are refused
    // from the gate, and the split has to say so or a change that moves the
    // verdict earlier is invisible to production telemetry.
    assert_eq!(
        progress.frames_rejected_before_decode, 2,
        "every frame that cannot move the cwd is refused without a parse, and \
         the one that can is not"
    );
    assert_eq!(progress.frames_persisted, 0);
    assert!(hit);
    assert_eq!(
        super::shared_jsonl_frame_preparations_for_test(page.file_identity),
        1,
        "only the context-bearing session_meta frame may reach canonical preparation"
    );
    assert_eq!(
        progress
            .frames_decoded
            .saturating_add(progress.frames_rejected_before_decode),
        3,
        "every attempted frame is either decoded or rejected before decode, never both"
    );
    assert_eq!(spy.capture_count(), 0);
    assert!(spy.inner.observations().is_empty());
    assert_eq!(
        stored_cursor(&spy)
            .await
            .expect("covered-past frames still advance the frontier")
            .position(),
        len
    );
}

/// The same rollout in scope: records still admit, and the decode still owns
/// the malformed verdict for the frame the scope gate no longer intercepts.
#[tokio::test]
async fn in_scope_frames_still_admit_and_keep_the_decode_verdict() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let path = temp.path().join("rollout.jsonl");
    write_undecodable_tail_rollout(&path, &cwd);
    let _pin = super::pin_shared_jsonl_paths(std::slice::from_ref(&path));
    let spy = SeamSpyAdmission::default();

    let progress =
        try_admit_codex_jsonl_observations_for_profile_with_admission(&path, None, &[], &spy, None)
            .await
            .expect("an in-scope rollout admits");
    let (page, hit) = super::shared_jsonl_page(
        &path,
        StoredCursor::default(),
        None,
        None,
        super::SharedJsonlFramePreparation::Lazy,
    )
    .await
    .expect("the retained admission page remains available");

    assert_eq!(spy.inner.observations().len(), 2);
    assert_eq!(progress.frames_persisted, 2);
    assert_eq!(progress.frames_decoded, 3);
    assert_eq!(
        progress.frames_rejected_before_decode, 0,
        "nothing in scope may be refused before it is decoded"
    );
    assert!(hit);
    assert_eq!(
        super::shared_jsonl_frame_preparations_for_test(page.file_identity),
        3,
        "the context frame and both in-scope frames must reach canonical preparation"
    );
    assert_eq!(
        progress
            .frames_decoded
            .saturating_add(progress.frames_rejected_before_decode),
        3,
        "every attempted frame is either decoded or rejected before decode, never both"
    );
    assert!(
        page.frames
            .iter()
            .all(|frame| frame.prepared.get().is_some()),
        "in-scope replay frames must prepare once through the shared admitted worker path"
    );
    assert_eq!(
        spy.cover_past_advances()
            .iter()
            .map(ObservationCursorAdvance::reason)
            .collect::<Vec<_>>(),
        vec![ObservationCoverageReason::MalformedFrame],
        "in scope, the undecodable frame keeps the decode's own verdict"
    );
}

#[tokio::test]
async fn exact_hook_prepares_an_in_scope_window_concurrently() {
    super::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let path = temp.path().join("parallel-exact-hook.jsonl");
    let mut contents = format!(
        "{}\n",
        json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": { "id": SESSION_ID, "cwd": cwd, "model": "gpt-5.5" }
        })
    );
    let event_count = 32_usize;
    for index in 0..event_count {
        contents.push_str(
            &json!({
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "user_message",
                    "message": format!("{index}:{}", "x".repeat(256 * 1024))
                }
            })
            .to_string(),
        );
        contents.push('\n');
    }
    std::fs::write(&path, contents).unwrap();
    super::SHARED_JSONL_PEAK_FRAME_PREPARATIONS.store(0, Ordering::Release);
    let prepared_before = super::SHARED_JSONL_TOTAL_FRAME_PREPARATIONS.load(Ordering::Acquire);

    let progress = try_admit_codex_jsonl_observations_for_profile_with_admission(
        &path,
        None,
        &[],
        &SeamSpyAdmission::default(),
        None,
    )
    .await
    .expect("bounded exact-hook admission");

    assert!(progress.frames_persisted >= u64::try_from(event_count).unwrap());
    assert!(
        super::SHARED_JSONL_TOTAL_FRAME_PREPARATIONS
            .load(Ordering::Acquire)
            .saturating_sub(prepared_before)
            >= event_count,
        "every in-scope event is prepared once through the shared window"
    );
    if std::thread::available_parallelism().is_ok_and(|cores| cores.get() > 1) {
        assert!(
            super::SHARED_JSONL_PEAK_FRAME_PREPARATIONS.load(Ordering::Acquire) > 1,
            "the exact-hook path must overlap independent frame preparation"
        );
    }
}
