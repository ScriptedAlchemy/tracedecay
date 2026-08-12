use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use serde_json::json;
use tempfile::{TempDir, tempdir};
use tokio::sync::Notify;
use tracedecay_domain::{Confidence, FactCategoryV1, FactOwnerV1, ProvenanceId, UtcMicros};
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
    NeverCancelled, VerifiedGraphSnapshot,
};
use tracedecay_store::{
    FactCommitOutcome, FactCurrentQuery, FactReadControl, FactStore, FactStoreError,
    FactWriteControl, ProjectMemoryGraphQueryV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::store::memory::crud::{initial_batch, sanitize_payload};
use crate::store::memory::{DatabaseFactStore, ProjectMemoryGraphReconciliationScheduleV1};
use crate::store_runtime::VerifiedGraphRuntimePortV1;

struct RecordingGraphRuntime {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    block_reconciliation: bool,
    snapshot_error: Option<GraphDbError>,
    reconciliation_cancelled: AtomicBool,
    reconciliation_closed: AtomicBool,
    reconciliation_started: AtomicBool,
    reconciliation_finished: AtomicBool,
    reconciliation_observed: AtomicBool,
    reconciliation_notify: Notify,
    publish_calls: AtomicUsize,
    reconcile_calls: AtomicUsize,
    snapshot_calls: AtomicUsize,
}

impl RecordingGraphRuntime {
    fn new(database: &Database) -> Self {
        Self {
            binding: database.retained_runtime().binding().clone(),
            locator: database.retained_runtime().locator().verified().clone(),
            block_reconciliation: false,
            snapshot_error: None,
            reconciliation_cancelled: AtomicBool::new(false),
            reconciliation_closed: AtomicBool::new(false),
            reconciliation_started: AtomicBool::new(false),
            reconciliation_finished: AtomicBool::new(false),
            reconciliation_observed: AtomicBool::new(false),
            reconciliation_notify: Notify::new(),
            publish_calls: AtomicUsize::new(0),
            reconcile_calls: AtomicUsize::new(0),
            snapshot_calls: AtomicUsize::new(0),
        }
    }

    fn blocking(database: &Database) -> Self {
        Self {
            block_reconciliation: true,
            ..Self::new(database)
        }
    }

    fn reset_required(database: &Database) -> Self {
        Self {
            snapshot_error: Some(GraphDbError::ResetRequired {
                message: "verified profile-memory graph generation mismatch".to_owned(),
            }),
            ..Self::new(database)
        }
    }
}

impl VerifiedGraphRuntimePortV1 for RecordingGraphRuntime {
    fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }

    fn cancel_reconciliation(&self) {
        self.reconciliation_cancelled.store(true, Ordering::Release);
    }

    fn close_reconciliation(&self) -> Result<(), GraphDbError> {
        self.reconciliation_closed.store(true, Ordering::Release);
        Ok(())
    }

    fn publish_verified_manifest(
        &self,
        _manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
        _cancelled: Arc<AtomicBool>,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.publish_calls.fetch_add(1, Ordering::SeqCst);
        Err(GraphDbError::invalid(
            "memory graph reads must not publish a generation",
        ))
    }

    fn reconcile_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.reconcile_calls.fetch_add(1, Ordering::SeqCst);
        if self.block_reconciliation {
            self.reconciliation_started.store(true, Ordering::Release);
            self.reconciliation_observed.store(true, Ordering::Release);
            self.reconciliation_notify.notify_one();
            while !self.reconciliation_cancelled.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            self.reconciliation_finished.store(true, Ordering::Release);
            return Err(GraphDbError::Cancelled);
        }
        let snapshot = VerifiedGraphSnapshot::memory(manifest.clone(), Arc::new(NeverCancelled))?;
        self.reconciliation_observed.store(true, Ordering::Release);
        self.reconciliation_notify.notify_one();
        Ok(snapshot)
    }

    fn verified_snapshot(
        &self,
        _projection: &GraphProjectionIdentity,
        read_control: FactReadControl,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        if read_control.interrupted() {
            return Err(GraphDbError::Cancelled);
        }
        self.snapshot_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = &self.snapshot_error {
            return Err(error.clone());
        }
        Ok(None)
    }
}

async fn database(label: &str) -> (TempDir, Database) {
    let directory = tempdir().expect("create graph reconciliation fixture directory");
    let path = directory.path().join(format!("{label}.db"));
    let authority = DatabaseAuthority::acquire_test(&path, "graph reconciliation test authority")
        .expect("acquire graph reconciliation fixture authority");
    let (database, _) = Database::publish_profile_memory_test_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .expect("publish graph reconciliation fixture runtime");
    (directory, database)
}

fn bind_runtime(database: &Database) -> Arc<RecordingGraphRuntime> {
    let runtime = Arc::new(RecordingGraphRuntime::new(database));
    let port: Arc<dyn VerifiedGraphRuntimePortV1> = runtime.clone();
    database
        .bind_memory_graph_runtime(port)
        .expect("bind recording graph runtime");
    runtime
}

