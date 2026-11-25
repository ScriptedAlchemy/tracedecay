//! Bounded forward progress for code-generation retention while a publisher
//! holds the generation store.
//!
//! A busy store is a normal state under steady publication. The pass defers a
//! bounded number of attempts, then waits for the holder within the daemon's
//! existing task deadline instead of failing forever. These tests drive the
//! production registered-project pass, so a regression in the admission path
//! is a failed collection, not a unit-test detail.

#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tracedecay_code_index_retention::code_index_generations::fixture::write_generation_store_fixture;
use tracedecay_code_index_retention::code_index_generations::{
    acquire_code_generation_store_lock, acquire_generation_segments_publication_lock,
    code_index_store_root,
};
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1,
};
use tracedecay_runtime_core::storage::profile_sharded_data_root;

use super::{
    CodeGenerationRetentionOutcomeV1, GENERATION_STORE_BUSY_DEFERRALS_BEFORE_WAIT,
    RegisteredProjectStoreV1, run_registered_code_generation_retention,
};
use crate::telemetry::StoreTelemetrySamplingRegistry;

const SUPERSEDED_GENERATIONS: usize = 4;

fn generation_file_count(store_root: &Path) -> usize {
    std::fs::read_dir(store_root.join("code-generations-v1"))
        .expect("list generations")
        .count()
}

struct Fixture {
    _temp: tempfile::TempDir,
    store: RegisteredProjectStoreV1,
    store_root: PathBuf,
    profile_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    schedulers: CodeIndexSchedulerRegistryV1,
    observations: StoreTelemetrySamplingRegistry,
}

async fn fixture() -> Fixture {
    let temp = tempfile::TempDir::new().expect("temp root");
    let profile_root = temp.path().join("profile");
    let checkout = temp.path().join("checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    let checkout = checkout.canonicalize().expect("canonical checkout");
    let runtime = RegisteredGlobalDbTestRuntime::profile(&profile_root)
        .await
        .expect("profile database");
    let profile_database = runtime.profile_database_arc();
    let data_root = profile_sharded_data_root(&profile_root, "proj_busy");
    let store_root = code_index_store_root(&data_root, &checkout);
    write_generation_store_fixture(&store_root, SUPERSEDED_GENERATIONS);
    let schedulers =
        CodeIndexSchedulerRegistryV1::with_resident_memory_and_progress_producer_incarnation(
            1,
            Arc::new(ProcessResidentMemoryV1::new(
                DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
            )),
            1,
        );
    Fixture {
        _temp: temp,
        store: RegisteredProjectStoreV1 {
            data_root,
            canonical_root: checkout,
        },
        store_root,
        profile_database,
        schedulers,
        observations: StoreTelemetrySamplingRegistry::default(),
    }
}

/// Hold the generation store exclusively until the returned sender releases it.
fn hold_store(store_root: &Path) -> (mpsc::Receiver<()>, mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let root = store_root.to_path_buf();
    let handle = thread::spawn(move || {
        let lock = acquire_code_generation_store_lock(&root).expect("fixture publisher lock");
        acquired_tx.send(()).expect("signal held lock");
        release_rx.recv().expect("wait for release");
        drop(lock);
    });
    (acquired_rx, release_tx, handle)
}

fn hold_segments(
    store_root: &Path,
) -> (mpsc::Receiver<()>, mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let root = store_root.to_path_buf();
    let handle = thread::spawn(move || {
        let lock = acquire_generation_segments_publication_lock(&root, &|| false)
            .expect("fixture shared-project publication lock");
        acquired_tx.send(()).expect("signal held segment lock");
        release_rx.recv().expect("wait for publication release");
        drop(lock);
    });
    (acquired_rx, release_tx, handle)
}

async fn run_pass(
    fixture: &Fixture,
    cancellation: &CancellationToken,
) -> CodeGenerationRetentionOutcomeV1 {
    run_registered_code_generation_retention(
        &fixture.store,
        &BTreeSet::new(),
        &fixture.schedulers,
        &fixture.profile_database,
        &fixture.observations,
        cancellation,
    )
    .await
}

#[tokio::test]
async fn retention_defers_a_bounded_number_of_times_then_collects_under_pressure() {
    let fixture = fixture().await;
    assert_eq!(
        generation_file_count(&fixture.store_root),
        SUPERSEDED_GENERATIONS
    );

    for attempt in 1..=GENERATION_STORE_BUSY_DEFERRALS_BEFORE_WAIT {
        let (acquired, release, handle) = hold_store(&fixture.store_root);
        acquired.recv().expect("the publisher acquired the store");
        let outcome = run_pass(&fixture, &CancellationToken::new()).await;
        assert_eq!(
            outcome,
            CodeGenerationRetentionOutcomeV1::Failed,
            "attempt {attempt} waited instead of deferring: {outcome:?}"
        );
        assert_eq!(
            generation_file_count(&fixture.store_root),
            SUPERSEDED_GENERATIONS,
            "a deferred attempt collected generations"
        );
        release.send(()).expect("release the publisher");
        handle.join().expect("publisher thread");
    }

    // The bound is reached: the next attempt waits for the publisher and
    // collects within its own deadline once the writer releases the store.
    let (acquired, release, handle) = hold_store(&fixture.store_root);
    acquired.recv().expect("the publisher acquired the store");
    let released = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        release.send(()).expect("release the publisher");
        handle.join().expect("publisher thread");
    });
    let outcome = run_pass(&fixture, &CancellationToken::new()).await;
    released.join().expect("release thread");
    assert_ne!(
        outcome,
        CodeGenerationRetentionOutcomeV1::Failed,
        "the bounded wait never collected the superseded generations: {outcome:?}"
    );
    assert_eq!(
        generation_file_count(&fixture.store_root),
        1,
        "only the active generation may survive a collected store"
    );
}

