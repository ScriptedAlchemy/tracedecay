use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use rusqlite::{Connection, Transaction};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope,
    storage::{StorageTelemetryReadV1, StoreKeyV1, StoreSizeTelemetryPort},
};
use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId};
use tracedecay_store::{
    AdmissionConfigV1, CommitSequenceV1, LocatorDigest, OperationPriorityV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeReadCoverageV1,
    RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestProbeV1,
    SnapshotLeaseV1, StorageRuntimeErrorV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use super::*;
use crate::{
    SqliteStoreSizeTelemetryPort,
    read_consistency::{
        ReadConsistencyConfig, ReadConsistencyCoordinator, RetainedSnapshotRegistry,
        RetainedSnapshotState,
    },
    watermark::CommittedWatermarkPublisher,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

#[derive(Clone)]
struct CountExecutor;

impl ReaderQueryExecutor for CountExecutor {
    fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        let count: i64 = snapshot
            .query_row("SELECT count(*) FROM markers", [], |row| row.get(0))
            .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
                operation: format!("count markers: {error}"),
            })?;
        RuntimeReadOutcomeV1::new(
            Some(RuntimeReadResultV1::GraphQuickCheck {
                healthy: count == 1,
            }),
            RuntimeReadCoverageV1::Latest { observed: None },
        )
        .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
            operation: format!("build test read: {error}"),
        })
    }
}

#[derive(Clone)]
struct SlowExecutor {
    delay: Duration,
}

impl ReaderQueryExecutor for SlowExecutor {
    fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        std::thread::sleep(self.delay);
        CountExecutor.execute_read(snapshot, request)
    }
}

struct TestStore {
    _directory: tempfile::TempDir,
    path: PathBuf,
    binding: StoreRuntimeBindingV1,
}

impl TestStore {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reader.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .execute_batch("CREATE TABLE markers(value INTEGER NOT NULL);")
            .unwrap();
        let binding = serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.reader",
                "profile_id": "profile.reader",
                "scope": { "kind": "project", "project_id": "project.reader" }
            },
            "incarnation": 1,
            "authority_epoch": 7
        }))
        .unwrap();
        Self {
            _directory: directory,
            path,
            binding,
        }
    }

    fn locator(&self) -> ExistingReaderLocator {
        let locator = VerifiedStoreLocatorV1::new(
            self.binding.shard_id.clone(),
            self.binding.incarnation,
            LocatorDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
        );
        ExistingReaderLocator::new(self.binding.clone(), locator, self.path.clone()).unwrap()
    }
}

fn telemetry_context(scope: ResolvedScope) -> RequestContext {
    let actor = ActorId::new("actor.storage-telemetry-test").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.storage-telemetry-test").unwrap(),
        1,
        ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.storage.telemetry").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.storage.telemetry.read").unwrap()]),
        DisclosureClass::Metadata,
    )
    .unwrap();
    RequestContext::new(
        actor,
        scope,
        grant,
        RequestId::new("request.storage-telemetry-test").unwrap(),
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationContext::active("cancel.storage-telemetry-test").unwrap(),
    )
    .unwrap()
}

fn telemetry_scope() -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.storage-telemetry-test").unwrap(),
        RepositoryId::new("repository.storage-telemetry-test").unwrap(),
        WorktreeId::new("worktree.storage-telemetry-test").unwrap(),
        None,
    )
    .unwrap()
}

struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: Arc<AtomicU8>,
}

impl Probe {
    fn for_request(request: &RuntimeReadRequestV1) -> Self {
        Self {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
            interruption: Arc::new(AtomicU8::new(0)),
        }
    }

    fn cancel(&self) {
        self.interruption.store(1, Ordering::SeqCst);
    }
}

impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }
    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }
    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        match self.interruption.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(RuntimeInterruptionV1::Cancelled),
            _ => Some(RuntimeInterruptionV1::DeadlineExceeded),
        }
    }
}

