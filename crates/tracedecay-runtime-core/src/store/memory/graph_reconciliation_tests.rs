use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use serde_json::json;
use tempfile::{TempDir, tempdir};
use tokio::sync::Notify;
use tracedecay_domain::{
    Confidence, FactCategoryV1, FactEventId, FactOwnerV1, ProvenanceId, UtcMicros,
};
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
    NeverCancelled, VerifiedGraphSnapshot,
};
use tracedecay_store::{
    FactCommitOutcome, FactCurrentQuery, FactReadControl, FactStore, FactStoreError,
    FactWriteBatch, FactWriteControl, ProjectMemoryAutomaticFactApplyDispositionV1,
    ProjectMemoryAutomaticFactEffectV1, ProjectMemoryAutomaticFactEvidenceV1,
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddDispositionV1,
    ProjectMemoryFactAddMaterialV1, ProjectMemoryFactCurationAddV1,
    ProjectMemoryFactCurationBatchV1, ProjectMemoryFactCurationEvidenceV1,
    ProjectMemoryFactCurationMutationKindV1, ProjectMemoryFactCurationOperationV1,
    ProjectMemoryFactCurationReviewRefV1, ProjectMemoryFactFeedbackActionV1,
    ProjectMemoryFactFeedbackCommandV1, ProjectMemoryFactIdV1, ProjectMemoryFactMergeCommandV1,
    ProjectMemoryFactMergeTargetV1, ProjectMemoryFactRemoveCommandV1,
    ProjectMemoryFactRetrievalCommandV1, ProjectMemoryFactStore, ProjectMemoryFactUpdateCommandV1,
    ProjectMemoryFactUpdatePatchV1, ProjectMemoryGraphQueryV1, StoreRuntimeBindingV1,
    VerifiedStoreLocatorV1, derive_project_memory_fact_curation_child_operation_id,
};

use crate::db::{
    Database, DatabaseAuthority, MemoryGraphReconciliationCancelErrorV1,
    ProjectMemoryReconciliationTelemetryObserverV1, TestDatabaseRuntimeMode,
};
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use crate::store::memory::automatic_facts::project_memory_record_automatic_fact_receipt_tx;
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
            binding: database.registered_binding().clone(),
            locator: database.registered_verified_locator().clone(),
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
        tokio::time::timeout(Duration::from_secs(1), async {
            while !runtime.reconciliation_observed.load(Ordering::Acquire) {
                runtime.reconciliation_notify.notified().await;
            }
        })
        .await
        .expect("scheduled graph reconciliation did not reach the mounted runtime");
    }
    assert!(
        runtime.reconciliation_observed.load(Ordering::Acquire),
        "scheduled graph reconciliation did not reach the mounted runtime"
    );
}