#[tokio::test]
async fn retention_waits_out_shared_project_publication_after_bounded_deferrals() {
    let fixture = fixture().await;
    // Every scope shares the project segment lease. Its publisher can keep
    // retention busy even when this scope's generation store is uncontended.
    // Reacquire it for successive publications between maintenance attempts.
    for attempt in 1..=GENERATION_STORE_BUSY_DEFERRALS_BEFORE_WAIT {
        let (acquired, release, handle) = hold_segments(&fixture.store_root);
        acquired
            .recv()
            .expect("the publication acquired the segment lease");
        let outcome = run_pass(&fixture, &CancellationToken::new()).await;
        release.send(()).expect("release the publication");
        handle.join().expect("publisher thread");
        assert_eq!(outcome, CodeGenerationRetentionOutcomeV1::Failed);
        assert_eq!(
            fixture
                .observations
                .generation_store_busy_deferrals(&fixture.store_root),
            attempt,
        );
        assert_eq!(
            generation_file_count(&fixture.store_root),
            SUPERSEDED_GENERATIONS
        );
    }

    let (acquired, release, handle) = hold_segments(&fixture.store_root);
    acquired
        .recv()
        .expect("the publication acquired the segment lease");
    let publisher = thread::spawn(move || {
        // Exceeds the ordinary 50 ms acquisition probe. The maintenance wait
        // must cover the shared segment lease as well as the scope lease.
        thread::sleep(Duration::from_millis(200));
        release.send(()).expect("release the publication");
        handle.join().expect("publisher thread");
    });
    let outcome = run_pass(&fixture, &CancellationToken::new()).await;
    publisher.join().expect("publication release thread");
    assert_ne!(
        outcome,
        CodeGenerationRetentionOutcomeV1::Failed,
        "the shared-project lease still used a single-probe budget: {outcome:?}",
    );
    assert_eq!(generation_file_count(&fixture.store_root), 1);
    assert_eq!(
        fixture
            .observations
            .generation_store_busy_deferrals(&fixture.store_root),
        0,
    );
}

#[tokio::test]
async fn busy_store_wait_observes_shutdown_cancellation() {
    for hold in [hold_store, hold_segments] {
        let fixture = fixture().await;
        // Reach the wait phase: the pass has deferred the bound's worth already.
        for _ in 0..GENERATION_STORE_BUSY_DEFERRALS_BEFORE_WAIT {
            fixture
                .observations
                .record_generation_store_busy(&fixture.store_root);
        }
        let (acquired, release, handle) = hold(&fixture.store_root);
        acquired.recv().expect("the publisher acquired the store");

        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        let started = Instant::now();
        let outcome = run_pass(&fixture, &cancellation).await;
        let elapsed = started.elapsed();
        canceller.join().expect("canceller thread");
        assert!(
            elapsed < Duration::from_secs(1),
            "a waiting retention attempt did not stop at cancellation: {elapsed:?}"
        );
        assert_eq!(
            outcome,
            CodeGenerationRetentionOutcomeV1::Failed,
            "the cancelled pass must not report progress"
        );
        assert_eq!(
            generation_file_count(&fixture.store_root),
            SUPERSEDED_GENERATIONS,
            "a cancelled wait collected generations"
        );
        release.send(()).expect("release the publisher");
        handle.join().expect("publisher thread");
    }
}