fn request(binding: &StoreRuntimeBindingV1, priority: OperationPriorityV1) -> RuntimeReadRequestV1 {
    let priority = match priority {
        OperationPriorityV1::Health => "health",
        OperationPriorityV1::Foreground => "foreground",
        OperationPriorityV1::Background => "background",
    };
    serde_json::from_value(serde_json::json!({
        "binding": binding,
        "consistency": { "kind": "latest_available" },
        "operation": { "kind": "graph_quick_check" },
        "priority": priority,
        "admission_bytes": 64,
        "control": {
            "requested_at": 1,
            "deadline": { "deadline_id": "deadline.reader" },
            "cancellation": { "cancellation_id": "cancellation.reader", "generation": 1 }
        }
    }))
    .unwrap()
}

fn exact_request(binding: &StoreRuntimeBindingV1, lease: &SnapshotLeaseV1) -> RuntimeReadRequestV1 {
    serde_json::from_value(serde_json::json!({
        "binding": binding,
        "consistency": { "kind": "exact_snapshot", "lease": lease },
        "operation": { "kind": "graph_quick_check" },
        "priority": "foreground",
        "admission_bytes": 64,
        "control": {
            "requested_at": 1,
            "deadline": { "deadline_id": "deadline.reader" },
            "cancellation": { "cancellation_id": "cancellation.reader", "generation": 1 }
        }
    }))
    .unwrap()
}

fn at_least_request(binding: &StoreRuntimeBindingV1, commit_sequence: u64) -> RuntimeReadRequestV1 {
    serde_json::from_value(serde_json::json!({
        "binding": binding,
        "consistency": { "kind": "at_least", "commit_sequence": commit_sequence },
        "operation": { "kind": "graph_quick_check" },
        "priority": "foreground",
        "admission_bytes": 64,
        "control": {
            "requested_at": 1,
            "deadline": { "deadline_id": "deadline.reader" },
            "cancellation": { "cancellation_id": "cancellation.reader", "generation": 1 }
        }
    }))
    .unwrap()
}

fn snapshot_lease(binding: &StoreRuntimeBindingV1) -> SnapshotLeaseV1 {
    serde_json::from_value(serde_json::json!({
        "lease_id": "lease.reader",
        "snapshot_id": "snapshot.reader",
        "watermark": {
            "shard_id": binding.shard_id,
            "incarnation": binding.incarnation,
            "authority_epoch": binding.authority_epoch,
            "commit_sequence": 8
        },
        "acquired_at": 1,
        "expires_at": 4_102_444_800_000_000_i64
    }))
    .unwrap()
}

fn expiring_snapshot_lease(
    binding: &StoreRuntimeBindingV1,
    lease_id: &str,
    snapshot_id: &str,
    expires_at: i64,
) -> SnapshotLeaseV1 {
    serde_json::from_value(serde_json::json!({
        "lease_id": lease_id,
        "snapshot_id": snapshot_id,
        "watermark": {
            "shard_id": binding.shard_id,
            "incarnation": binding.incarnation,
            "authority_epoch": binding.authority_epoch,
            "commit_sequence": 8
        },
        "acquired_at": expires_at - 1_000_000,
        "expires_at": expires_at
    }))
    .unwrap()
}

fn utc_now_micros() -> i64 {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros();
    i64::try_from(micros).unwrap()
}

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(future)
}

fn healthy(outcome: &RuntimeReadOutcomeV1) -> bool {
    matches!(
        outcome.value(),
        Some(RuntimeReadResultV1::GraphQuickCheck { healthy: true })
    )
}

fn two_reader_budget() -> tracedecay_store::ReaderBudgetV1 {
    let mut budget = AdmissionConfigV1::default().readers;
    budget.min_per_hot_shard = 2;
    budget.max_per_hot_shard = 2;
    budget
}