async fn wait_for_completed_reconciliation_pass(
    observer: &ProjectMemoryReconciliationTelemetryObserverV1,
    expected_reconciliation_passes: u64,
) {
    for _ in 0..256 {
        let snapshot = observer.snapshot();
        if snapshot.reconciliation_passes == expected_reconciliation_passes
            && snapshot.active_reconciliation_pass_count == 0
        {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("project memory reconciliation did not complete its full pass");
}

fn non_graph_write_fixture_batch(label: &str) -> FactWriteBatch {
    let content = format!("canonical {label} fact without a graph source change");
    let sanitized = sanitize_payload(
        &content,
        FactCategoryV1::General,
        &[],
        &[],
        &json!({"fixture": label}),
        None,
    )
    .expect("sanitize non-graph write fixture payload")
    .expect("non-graph write fixture remains durable");
    initial_batch(
        &FactOwnerV1::Profile,
        &ProvenanceId::new(format!("graph.reconciliation.{label}.seed"))
            .expect("non-graph write seed operation id"),
        sanitized.payload,
        sanitized.access,
        Confidence::new(0.8).expect("non-graph write fixture confidence"),
        None,
        UtcMicros(1_000_000),
    )
    .expect("non-graph write fact batch")
}

async fn seed_fact_for_non_graph_write(
    store: &DatabaseFactStore<'_>,
    runtime: &RecordingGraphRuntime,
    label: &str,
) -> ProjectMemoryFactIdV1 {
    let batch = non_graph_write_fixture_batch(label);
    let target = ProjectMemoryFactIdV1::new(FactOwnerV1::Profile, batch.fact_id().clone())
        .expect("non-graph write target");
    let outcome = store
        .commit_fact(batch, &write_control())
        .await
        .expect("commit non-graph write fixture");
    assert!(matches!(outcome, FactCommitOutcome::Committed(_)));
    wait_for_reconciliation(runtime).await;
    runtime.reconcile_calls.store(0, Ordering::SeqCst);
    runtime
        .reconciliation_observed
        .store(false, Ordering::Release);
    target
}

struct HighLevelFactSeed {
    target: ProjectMemoryFactIdV1,
    last_event_id: FactEventId,
    content: String,
}

fn accepted_add_command(operation_id: &str, content: &str) -> ProjectMemoryFactAddCommandV1 {
    let material = json!({
        "content": content,
        "category": "project",
        "tags": ["graph-reconciliation"],
        "entities": ["TraceDecay"],
        "metadata": {"fixture": "graph-reconciliation"},
    });
    let receipt = match sanitize_memory_fact_payload(material.clone())
        .expect("sanitize graph reconciliation add fixture")
    {
        MemoryFactSanitizationV1::Durable { payload, receipt } => {
            assert_eq!(payload, material);
            receipt
        }
        MemoryFactSanitizationV1::Quarantined => {
            panic!("graph reconciliation add fixture must remain durable")
        }
    };
    ProjectMemoryFactAddMaterialV1::new(
        FactOwnerV1::Profile,
        content.to_owned(),
        FactCategoryV1::Project,
        None,
        vec!["graph-reconciliation".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({"fixture": "graph-reconciliation"}),
        receipt,
        None,
        Confidence::new(0.8).expect("graph reconciliation add confidence"),
        None,
    )
    .and_then(|material| {
        material.into_command(
            ProvenanceId::new(operation_id.to_owned())
                .expect("graph reconciliation add operation id"),
        )
    })
    .expect("graph reconciliation add command")
}

fn reset_reconciliation(runtime: &RecordingGraphRuntime) {
    runtime.reconcile_calls.store(0, Ordering::SeqCst);
    runtime
        .reconciliation_observed
        .store(false, Ordering::Release);
}

fn assert_no_reconciliation(runtime: &RecordingGraphRuntime) {
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
}

async fn add_high_level_source_fact(
    store: &DatabaseFactStore<'_>,
    label: &str,
    content: &str,
) -> HighLevelFactSeed {
    let outcome = store
        .add_project_memory_fact(
            accepted_add_command(&format!("graph.reconciliation.{label}.seed"), content),
            &write_control(),
        )
        .await
        .expect("seed graph reconciliation source fact");
    assert_eq!(
        outcome.disposition(),
        ProjectMemoryFactAddDispositionV1::Added
    );
    assert!(!outcome.commit_replayed());
    let receipt = outcome
        .commit_receipt()
        .expect("seed graph reconciliation source fact commit receipt");
    let target = ProjectMemoryFactIdV1::new(FactOwnerV1::Profile, outcome.fact().fact_id().clone())
        .expect("seed graph reconciliation source target");
    HighLevelFactSeed {
        target,
        last_event_id: receipt.last_event_id().clone(),
        content: content.to_owned(),
    }
}

async fn seed_high_level_fact(
    store: &DatabaseFactStore<'_>,
    runtime: &RecordingGraphRuntime,
    label: &str,
    content: &str,
) -> HighLevelFactSeed {
    let seed = add_high_level_source_fact(store, label, content).await;
    wait_for_reconciliation(runtime).await;
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 1);
    reset_reconciliation(runtime);
    seed
}

fn curation_add_batch(
    seed: &HighLevelFactSeed,
    outer_operation_id: &str,
    content: &str,
) -> ProjectMemoryFactCurationBatchV1 {
    let outer_operation_id =
        ProvenanceId::new(outer_operation_id.to_owned()).expect("curation outer operation id");
    let child_operation_id = derive_project_memory_fact_curation_child_operation_id(
        &outer_operation_id,
        0,
        ProjectMemoryFactCurationMutationKindV1::Add,
    )
    .expect("curation add child operation id");
    let evidence = ProjectMemoryFactCurationEvidenceV1::new(
        &FactOwnerV1::Profile,
        vec![ProjectMemoryFactCurationReviewRefV1::new(
            seed.target.clone(),
            seed.last_event_id.clone(),
        )],
        Confidence::new(0.8).expect("curation evidence confidence"),
        "canonical graph reconciliation review".to_owned(),
    )
    .expect("curation add evidence");
    let add = ProjectMemoryFactCurationAddV1::new(
        accepted_add_command(child_operation_id.as_str(), content),
        evidence,
    )
    .expect("curation add operation");
    ProjectMemoryFactCurationBatchV1::new(
        FactOwnerV1::Profile,
        outer_operation_id,
        None,
        Confidence::new(0.8).expect("curation minimum confidence"),
        vec![ProjectMemoryFactCurationOperationV1::Add(add)],
    )
    .expect("curation add batch")
}

fn merge_command(
    operation_id: &str,
    winner: &HighLevelFactSeed,
    loser: &HighLevelFactSeed,
) -> ProjectMemoryFactMergeCommandV1 {
    ProjectMemoryFactMergeCommandV1::new(
        FactOwnerV1::Profile,
        ProvenanceId::new(operation_id.to_owned()).expect("merge operation id"),
        ProjectMemoryFactMergeTargetV1::new(winner.target.clone(), winner.last_event_id.clone())
            .expect("merge winner target"),
        vec![
            ProjectMemoryFactMergeTargetV1::new(loser.target.clone(), loser.last_event_id.clone())
                .expect("merge loser target"),
        ],
        None,
        None,
    )
    .expect("merge command")
}

fn automatic_evidence(label: &str) -> ProjectMemoryAutomaticFactEvidenceV1 {
    ProjectMemoryAutomaticFactEvidenceV1::new(
        Some(format!("graph-reconciliation.{label}.evidence")),
        Some(json!({"candidate": label})),
        Some(json!({"validated": true})),
    )
    .expect("automatic fact evidence")
}

async fn seed_quarantined_automatic_fact(
    database: &Database,
    apply_id: &ProvenanceId,
    request: &ProjectMemoryFactAddCommandV1,
    evidence: &ProjectMemoryAutomaticFactEvidenceV1,
) {
    let transaction = database
        .begin_memory_write_transaction("seed quarantined automatic fact")
        .await
        .expect("begin quarantined automatic fact transaction");
    project_memory_record_automatic_fact_receipt_tx(
        &transaction,
        apply_id,
        request,
        request.input_digest(),
        evidence,
        &ProjectMemoryAutomaticFactEffectV1::Quarantined {
            reason: "canonical graph reconciliation quarantine".to_owned(),
        },
        UtcMicros(3_000_000),
    )
    .await
    .expect("record quarantined automatic fact receipt");
    transaction
        .commit()
        .await
        .expect("commit quarantined automatic fact receipt");
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
        .project_memory_write(
            &write_control(),
            |_| true,
            |_transaction| Box::pin(async { Ok::<(), FactStoreError>(()) }),
        )
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

#[tokio::test]
async fn retrieval_telemetry_does_not_reconcile_unchanged_memory_graph() {
    let (_directory, database) = database("retrieval-telemetry-reconciliation").await;
    let runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);
    let target = seed_fact_for_non_graph_write(&store, &runtime, "retrieval-telemetry").await;

    store
        .record_project_memory_fact_retrieval(
            ProjectMemoryFactRetrievalCommandV1::new(
                FactOwnerV1::Profile,
                ProvenanceId::new("graph.reconciliation.retrieval.record".to_owned())
                    .expect("retrieval record operation id"),
                vec![target],
                true,
            )
            .expect("retrieval telemetry command"),
            &write_control(),
        )
        .await
        .expect("record retrieval telemetry");

    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn idempotent_fact_replay_does_not_reconcile_unchanged_memory_graph() {
    let (_directory, database) = database("idempotent-replay-reconciliation").await;
    let runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);
    let batch = non_graph_write_fixture_batch("idempotent-replay");
    let replay = batch.clone();

    let outcome = store
        .commit_fact(batch, &write_control())
        .await
        .expect("commit idempotent replay fixture");
    assert!(matches!(outcome, FactCommitOutcome::Committed(_)));
    wait_for_reconciliation(&runtime).await;
    runtime.reconcile_calls.store(0, Ordering::SeqCst);
    runtime
        .reconciliation_observed
        .store(false, Ordering::Release);

    let outcome = store
        .commit_fact(replay, &write_control())
        .await
        .expect("replay idempotent fact commit");
    assert!(matches!(outcome, FactCommitOutcome::IdempotentReplay(_)));
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn high_level_remove_source_change_replays_and_noops_without_reconciliation() {
    let (_directory, database) = database("high-level-remove-replay-reconciliation").await;
    let runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);
    let target = seed_fact_for_non_graph_write(&store, &runtime, "high-level-remove-replay").await;

    let removed = store
        .remove_project_memory_fact(
            ProjectMemoryFactRemoveCommandV1::new(
                target.clone(),
                ProvenanceId::new("graph.reconciliation.remove.changed".to_owned())
                    .expect("changed remove operation id"),
                None,
                None,
            )
            .expect("changed remove command"),
            &write_control(),
        )
        .await
        .expect("remove fact");
    assert!(removed.was_removed());
    assert!(!removed.commit_replayed());
    wait_for_reconciliation(&runtime).await;
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 1);
    runtime.reconcile_calls.store(0, Ordering::SeqCst);
    runtime
        .reconciliation_observed
        .store(false, Ordering::Release);

    let replay = store
        .remove_project_memory_fact(
            ProjectMemoryFactRemoveCommandV1::new(
                target.clone(),
                ProvenanceId::new("graph.reconciliation.remove.changed".to_owned())
                    .expect("replayed remove operation id"),
                None,
                None,
            )
            .expect("replayed remove command"),
            &write_control(),
        )
        .await
        .expect("replay remove fact");
    assert!(replay.was_removed());
    assert!(replay.commit_replayed());
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 0);

    let no_op = store
        .remove_project_memory_fact(
            ProjectMemoryFactRemoveCommandV1::new(
                target,
                ProvenanceId::new("graph.reconciliation.remove.no-op".to_owned())
                    .expect("no-op remove operation id"),
                None,
                None,
            )
            .expect("no-op remove command"),
            &write_control(),
        )
        .await
        .expect("remove already removed fact");
    assert!(!no_op.was_removed());
    assert!(!no_op.commit_replayed());
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn high_level_add_reconciles_committed_not_replayed_or_duplicate_outcomes() {
    let (_directory, database) = database("high-level-add-outcomes-reconciliation").await;
    let runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);
    let content = "canonical high-level add graph source outcome";
    let command = accepted_add_command("graph.reconciliation.add.changed", content);

    let committed = store
        .add_project_memory_fact(command.clone(), &write_control())
        .await
        .expect("commit high-level add");
    assert_eq!(
        committed.disposition(),
        ProjectMemoryFactAddDispositionV1::Added
    );
    assert!(committed.commit_receipt().is_some());
    assert!(!committed.commit_replayed());
    wait_for_reconciliation(&runtime).await;
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 1);
    reset_reconciliation(&runtime);

    let replayed = store
        .add_project_memory_fact(command, &write_control())
        .await
        .expect("replay high-level add");
    assert!(replayed.commit_replayed());
    assert!(replayed.commit_receipt().is_some());
    assert_no_reconciliation(&runtime);

    let duplicate = store
        .add_project_memory_fact(
            accepted_add_command("graph.reconciliation.add.duplicate", content),
            &write_control(),
        )
        .await
        .expect("commit high-level add duplicate");
    assert_eq!(
        duplicate.disposition(),
        ProjectMemoryFactAddDispositionV1::NearDuplicate
    );
    assert!(duplicate.commit_receipt().is_none());
    assert!(!duplicate.commit_replayed());
    assert_no_reconciliation(&runtime);
}