fn bind_blocking_runtime(database: &Database) -> Arc<RecordingGraphRuntime> {
    let runtime = Arc::new(RecordingGraphRuntime::blocking(database));
    let port: Arc<dyn VerifiedGraphRuntimePortV1> = runtime.clone();
    database
        .bind_memory_graph_runtime(port)
        .expect("bind blocking graph runtime");
    runtime
}

fn bind_reset_required_runtime(database: &Database) -> Arc<RecordingGraphRuntime> {
    let runtime = Arc::new(RecordingGraphRuntime::reset_required(database));
    let port: Arc<dyn VerifiedGraphRuntimePortV1> = runtime.clone();
    database
        .bind_memory_graph_runtime(port)
        .expect("bind reset-required graph runtime");
    runtime
}

fn write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

async fn wait_for_reconciliation(runtime: &RecordingGraphRuntime) {
    if !runtime.reconciliation_observed.load(Ordering::Acquire) {
        tokio::time::timeout(
            Duration::from_secs(1),
            runtime.reconciliation_notify.notified(),
        )
        .await
        .expect("scheduled graph reconciliation did not reach the mounted runtime");
    }
    assert!(
        runtime.reconciliation_observed.load(Ordering::Acquire),
        "scheduled graph reconciliation did not reach the mounted runtime"
    );
}