#[test]
fn reserved_health_reader_reports_exact_store_size_pragmas() {
    let store = TestStore::new();
    let pool = ReaderPool::start(
        store.locator(),
        AdmissionConfigV1::default().readers,
        CountExecutor,
    )
    .unwrap();

    let sample = pool
        .read_store_size(Duration::from_millis(100), || None)
        .expect("store size sample");

    assert!(sample.page_size_bytes > 0);
    assert!(sample.page_count > 0);
    assert!(sample.freelist_pages <= sample.page_count);
    let table_sizes = pool
        .read_table_sizes(Duration::from_millis(100), || None)
        .expect("table size samples");
    assert!(
        table_sizes
            .iter()
            .any(|sample| sample.table_name == "markers" && sample.bytes == 0),
        "an empty table has zero payload bytes rather than one page of fabricated payload"
    );
    let snapshot = pool.snapshot();
    assert_eq!(snapshot.leased_health, 0);
    assert_eq!(snapshot.available_health, 1);
}

#[test]
fn application_telemetry_port_reads_real_store_size() {
    let store = TestStore::new();
    let pool = ReaderPool::start(
        store.locator(),
        AdmissionConfigV1::default().readers,
        CountExecutor,
    )
    .unwrap();
    let scope = telemetry_scope();
    let context = telemetry_context(scope.clone());
    let port = SqliteStoreSizeTelemetryPort::new(
        crate::migration_sql::MigrationSqlHandle::attach_read_only(&pool),
        StoreKeyV1::new("reader.db").unwrap(),
        scope,
        Duration::from_millis(100),
    );

    let read = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(port.store_size(&context, &StoreKeyV1::new("reader.db").unwrap()));

    let StorageTelemetryReadV1::Observed { sample } = read else {
        panic!("production telemetry port must observe the retained store");
    };
    assert!(sample.page_size_bytes > 0);
    assert!(sample.page_count > 0);
    assert!(sample.freelist_pages <= sample.page_count);
}