#[tokio::test]
async fn high_level_update_reconciles_committed_not_replayed_outcomes() {
    let (_directory, database) = database("high-level-update-outcomes-reconciliation").await;
    let runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);
    let seed = seed_high_level_fact(
        &store,
        &runtime,
        "update-outcomes",
        "The durable runner preserves exact acknowledgements across interrupted writes.",
    )
    .await;
    let command = ProjectMemoryFactUpdateCommandV1::new(
        seed.target,
        ProvenanceId::new("graph.reconciliation.update.changed".to_owned())
            .expect("update operation id"),
        None,
        ProjectMemoryFactUpdatePatchV1::new(
            Some("canonical high-level update graph source outcome".to_owned()),
            None,
            None,
            None,
            None,
            None,
            Some(Confidence::new(0.9).expect("updated fact confidence")),
        )
        .expect("update patch"),
        None,
    )
    .expect("update command");

    let committed = store
        .update_project_memory_fact(command.clone(), &write_control())
        .await
        .expect("commit high-level update");
    assert!(!committed.commit_replayed());
    wait_for_reconciliation(&runtime).await;
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 1);
    reset_reconciliation(&runtime);

    let replayed = store
        .update_project_memory_fact(command, &write_control())
        .await
        .expect("replay high-level update");
    assert!(replayed.commit_replayed());
    assert_no_reconciliation(&runtime);
}