#[tokio::test]
async fn graph_read_inspects_verified_snapshot_without_publishing() {
    let (_directory, database) = database("snapshot-only-read").await;
    let runtime = bind_runtime(&database);
    let query =
        ProjectMemoryGraphQueryV1::new(FactOwnerV1::Profile, Vec::new(), 8).expect("graph query");

    let result = super::graph::project_memory_graph(
        &database,
        query,
        &FactReadControl::new(Arc::new(|| false)),
    )
    .await;

    assert!(matches!(result, Err(FactStoreError::GraphUnavailable)));
    assert_eq!(runtime.snapshot_calls.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn graph_read_observes_live_cancellation_before_snapshot_access() {
    let (_directory, database) = database("live-cancelled-read").await;
    let runtime = bind_runtime(&database);
    let interrupted = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&interrupted);
    let read_control = FactReadControl::new(Arc::new(move || observed.load(Ordering::Acquire)));
    interrupted.store(true, Ordering::Release);
    let query =
        ProjectMemoryGraphQueryV1::new(FactOwnerV1::Profile, Vec::new(), 8).expect("graph query");

    let result = super::graph::project_memory_graph(&database, query, &read_control).await;

    assert!(matches!(result, Err(FactStoreError::GraphCancelled)));
    assert_eq!(runtime.snapshot_calls.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn graph_read_preserves_reset_required_from_the_verified_snapshot() {
    let (_directory, database) = database("reset-required-read").await;
    let runtime = bind_reset_required_runtime(&database);
    let query =
        ProjectMemoryGraphQueryV1::new(FactOwnerV1::Profile, Vec::new(), 8).expect("graph query");

    let result = super::graph::project_memory_graph(
        &database,
        query,
        &FactReadControl::new(Arc::new(|| false)),
    )
    .await;

    assert!(matches!(
        result,
        Err(FactStoreError::GraphResetRequired {
            owner: FactOwnerV1::Profile,
            reason,
        }) if reason == "verified profile-memory graph generation mismatch"
    ));
    assert_eq!(runtime.snapshot_calls.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn successful_project_memory_transaction_schedules_lifecycle_reconciliation() {
    let (_directory, database) = database("write-side-reconciliation").await;
    let runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);

    store
        .project_memory_write(&write_control(), |_transaction| {
            Box::pin(async { Ok::<(), FactStoreError>(()) })
        })
        .await
        .expect("commit project-memory transaction");
    wait_for_reconciliation(&runtime).await;

    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.snapshot_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn committed_low_level_fact_batch_schedules_lifecycle_reconciliation() {
    let (_directory, database) = database("low-level-reconciliation").await;
    let runtime = bind_runtime(&database);
    let sanitized = sanitize_payload(
        "canonical low-level graph reconciliation fact",
        FactCategoryV1::General,
        &[],
        &[],
        &json!({"fixture": "low-level-reconciliation"}),
        None,
    )
    .expect("sanitize low-level reconciliation payload")
    .expect("low-level reconciliation payload remains durable");
    let batch = initial_batch(
        &FactOwnerV1::Profile,
        &ProvenanceId::new("graph.reconciliation.low-level".to_owned())
            .expect("low-level operation id"),
        sanitized.payload,
        sanitized.access,
        Confidence::new(0.8).expect("low-level fixture confidence"),
        None,
        UtcMicros(1_000_000),
    )
    .expect("low-level fact batch");

    let outcome = DatabaseFactStore::new(&database)
        .commit_fact(batch, &write_control())
        .await
        .expect("commit low-level fact batch");
    assert!(matches!(outcome, FactCommitOutcome::Committed(_)));
    wait_for_reconciliation(&runtime).await;

    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caller_drop_after_commit_start_cannot_lose_reconciliation() {
    let (_directory, database) = database("dropped-low-level-commit-caller").await;
    let runtime = bind_runtime(&database);
    let sanitized = sanitize_payload(
        "canonical fact whose caller is dropped after commit start",
        FactCategoryV1::General,
        &[],
        &[],
        &json!({"fixture": "dropped-low-level-commit-caller"}),
        None,
    )
    .expect("sanitize dropped-caller payload")
    .expect("dropped-caller payload remains durable");
    let batch = initial_batch(
        &FactOwnerV1::Profile,
        &ProvenanceId::new("graph.reconciliation.dropped-caller".to_owned())
            .expect("dropped-caller operation id"),
        sanitized.payload,
        sanitized.access,
        Confidence::new(0.8).expect("dropped-caller confidence"),
        None,
        UtcMicros(2_000_000),
    )
    .expect("dropped-caller fact batch");
    let fact_id = batch.fact_id().clone();
    let commit_started = Arc::new(AtomicBool::new(false));
    let commit_observed = Arc::new(Notify::new());
    let release_commit = Arc::new(AtomicBool::new(false));
    let observed_start = Arc::clone(&commit_started);
    let observed_commit = Arc::clone(&commit_observed);
    let observed_release = Arc::clone(&release_commit);
    let control = FactWriteControl::new(
        Arc::new(|| false),
        Arc::new(move || {
            observed_start.store(true, Ordering::Release);
            observed_commit.notify_one();
            while !observed_release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            true
        }),
    );
    let caller_database = database.clone();
    let caller = tokio::spawn(async move {
        DatabaseFactStore::new(&caller_database)
            .commit_fact(batch, &control)
            .await
    });
    if !commit_started.load(Ordering::Acquire) {
        tokio::time::timeout(Duration::from_secs(1), commit_observed.notified())
            .await
            .expect("fact commit never reached the caller-owned commit-start gate");
    }
    assert!(
        commit_started.load(Ordering::Acquire),
        "fact commit never reached the caller-owned commit-start gate"
    );

    caller.abort();
    release_commit.store(true, Ordering::Release);
    assert!(
        caller
            .await
            .expect_err("caller task must be aborted")
            .is_cancelled(),
        "caller future must be dropped while the owned commit remains live"
    );
    wait_for_reconciliation(&runtime).await;

    let current = DatabaseFactStore::new(&database)
        .query_fact_current(
            FactCurrentQuery::new(FactOwnerV1::Profile, fact_id)
                .expect("dropped-caller current-fact query"),
        )
        .await
        .expect("query fact committed after caller drop");
    assert!(current.is_some(), "owned commit must remain authoritative");
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn concurrent_schedule_triggers_coalesce_before_spawning_more_work() {
    let (_directory, database) = database("coalesced-reconciliation").await;
    let runtime = bind_runtime(&database);

    assert_eq!(
        super::schedule_project_memory_graph_reconciliation(database.clone()),
        ProjectMemoryGraphReconciliationScheduleV1::Scheduled
    );
    assert_eq!(
        super::schedule_project_memory_graph_reconciliation(database.clone()),
        ProjectMemoryGraphReconciliationScheduleV1::AlreadyScheduled
    );
    wait_for_reconciliation(&runtime).await;

    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unmounted_reconciliation_is_a_truthful_schedule_state() {
    let (_directory, database) = database("unmounted-reconciliation").await;

    assert_eq!(
        super::schedule_project_memory_graph_reconciliation(database),
        ProjectMemoryGraphReconciliationScheduleV1::NotMounted
    );
}

#[tokio::test]
async fn retired_lifecycle_refuses_new_reconciliation_work() {
    let (_directory, database) = database("retired-reconciliation").await;
    let _runtime = bind_runtime(&database);
    database
        .memory_graph_reconciliation_task_owner()
        .expect("bound runtime has reconciliation owner")
        .cancel();

    assert_eq!(
        super::schedule_project_memory_graph_reconciliation(database),
        ProjectMemoryGraphReconciliationScheduleV1::LifecycleClosed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_blocking_graph_publication_to_observe_cancellation() {
    let (_directory, database) = database("blocking-reconciliation-shutdown").await;
    let runtime = bind_blocking_runtime(&database);
    let owner = database
        .memory_graph_reconciliation_task_owner()
        .expect("bound runtime has reconciliation owner");

    assert_eq!(
        super::schedule_project_memory_graph_reconciliation(database.clone()),
        ProjectMemoryGraphReconciliationScheduleV1::Scheduled
    );
    wait_for_reconciliation(&runtime).await;
    assert!(
        runtime.reconciliation_started.load(Ordering::Acquire),
        "blocking publication never started"
    );

    owner
        .shutdown()
        .await
        .expect("cancel and join blocking graph publication");

    assert!(runtime.reconciliation_cancelled.load(Ordering::Acquire));
    assert!(
        runtime.reconciliation_finished.load(Ordering::Acquire),
        "shutdown returned before blocking publication exited"
    );
    assert!(
        runtime.reconciliation_closed.load(Ordering::Acquire),
        "shutdown returned before closing the exact graph attachment"
    );
    assert!(!owner.running());
}