#[test]
fn application_telemetry_port_compares_table_payload_watermarks() {
    let store = TestStore::new();
    let pool = ReaderPool::start(
        store.locator(),
        AdmissionConfigV1::default().readers,
        CountExecutor,
    )
    .unwrap();
    let scope = telemetry_scope();
    let context = telemetry_context(scope.clone());
    let port = SqliteStoreSizeTelemetryPort::new(
        crate::migration_sql::MigrationSqlHandle::attach_read_only(&pool),
        StoreKeyV1::new("reader.db").unwrap(),
        scope,
        Duration::from_millis(100),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    assert!(
        runtime
            .block_on(port.table_growth(&context, &StoreKeyV1::new("reader.db").unwrap()))
            .is_empty(),
        "the first read establishes a baseline"
    );
    let connection = Connection::open(&store.path).unwrap();
    for value in 0..256 {
        connection
            .execute("INSERT INTO markers(value) VALUES (?1)", [value])
            .unwrap();
    }

    let growth =
        runtime.block_on(port.table_growth(&context, &StoreKeyV1::new("reader.db").unwrap()));
    let markers = growth
        .iter()
        .find(|sample| sample.table.as_str() == "markers")
        .expect("markers growth sample");
    assert!(markers.current_bytes > markers.previous_bytes);
}

#[test]
fn deferred_snapshot_excludes_uncommitted_and_later_committed_rows() {
    let store = TestStore::new();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let read = request(&store.binding, OperationPriorityV1::Foreground);
    let probe = Probe::for_request(&read);
    let mut lease = pool.acquire(&read, &probe, Duration::ZERO).unwrap();
    let mut snapshot = lease.begin_snapshot().unwrap();

    let mut writer = Connection::open(&store.path).unwrap();
    let transaction = writer.transaction().unwrap();
    transaction
        .execute("INSERT INTO markers(value) VALUES (1)", [])
        .unwrap();
    assert!(!healthy(&snapshot.execute(read.clone(), &probe).unwrap()));
    transaction.commit().unwrap();
    assert!(
        !healthy(&snapshot.execute(read.clone(), &probe).unwrap()),
        "one lease must remain on its first-read snapshot"
    );
    drop(snapshot);

    let mut next = lease.begin_snapshot().unwrap();
    assert!(healthy(&next.execute(read, &probe).unwrap()));
}

#[test]
fn saturated_general_lane_does_not_consume_reserved_health_reader() {
    let store = TestStore::new();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let regular = request(&store.binding, OperationPriorityV1::Foreground);
    let regular_probe = Probe::for_request(&regular);
    let _first = pool
        .acquire(&regular, &regular_probe, Duration::ZERO)
        .unwrap();
    let _second = pool
        .acquire(&regular, &regular_probe, Duration::ZERO)
        .unwrap();
    assert!(matches!(
        pool.acquire(&regular, &regular_probe, Duration::ZERO),
        Err(ReaderAcquireError::Saturated {
            scope: tracedecay_store::SaturationScopeV1::ReaderPool
        })
    ));

    let health = request(&store.binding, OperationPriorityV1::Health);
    let health_probe = Probe::for_request(&health);
    let _health = pool
        .acquire(&health, &health_probe, Duration::ZERO)
        .unwrap();
    let snapshot = pool.snapshot();
    assert_eq!((snapshot.leased_general, snapshot.leased_health), (2, 1));
}

#[test]
fn drain_rejects_new_general_acquisitions_but_preserves_existing_and_health_leases() {
    let store = TestStore::new();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let regular = request(&store.binding, OperationPriorityV1::Foreground);
    let regular_probe = Probe::for_request(&regular);
    let mut existing = pool
        .acquire(&regular, &regular_probe, Duration::ZERO)
        .unwrap();

    pool.begin_drain();

    assert_eq!(pool.snapshot().state, ReaderPoolState::Draining);
    assert!(matches!(
        pool.acquire(&regular, &regular_probe, Duration::ZERO),
        Err(ReaderAcquireError::Interrupted {
            reason: tracedecay_store::UnavailableReasonV1::Draining
        })
    ));
    let mut snapshot = existing.begin_snapshot().unwrap();
    assert!(!healthy(
        &snapshot.execute(regular.clone(), &regular_probe).unwrap()
    ));

    let health = request(&store.binding, OperationPriorityV1::Health);
    let health_probe = Probe::for_request(&health);
    let _health = pool
        .acquire(&health, &health_probe, Duration::ZERO)
        .unwrap();
}

#[test]
fn facade_reports_drain_truthfully_while_reserved_health_remains_usable() {
    let store = TestStore::new();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let registry = SqliteRetainedSnapshotRegistry::new(pool.clone());
    let mut observed = snapshot_lease(&store.binding).watermark;
    observed.commit_sequence = CommitSequenceV1(0);
    let commits = CommittedWatermarkPublisher::with_initial_watermarks([observed]).unwrap();
    pool.begin_drain();
    let facade = ReaderFacade::new(
        pool,
        ReadConsistencyCoordinator::new(ReadConsistencyConfig {
            max_wait: Duration::ZERO,
            cancellation_poll_interval: Duration::from_millis(1),
        }),
        commits.subscribe(),
        registry,
        Duration::ZERO,
    );

    let regular = request(&store.binding, OperationPriorityV1::Foreground);
    let regular_probe = Probe::for_request(&regular);
    let regular_outcome = run(facade.read(regular, &regular_probe)).unwrap();
    assert!(matches!(
        regular_outcome.coverage(),
        RuntimeReadCoverageV1::Unavailable {
            reason: tracedecay_store::UnavailableReasonV1::Draining,
            ..
        }
    ));

    let health = request(&store.binding, OperationPriorityV1::Health);
    let health_probe = Probe::for_request(&health);
    let health_outcome = run(facade.read(health, &health_probe)).unwrap();
    assert!(health_outcome.value().is_some());
}

#[test]
fn cancellation_preempts_acquisition_without_changing_accounting() {
    let store = TestStore::new();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let read = request(&store.binding, OperationPriorityV1::Foreground);
    let probe = Probe::for_request(&read);
    probe.cancel();
    let before = pool.snapshot();
    assert!(matches!(
        pool.acquire(&read, &probe, Duration::from_secs(1)),
        Err(ReaderAcquireError::Interrupted {
            reason: tracedecay_store::UnavailableReasonV1::Cancelled
        })
    ));
    assert_eq!(pool.snapshot(), before);
}

#[test]
fn retirement_is_elapsed_time_modelled_and_never_retires_the_floor() {
    let store = TestStore::new();
    let mut budget = two_reader_budget();
    budget.max_per_hot_shard = 3;
    let pool = ReaderPool::start(store.locator(), budget, CountExecutor).unwrap();
    let read = request(&store.binding, OperationPriorityV1::Foreground);
    let probe = Probe::for_request(&read);
    let leases = (0..3)
        .map(|_| pool.acquire(&read, &probe, Duration::ZERO).unwrap())
        .collect::<Vec<_>>();
    drop(leases);

    assert_eq!(pool.snapshot().general_workers, 3);
    assert_eq!(
        pool.retire_idle_at(Instant::now() + Duration::from_secs(60)),
        1
    );
    assert_eq!(pool.snapshot().general_workers, 2);
}

#[test]
fn dropping_snapshot_and_reader_lease_restores_capacity() {
    let store = TestStore::new();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let read = request(&store.binding, OperationPriorityV1::Foreground);
    let probe = Probe::for_request(&read);
    {
        let mut lease = pool.acquire(&read, &probe, Duration::ZERO).unwrap();
        let snapshot = lease.begin_snapshot().unwrap();
        drop(snapshot);
        assert_eq!(pool.snapshot().leased_general, 1);
    }
    let state = pool.snapshot();
    assert_eq!(state.leased_general, 0);
    assert_eq!(state.available_general, 2);
}

#[test]
fn retained_snapshot_uses_a_pool_connection_and_facade_executes_that_exact_view() {
    let store = TestStore::new();
    Connection::open(&store.path)
        .unwrap()
        .execute("INSERT INTO markers(value) VALUES (1)", [])
        .unwrap();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let lease = snapshot_lease(&store.binding);
    let exact = exact_request(&store.binding, &lease);
    let probe = Probe::for_request(&exact);
    let registry = SqliteRetainedSnapshotRegistry::new(pool.clone());
    let worker_count = pool.snapshot().general_workers;

    registry
        .retain(lease.clone(), &exact, &probe, Duration::ZERO)
        .unwrap();
    assert_eq!(pool.snapshot().general_workers, worker_count);
    assert!(matches!(
        registry.lookup(&lease.lease_id),
        RetainedSnapshotState::Retained(found) if *found == lease
    ));

    Connection::open(&store.path)
        .unwrap()
        .execute("INSERT INTO markers(value) VALUES (2)", [])
        .unwrap();
    let commits =
        CommittedWatermarkPublisher::with_initial_watermarks([lease.watermark.clone()]).unwrap();
    let facade = ReaderFacade::new(
        pool,
        ReadConsistencyCoordinator::new(ReadConsistencyConfig {
            max_wait: Duration::ZERO,
            cancellation_poll_interval: Duration::from_millis(1),
        }),
        commits.subscribe(),
        registry,
        Duration::ZERO,
    );
    let outcome = run(facade.read(exact, &probe)).unwrap();

    assert!(healthy(&outcome));
    assert!(matches!(
        outcome.coverage(),
        RuntimeReadCoverageV1::Complete { .. }
    ));
}

#[test]
fn retain_reclaims_expired_capacity_without_stale_release_of_reused_id() {
    let store = TestStore::new();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let registry = SqliteRetainedSnapshotRegistry::new(pool.clone());
    let expires_at = utc_now_micros() + 250_000;
    let expired = expiring_snapshot_lease(
        &store.binding,
        "lease.reused",
        "snapshot.expired",
        expires_at,
    );
    let other_expired =
        expiring_snapshot_lease(&store.binding, "lease.other", "snapshot.other", expires_at);
    for lease in [&expired, &other_expired] {
        let exact = exact_request(&store.binding, lease);
        registry
            .retain(
                lease.clone(),
                &exact,
                &Probe::for_request(&exact),
                Duration::ZERO,
            )
            .unwrap();
    }
    assert_eq!(pool.snapshot().leased_general, 2);
    std::thread::sleep(Duration::from_millis(300));

    let replacement = expiring_snapshot_lease(
        &store.binding,
        "lease.reused",
        "snapshot.replacement",
        4_102_444_800_000_000,
    );
    let exact = exact_request(&store.binding, &replacement);
    registry
        .retain(
            replacement.clone(),
            &exact,
            &Probe::for_request(&exact),
            Duration::ZERO,
        )
        .unwrap();

    assert_eq!(pool.snapshot().leased_general, 1);
    assert!(!registry.release(&expired));
    let stale_exact = exact_request(&store.binding, &expired);
    let stale_probe = Probe::for_request(&stale_exact);
    assert!(matches!(
        registry.execute_exact(&expired.lease_id, stale_exact, &stale_probe),
        Ok(RetainedExecution::Unavailable(
            tracedecay_store::UnavailableReasonV1::SnapshotNotRetained
        ))
    ));
    assert!(matches!(
        registry.lookup(&replacement.lease_id),
        RetainedSnapshotState::Retained(found) if *found == replacement
    ));
    assert!(registry.release(&replacement));
}

#[test]
fn facade_does_not_query_when_at_least_coverage_is_stale() {
    let store = TestStore::new();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let mut observed = snapshot_lease(&store.binding).watermark;
    observed.commit_sequence = CommitSequenceV1(4);
    let commits = CommittedWatermarkPublisher::with_initial_watermarks([observed]).unwrap();
    let registry = SqliteRetainedSnapshotRegistry::new(pool.clone());
    let facade = ReaderFacade::new(
        pool,
        ReadConsistencyCoordinator::new(ReadConsistencyConfig {
            max_wait: Duration::ZERO,
            cancellation_poll_interval: Duration::from_millis(1),
        }),
        commits.subscribe(),
        registry,
        Duration::ZERO,
    );
    let read = at_least_request(&store.binding, 5);
    let probe = Probe::for_request(&read);

    let outcome = run(facade.read(read, &probe)).unwrap();

    assert!(outcome.value().is_none());
    assert!(matches!(
        outcome.coverage(),
        RuntimeReadCoverageV1::Stale { .. }
    ));
}

#[test]
fn cancellation_bounds_query_return_even_when_the_executor_is_still_running() {
    let store = TestStore::new();
    let pool = ReaderPool::start(
        store.locator(),
        two_reader_budget(),
        SlowExecutor {
            delay: Duration::from_millis(250),
        },
    )
    .unwrap();
    let read = request(&store.binding, OperationPriorityV1::Foreground);
    let probe = Probe::for_request(&read);
    let cancellation = Arc::clone(&probe.interruption);
    let mut lease = pool.acquire(&read, &probe, Duration::ZERO).unwrap();
    let mut snapshot = lease.begin_snapshot().unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        cancellation.store(1, Ordering::SeqCst);
    });

    let started = Instant::now();
    let outcome = snapshot.execute(read, &probe).unwrap();
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(matches!(
        outcome.coverage(),
        RuntimeReadCoverageV1::Unavailable {
            reason: tracedecay_store::UnavailableReasonV1::Cancelled,
            ..
        }
    ));

    let drop_started = Instant::now();
    drop(snapshot);
    drop(lease);
    assert!(drop_started.elapsed() < Duration::from_millis(100));
}