#[tokio::test]
async fn high_level_curation_reconciles_changed_not_replayed_or_noop_outcomes() {
    let (_directory, database) = database("high-level-curation-outcomes-reconciliation").await;
    let runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);
    let seed = seed_high_level_fact(
        &store,
        &runtime,
        "curation-outcomes",
        "A per-project lease distinguishes reconciliation responsibilities during recovery.",
    )
    .await;
    let changed = curation_add_batch(
        &seed,
        "graph.reconciliation.curation.changed",
        "canonical high-level curation graph source outcome",
    );

    let committed = store
        .apply_project_memory_fact_curation(changed.clone(), &write_control())
        .await
        .expect("commit high-level curation");
    assert!(!committed.replayed());
    assert!(!committed.changed_facts().is_empty());
    wait_for_reconciliation(&runtime).await;
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 1);
    reset_reconciliation(&runtime);

    let replayed = store
        .apply_project_memory_fact_curation(changed, &write_control())
        .await
        .expect("replay high-level curation");
    assert!(replayed.replayed());
    assert!(!replayed.changed_facts().is_empty());
    assert_no_reconciliation(&runtime);

    let no_op = store
        .apply_project_memory_fact_curation(
            curation_add_batch(&seed, "graph.reconciliation.curation.no-op", &seed.content),
            &write_control(),
        )
        .await
        .expect("commit no-op high-level curation");
    assert!(!no_op.replayed());
    assert!(no_op.changed_facts().is_empty());
    assert_no_reconciliation(&runtime);
}

