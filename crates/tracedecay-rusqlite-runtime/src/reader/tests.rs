use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use rusqlite::{Connection, Transaction};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope,
    storage::{
        StorageTelemetryReadV1, StoreKeyV1, StoreSizeTelemetryPort, TableGrowthTelemetryReadV1,
    },
};
use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId};
use tracedecay_store::{
    AdmissionConfigV1, LocatorDigest, OperationPriorityV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeReadCoverageV1, RuntimeReadOutcomeV1,
    RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestProbeV1, StorageRuntimeErrorV1,
    StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use super::*;
use crate::SqliteStoreSizeTelemetryPort;
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

enum SecondPollAction {
    Cancel,
    Release(ReaderLease<CountExecutor>),
}

struct SecondPollProbe {
    base: Probe,
    polls: AtomicU8,
    action: Mutex<Option<SecondPollAction>>,
}

impl SecondPollProbe {
    fn cancelling(request: &RuntimeReadRequestV1) -> Self {
        Self {
            base: Probe::for_request(request),
            polls: AtomicU8::new(0),
            action: Mutex::new(Some(SecondPollAction::Cancel)),
        }
    }

    fn releasing(request: &RuntimeReadRequestV1, lease: ReaderLease<CountExecutor>) -> Self {
        Self {
            base: Probe::for_request(request),
            polls: AtomicU8::new(0),
            action: Mutex::new(Some(SecondPollAction::Release(lease))),
        }
    }
}

impl RuntimeRequestProbeV1 for SecondPollProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        self.base.cancellation_identity()
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        self.base.deadline_identity()
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        if self.polls.fetch_add(1, Ordering::SeqCst) > 0 {
            match self.action.lock().unwrap().take() {
                Some(SecondPollAction::Cancel) => self.base.cancel(),
                Some(SecondPollAction::Release(lease)) => drop(lease),
                None => {}
            }
        }
        self.base.interruption()
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
fn checkpoint_pressure_blocks_general_reads_but_preserves_health() {
    let store = TestStore::new();
    let (pressure_tx, pressure_rx) =
        tokio::sync::watch::channel(crate::CheckpointPressure::BlockGeneral {
            wal: crate::CheckpointWal {
                frames: 64,
                bytes: 256 * 1024 * 1024,
            },
            blockers: crate::CheckpointBlockers::default(),
        });
    let pool = ReaderPool::start_with_checkpoint_pressure(
        store.locator(),
        two_reader_budget(),
        CountExecutor,
        Some(pressure_rx),
    )
    .unwrap();
    let general = request(&store.binding, OperationPriorityV1::Foreground);
    let general_probe = Probe::for_request(&general);
    assert!(matches!(
        pool.acquire(&general, &general_probe, Duration::from_millis(20)),
        Err(ReaderAcquireError::Saturated { .. })
    ));

    let health = request(&store.binding, OperationPriorityV1::Health);
    let health_probe = Probe::for_request(&health);
    {
        let mut lease = pool
            .acquire(&health, &health_probe, Duration::from_millis(20))
            .unwrap();
        let mut snapshot = lease.begin_snapshot().unwrap();
        assert!(matches!(
            snapshot
                .execute(health.clone(), &health_probe)
                .unwrap()
                .value(),
            Some(RuntimeReadResultV1::GraphQuickCheck { .. })
        ));
    }

    pressure_tx
        .send(crate::CheckpointPressure::Open)
        .expect("reader holds pressure receiver");
    let mut lease = pool
        .acquire(&general, &general_probe, Duration::from_millis(20))
        .unwrap();
    let mut snapshot = lease.begin_snapshot().unwrap();
    assert!(matches!(
        snapshot
            .execute(general.clone(), &general_probe)
            .unwrap()
            .value(),
        Some(RuntimeReadResultV1::GraphQuickCheck { .. })
    ));
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
fn migration_health_snapshot_retires_its_reader_after_drop() {
    let store = TestStore::new();
    let pool = ReaderPool::start(
        store.locator(),
        AdmissionConfigV1::default().readers,
        CountExecutor,
    )
    .unwrap();

    let snapshot = pool
        .begin_migration_health_snapshot(Duration::from_millis(100))
        .unwrap();
    assert_eq!(pool.snapshot().leased_health, 1);
    drop(snapshot);

    let state = pool.snapshot();
    assert_eq!(state.leased_health, 0);
    assert_eq!(state.health_workers, 0);
    assert_eq!(state.available_health, 0);
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

    let baseline =
        runtime.block_on(port.table_growth(&context, &StoreKeyV1::new("reader.db").unwrap()));
    assert!(
        matches!(
            baseline,
            TableGrowthTelemetryReadV1::BaselineEstablished {
                tables_observed,
                ..
            } if tables_observed > 0
        ),
        "the first read must report baseline establishment, got {baseline:?}"
    );
    let connection = Connection::open(&store.path).unwrap();
    for value in 0..256 {
        connection
            .execute("INSERT INTO markers(value) VALUES (?1)", [value])
            .unwrap();
    }

    let growth =
        runtime.block_on(port.table_growth(&context, &StoreKeyV1::new("reader.db").unwrap()));
    let TableGrowthTelemetryReadV1::Observed { samples, .. } = growth else {
        panic!("the second read must compare table watermarks");
    };
    let markers = samples
        .iter()
        .find(|sample| sample.table.as_str() == "markers")
        .expect("markers growth sample");
    assert!(markers.current_bytes > markers.previous_bytes);
}

#[test]
fn application_telemetry_port_marks_new_table_baseline_pending() {
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

    let baseline =
        runtime.block_on(port.table_growth(&context, &StoreKeyV1::new("reader.db").unwrap()));
    assert!(matches!(
        baseline,
        TableGrowthTelemetryReadV1::BaselineEstablished { .. }
    ));

    let connection = Connection::open(&store.path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE created_after_baseline (id INTEGER PRIMARY KEY, payload TEXT);
             INSERT INTO created_after_baseline(payload) VALUES ('new');",
        )
        .unwrap();

    let read =
        runtime.block_on(port.table_growth(&context, &StoreKeyV1::new("reader.db").unwrap()));
    let TableGrowthTelemetryReadV1::Observed {
        samples,
        baseline_pending,
        ..
    } = read
    else {
        panic!("the second read must compare table watermarks");
    };
    assert!(
        samples
            .iter()
            .all(|sample| sample.table.as_str() != "created_after_baseline"),
        "new table must not be compared against fabricated zero bytes"
    );
    let pending = baseline_pending
        .iter()
        .find(|pending| pending.table.as_str() == "created_after_baseline")
        .expect("new table is explicitly baseline-pending");
    assert!(pending.current_bytes.get() > 0);
}

#[test]
fn application_telemetry_port_reports_denied_table_growth_without_zero() {
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
        .block_on(port.table_growth(&context, &StoreKeyV1::new("other.db").unwrap()));

    assert_eq!(
        read,
        TableGrowthTelemetryReadV1::Denied {
            store: StoreKeyV1::new("other.db").unwrap(),
        }
    );
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
fn dispatch_acquisition_grace_admits_after_transient_lease_release() {
    let store = TestStore::new();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let read = request(&store.binding, OperationPriorityV1::Foreground);
    let occupancy_probe = Probe::for_request(&read);
    let _first = pool
        .acquire(&read, &occupancy_probe, Duration::ZERO)
        .unwrap();
    let second = pool
        .acquire(&read, &occupancy_probe, Duration::ZERO)
        .unwrap();
    let dispatch_probe = SecondPollProbe::releasing(&read, second);

    let _replacement = pool
        .acquire_for_dispatch(&read, &dispatch_probe)
        .expect("one dispatch grace quantum should absorb a lease handoff");

    assert_eq!(pool.snapshot().leased_general, 2);
}

#[test]
fn dispatch_acquisition_grace_observes_cancellation_while_waiting() {
    let store = TestStore::new();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let read = request(&store.binding, OperationPriorityV1::Foreground);
    let occupancy_probe = Probe::for_request(&read);
    let _first = pool
        .acquire(&read, &occupancy_probe, Duration::ZERO)
        .unwrap();
    let _second = pool
        .acquire(&read, &occupancy_probe, Duration::ZERO)
        .unwrap();
    let dispatch_probe = SecondPollProbe::cancelling(&read);
    let before = pool.snapshot();

    assert!(matches!(
        pool.acquire_for_dispatch(&read, &dispatch_probe),
        Err(ReaderAcquireError::Interrupted {
            reason: tracedecay_store::UnavailableReasonV1::Cancelled
        })
    ));
    assert_eq!(pool.snapshot(), before);
}

#[test]
fn dispatch_acquisition_grace_keeps_true_saturation_bounded() {
    let store = TestStore::new();
    let pool = ReaderPool::start(store.locator(), two_reader_budget(), CountExecutor).unwrap();
    let read = request(&store.binding, OperationPriorityV1::Foreground);
    let probe = Probe::for_request(&read);
    let _first = pool.acquire(&read, &probe, Duration::ZERO).unwrap();
    let _second = pool.acquire(&read, &probe, Duration::ZERO).unwrap();

    let started = Instant::now();
    assert!(matches!(
        pool.acquire_for_dispatch(&read, &probe),
        Err(ReaderAcquireError::Saturated {
            scope: tracedecay_store::SaturationScopeV1::ReaderPool
        })
    ));
    let elapsed = started.elapsed();
    assert!(elapsed >= pool::ACQUISITION_POLL_QUANTUM);
    assert!(elapsed < Duration::from_secs(1));
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

#[test]
fn acquire_drives_burst_worker_retirement_on_entry() {
    // `acquire_lane` no longer retires on every bounded poll tick, only on entry
    // and after a notified wake. This guards that the entry retirement still
    // fires: a burst worker aged past the idle window must be shed when the next
    // acquisition walks the pool, shrinking back toward the floor.
    let store = TestStore::new();
    let mut budget = two_reader_budget();
    budget.max_per_hot_shard = 3;
    budget.idle_burst_retire_ms = 1;
    let pool = ReaderPool::start(store.locator(), budget, CountExecutor).unwrap();
    let read = request(&store.binding, OperationPriorityV1::Foreground);
    let probe = Probe::for_request(&read);

    let leases = (0..3)
        .map(|_| pool.acquire(&read, &probe, Duration::ZERO).unwrap())
        .collect::<Vec<_>>();
    drop(leases);
    assert_eq!(pool.snapshot().general_workers, 3);
    // Let the returned burst worker age past the 1ms idle window.
    std::thread::sleep(Duration::from_millis(10));

    // A fresh acquisition retires the aged burst worker on entry, then leases a
    // survivor. The floor (min_per_hot_shard = 2) is never breached.
    let lease = pool.acquire(&read, &probe, Duration::ZERO).unwrap();
    let state = pool.snapshot();
    assert_eq!(state.general_workers, 2);
    assert_eq!(state.leased_general, 1);
    drop(lease);
}