#[tokio::test]
async fn high_level_merge_reconciles_committed_not_replayed_outcomes() {
    let (_directory, database) = database("high-level-merge-outcomes-reconciliation").await;
    let runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);
    let winner = seed_high_level_fact(
        &store,
        &runtime,
        "merge-winner",
        "Cascading schema improvements require a reliable migration journal before repair.",
    )
    .await;
    let loser = seed_high_level_fact(
        &store,
        &runtime,
        "merge-loser",
        "Hummingbird field recordings were tagged as audio evidence during dry-run testing.",
    )
    .await;
    let command = merge_command("graph.reconciliation.merge.changed", &winner, &loser);

    let committed = store
        .merge_project_memory_facts(command.clone(), &write_control())
        .await
        .expect("commit high-level merge");
    assert!(!committed.replayed());
    wait_for_reconciliation(&runtime).await;
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 1);
    reset_reconciliation(&runtime);

    let replayed = store
        .merge_project_memory_facts(command, &write_control())
        .await
        .expect("replay high-level merge");
    assert!(replayed.replayed());
    assert_no_reconciliation(&runtime);
}

#[tokio::test]
async fn high_level_automatic_fact_reconciles_applied_not_terminal_non_sources() {
    let (_directory, database) = database("high-level-automatic-outcomes-reconciliation").await;
    let runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);
    let apply_id = ProvenanceId::new("graph.reconciliation.automatic.applied".to_owned())
        .expect("applied automatic fact id");
    let command = accepted_add_command(
        "graph.reconciliation.automatic.applied.operation",
        "canonical high-level automatic graph source outcome",
    );
    let evidence = automatic_evidence("applied");

    let applied = store
        .apply_project_memory_automatic_fact(
            apply_id.clone(),
            command.clone(),
            evidence.clone(),
            &write_control(),
        )
        .await
        .expect("apply high-level automatic fact");
    assert_eq!(
        applied.disposition(),
        ProjectMemoryAutomaticFactApplyDispositionV1::Applied
    );
    wait_for_reconciliation(&runtime).await;
    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 1);
    reset_reconciliation(&runtime);

    let already_applied = store
        .apply_project_memory_automatic_fact(apply_id, command, evidence, &write_control())
        .await
        .expect("replay high-level automatic fact");
    assert_eq!(
        already_applied.disposition(),
        ProjectMemoryAutomaticFactApplyDispositionV1::AlreadyApplied
    );
    assert_no_reconciliation(&runtime);

    let quarantined_apply_id =
        ProvenanceId::new("graph.reconciliation.automatic.quarantined".to_owned())
            .expect("quarantined automatic fact id");
    let quarantined_command = accepted_add_command(
        "graph.reconciliation.automatic.quarantined.operation",
        "canonical high-level automatic quarantine outcome",
    );
    let quarantined_evidence = automatic_evidence("quarantined");
    seed_quarantined_automatic_fact(
        &database,
        &quarantined_apply_id,
        &quarantined_command,
        &quarantined_evidence,
    )
    .await;

    let quarantined = store
        .apply_project_memory_automatic_fact(
            quarantined_apply_id,
            quarantined_command,
            quarantined_evidence,
            &write_control(),
        )
        .await
        .expect("observe quarantined high-level automatic fact");
    assert_eq!(
        quarantined.disposition(),
        ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined
    );
    assert_no_reconciliation(&runtime);
}

#[tokio::test]
async fn feedback_telemetry_does_not_reconcile_unchanged_memory_graph() {
    let (_directory, database) = database("feedback-telemetry-reconciliation").await;
    let runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);
    let target = seed_fact_for_non_graph_write(&store, &runtime, "feedback-telemetry").await;

    store
        .record_project_memory_fact_feedback(
            ProjectMemoryFactFeedbackCommandV1::new(
                target,
                ProvenanceId::new("graph.reconciliation.feedback.record".to_owned())
                    .expect("feedback record operation id"),
                None,
                ProjectMemoryFactFeedbackActionV1::Helpful,
                None,
                Some("graph reconciliation regression".to_owned()),
                Some("feedback does not change graph source rows".to_owned()),
            )
            .expect("feedback telemetry command"),
            &write_control(),
        )
        .await
        .expect("record feedback telemetry");

    assert_eq!(runtime.reconcile_calls.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.publish_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn settled_workload_telemetry_stays_flat_until_exact_source_mutation() {
    let (_directory, database) = database("settled-workload-telemetry").await;
    let _runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);
    let observer = database.project_memory_reconciliation_telemetry_observer();
    let expected_seed_passes = observer.snapshot().reconciliation_passes + 1;
    let seed = add_high_level_source_fact(
        &store,
        "settled-workload",
        "A settled memory graph must not reconcile for retrieval or feedback telemetry.",
    )
    .await;
    wait_for_completed_reconciliation_pass(&observer, expected_seed_passes).await;
    let seeded = observer.snapshot();
    assert_eq!(seeded.reconciliation_passes, expected_seed_passes);

    store
        .record_project_memory_fact_retrieval(
            ProjectMemoryFactRetrievalCommandV1::new(
                FactOwnerV1::Profile,
                ProvenanceId::new("graph.reconciliation.settled.retrieval".to_owned())
                    .expect("settled retrieval operation id"),
                vec![seed.target.clone()],
                true,
            )
            .expect("settled retrieval command"),
            &write_control(),
        )
        .await
        .expect("record settled retrieval telemetry");
    store
        .record_project_memory_fact_feedback(
            ProjectMemoryFactFeedbackCommandV1::new(
                seed.target.clone(),
                ProvenanceId::new("graph.reconciliation.settled.feedback".to_owned())
                    .expect("settled feedback operation id"),
                None,
                ProjectMemoryFactFeedbackActionV1::Helpful,
                None,
                Some("settled workload telemetry".to_owned()),
                Some("feedback does not change graph source rows".to_owned()),
            )
            .expect("settled feedback command"),
            &write_control(),
        )
        .await
        .expect("record settled feedback telemetry");
    let settled = observer.snapshot();

    assert_eq!(settled.reconciliation_passes, seeded.reconciliation_passes);
    assert_eq!(settled.source_rows_loaded, seeded.source_rows_loaded);
    assert_eq!(settled.source_bytes_loaded, seeded.source_bytes_loaded);
    assert_eq!(settled.publication_attempts, seeded.publication_attempts);
    assert_eq!(
        settled.retained_reconciliation_task_count,
        seeded.retained_reconciliation_task_count
    );
    assert_eq!(
        settled.retained_graph_owner_count,
        seeded.retained_graph_owner_count
    );

    store
        .record_project_memory_fact_retrieval(
            ProjectMemoryFactRetrievalCommandV1::new(
                FactOwnerV1::Profile,
                ProvenanceId::new("graph.reconciliation.settled.retrieval".to_owned())
                    .expect("replayed settled retrieval operation id"),
                vec![seed.target.clone()],
                true,
            )
            .expect("replayed settled retrieval command"),
            &write_control(),
        )
        .await
        .expect("replay settled retrieval telemetry");
    store
        .record_project_memory_fact_feedback(
            ProjectMemoryFactFeedbackCommandV1::new(
                seed.target,
                ProvenanceId::new("graph.reconciliation.settled.feedback".to_owned())
                    .expect("replayed settled feedback operation id"),
                None,
                ProjectMemoryFactFeedbackActionV1::Helpful,
                None,
                Some("settled workload telemetry".to_owned()),
                Some("feedback does not change graph source rows".to_owned()),
            )
            .expect("replayed settled feedback command"),
            &write_control(),
        )
        .await
        .expect("replay settled feedback telemetry");
    let repeated = observer.snapshot();

    assert_eq!(
        repeated.reconciliation_passes,
        settled.reconciliation_passes
    );
    assert_eq!(repeated.source_rows_loaded, settled.source_rows_loaded);
    assert_eq!(repeated.source_bytes_loaded, settled.source_bytes_loaded);
    assert_eq!(repeated.publication_attempts, settled.publication_attempts);
    assert_eq!(
        repeated.retained_reconciliation_task_count,
        settled.retained_reconciliation_task_count
    );
    assert_eq!(
        repeated.retained_graph_owner_count,
        settled.retained_graph_owner_count
    );

    let expected_reconciliation_passes = repeated.reconciliation_passes + 1;
    add_high_level_source_fact(
        &store,
        "settled-workload-source-mutation",
        "A real graph source mutation must reconcile exactly once after settlement.",
    )
    .await;
    wait_for_completed_reconciliation_pass(&observer, expected_reconciliation_passes).await;
    let source_mutated = observer.snapshot();

    assert_eq!(
        source_mutated.reconciliation_passes,
        repeated.reconciliation_passes + 1
    );
    assert_eq!(
        source_mutated.publication_attempts,
        repeated.publication_attempts + 1
    );
    assert!(source_mutated.source_rows_loaded > repeated.source_rows_loaded);
    assert!(source_mutated.source_bytes_loaded > repeated.source_bytes_loaded);
    assert_eq!(
        source_mutated.retained_reconciliation_task_count,
        repeated.retained_reconciliation_task_count
    );
    assert_eq!(
        source_mutated.retained_graph_owner_count,
        repeated.retained_graph_owner_count
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caller_drop_after_high_level_remove_commit_start_cannot_lose_reconciliation() {
    let (_directory, database) = database("dropped-high-level-remove-caller").await;
    let runtime = bind_runtime(&database);
    let store = DatabaseFactStore::new(&database);
    let seed = seed_high_level_fact(
        &store,
        &runtime,
        "dropped-high-level-remove",
        "A detached high-level remove must preserve its durable graph source mutation.",
    )
    .await;
    let target = seed.target;
    let fact_id = target.fact_id().clone();
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
            .remove_project_memory_fact(
                ProjectMemoryFactRemoveCommandV1::new(
                    target,
                    ProvenanceId::new("graph.reconciliation.dropped-high-level-remove".to_owned())
                        .expect("dropped high-level remove operation id"),
                    None,
                    None,
                )
                .expect("dropped high-level remove command"),
                &control,
            )
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
    assert!(
        current.is_none(),
        "owned high-level remove must leave no current fact after caller drop"
    );
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
        .cancel()
        .expect("cancel bound reconciler");

    assert_eq!(
        super::schedule_project_memory_graph_reconciliation(database),
        ProjectMemoryGraphReconciliationScheduleV1::LifecycleClosed
    );
}

#[tokio::test]
async fn reconciliation_owner_uses_a_weak_bound_runtime_after_database_drop() {
    let (_directory, database) = database("weak-owner-runtime").await;
    let runtime = bind_runtime(&database);
    let runtime_weak = Arc::downgrade(&runtime);
    let owner = database
        .memory_graph_reconciliation_task_owner()
        .expect("bound runtime has reconciliation owner");

    drop(runtime);
    assert!(
        runtime_weak.upgrade().is_some(),
        "the bound database owns its live graph runtime"
    );
    drop(database);
    assert!(
        runtime_weak.upgrade().is_none(),
        "the reconciliation owner must not retain the bound graph runtime"
    );
    assert_eq!(
        owner.cancel(),
        Err(MemoryGraphReconciliationCancelErrorV1::RuntimeUnavailable)
    );
}

#[tokio::test]
async fn retirement_reservation_reports_a_distinct_schedule_outcome_and_drops_cleanly() {
    let (_directory, database) = database("reserved-reconciliation").await;
    let runtime = bind_runtime(&database);
    let owner = database
        .memory_graph_reconciliation_task_owner()
        .expect("bound runtime has reconciliation owner");

    let reservation = owner
        .reserve_retirement()
        .expect("idle reconciler retirement reservation");
    assert_eq!(
        super::schedule_project_memory_graph_reconciliation(database.clone()),
        ProjectMemoryGraphReconciliationScheduleV1::Retiring
    );
    drop(reservation);

    assert_eq!(
        super::schedule_project_memory_graph_reconciliation(database),
        ProjectMemoryGraphReconciliationScheduleV1::Scheduled
    );
    wait_for_reconciliation(&runtime).await;
    owner.shutdown().await.expect("join reconciler worker");
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
        !runtime.reconciliation_closed.load(Ordering::Acquire),
        "reconciliation shutdown must not close the graph attachment outside GraphDb retirement"
    );
    assert!(!owner.running());
}
