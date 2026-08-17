use std::sync::atomic::AtomicBool;

use super::test_runtime::test_code_shard;
use super::{
    Arc, Database, DatabaseAuthority, DatabaseOwnerErrorV1, DatabaseOwnerWeakLeaseIssuerErrorV1,
    TestDatabaseRuntimeMode, TestDatabaseRuntimeScope, adaptive_cache_sizes,
    platform_safe_mmap_size,
};
use crate::db::DatabaseOwnerV1;
use crate::store_runtime::VerifiedGraphRuntimePortV1;
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
    VerifiedGraphSnapshot,
};
use tracedecay_store::{
    FactReadControl, ProjectId, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestProbeV1,
    StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

const KB: u64 = 1024;
const MB: u64 = 1024 * 1024;

struct CancelledSnapshotProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl RuntimeRequestProbeV1 for CancelledSnapshotProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        Some(RuntimeInterruptionV1::Cancelled)
    }

    fn try_begin_commit(&self) -> bool {
        false
    }
}

fn cancelled_snapshot_probe() -> Arc<dyn RuntimeRequestProbeV1> {
    Arc::new(CancelledSnapshotProbe {
        cancellation: RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new("cancellation.snapshot-test").unwrap(),
            generation: 1,
        },
        deadline: RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new("deadline.snapshot-test").unwrap(),
        },
    })
}

macro_rules! assert_retained_purpose_adapter_blocks_one_client {
    ($owner:expr, $control:expr, $database:ident, $adapter:ident) => {{
        let adapter_clone = $adapter.clone();
        drop($database);

        let target = $owner
            .reserve_retirement()
            .unwrap()
            .into_store_retirement_target()
            .unwrap();
        let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(refusal) =
            $control.registry().reserve_retirement_batch(vec![target])
        else {
            panic!("a retained purpose adapter must block database owner retirement");
        };
        assert!(refusal.blockers().iter().any(|blocker| matches!(
            blocker,
            crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
                count: 1,
                ..
            }
        )));
        drop(refusal);

        drop(adapter_clone);
        drop($adapter);
        assert!($owner.issue_lease().is_ok());
    }};
}

async fn install_workflow_schema_for_purpose_test(database: &Database) {
    for table in tracedecay_rusqlite_runtime::workflow::WORKFLOW_TABLE_CONTRACTS_V1 {
        database
            .execute_write_batch("install workflow purpose test schema", table.sql)
            .await
            .unwrap();
    }
    database
        .execute_write_batch(
            "stamp workflow purpose test schema",
            tracedecay_rusqlite_runtime::workflow::WORKFLOW_SCHEMA_IDENTITY_V1,
        )
        .await
        .unwrap();
}

struct PurposeTestRemoteKeyring(Arc<tracedecay_rusqlite_runtime::remote::RemoteSpoolKeyV1>);

impl tracedecay_rusqlite_runtime::remote::RemoteSpoolKeyringV1 for PurposeTestRemoteKeyring {
    fn active_key(
        &self,
    ) -> std::result::Result<
        Arc<tracedecay_rusqlite_runtime::remote::RemoteSpoolKeyV1>,
        tracedecay_rusqlite_runtime::remote::RemoteSqliteStorageErrorV1,
    > {
        Ok(Arc::clone(&self.0))
    }

    fn key(
        &self,
        revision: u64,
    ) -> std::result::Result<
        Option<Arc<tracedecay_rusqlite_runtime::remote::RemoteSpoolKeyV1>>,
        tracedecay_rusqlite_runtime::remote::RemoteSqliteStorageErrorV1,
    > {
        Ok((revision == self.0.revision()).then(|| Arc::clone(&self.0)))
    }
}

fn purpose_test_remote_keyring()
-> Arc<dyn tracedecay_rusqlite_runtime::remote::RemoteSpoolKeyringV1> {
    Arc::new(PurposeTestRemoteKeyring(Arc::new(
        tracedecay_rusqlite_runtime::remote::RemoteSpoolKeyV1::from_secret_bytes(1, vec![1; 32])
            .unwrap(),
    )))
}

async fn publish_fixture_owner_runtime(
    db_path: &std::path::Path,
    authority: &DatabaseAuthority,
    mode: TestDatabaseRuntimeMode,
    target_shard: tracedecay_store::StoreShardIdV1,
) -> crate::errors::Result<DatabaseOwnerV1> {
    Ok(
        Database::publish_fixture_runtime_publication(db_path, authority, mode, target_shard)
            .await?
            .owner,
    )
}

struct OwnerAccessTestGraphRuntime {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
}

impl VerifiedGraphRuntimePortV1 for OwnerAccessTestGraphRuntime {
    fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }

    fn cancel_reconciliation(&self) {}

    fn publish_verified_manifest(
        &self,
        _manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
        _cancelled: Arc<AtomicBool>,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        Err(GraphDbError::unavailable(
            "owner-access test graph has no publication",
        ))
    }

    fn reconcile_verified_manifest(
        &self,
        _manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        Err(GraphDbError::unavailable(
            "owner-access test graph has no reconciliation",
        ))
    }

    fn verified_snapshot(
        &self,
        _projection: &GraphProjectionIdentity,
        _read_control: FactReadControl,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        Ok(None)
    }
}

#[test]
fn adaptive_new_db_gets_minimum() {
    let (cache_kb, mmap) = adaptive_cache_sizes(0);
    assert_eq!(cache_kb, 2 * MB / KB); // 2 MB in KiB = 2048
    assert_eq!(mmap, 0);
}

#[test]
fn adaptive_small_db() {
    // 5 MB DB → cache = 2 MB (floor), mmap = 10 MB
    let (cache_kb, mmap) = adaptive_cache_sizes(5 * MB);
    assert_eq!(cache_kb, 2 * MB / KB);
    assert_eq!(mmap, 10 * MB);
}

#[test]
fn adaptive_medium_db() {
    // 100 MB DB → cache = 25 MB, mmap = 200 MB
    let (cache_kb, mmap) = adaptive_cache_sizes(100 * MB);
    assert_eq!(cache_kb, 25 * MB / KB);
    assert_eq!(mmap, 200 * MB);
}

#[test]
fn adaptive_large_db() {
    // 500 MB DB → cache = 64 MB (cap), mmap = 256 MB (cap)
    let (cache_kb, mmap) = adaptive_cache_sizes(500 * MB);
    assert_eq!(cache_kb, 64 * MB / KB);
    assert_eq!(mmap, 256 * MB);
}

#[test]
fn adaptive_very_large_db() {
    // 2 GB DB → both capped at max
    let (cache_kb, mmap) = adaptive_cache_sizes(2 * 1024 * MB);
    assert_eq!(cache_kb, 64 * MB / KB);
    assert_eq!(mmap, 256 * MB);
}

#[test]
fn mmap_disabled_for_every_graph_database() {
    let raw = 200 * MB;
    let effective = platform_safe_mmap_size(raw);
    assert_eq!(effective, 0);
    assert_eq!(platform_safe_mmap_size(0), 0);
}

#[tokio::test]
async fn database_owner_issues_client_leases_over_one_stable_database_inner() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "connection reuse").unwrap();
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let first = owner.issue_lease().unwrap();
    let second = owner.issue_lease().unwrap();
    let mut readers = Vec::new();
    for _ in 0..12 {
        readers.push(owner.issue_lease().unwrap());
    }

    assert!(Arc::ptr_eq(&first.inner, &second.inner));
    assert!(
        readers
            .iter()
            .all(|reader| Arc::ptr_eq(&first.inner, &reader.inner))
    );
    assert!(first.is_writable());
}

#[tokio::test]
async fn repeated_authorized_opens_share_one_writer_lane() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "writer reuse").unwrap();
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let first = owner.issue_lease().unwrap();
    let second = owner.issue_lease().unwrap();

    let first_writer = first.writer().await;
    assert!(second.inner.writer.try_lock().is_err());
    drop(first_writer);
    assert!(second.inner.writer.try_lock().is_ok());
}

#[tokio::test]
async fn database_owner_leases_preserve_registered_identity() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "preflight reuse").unwrap();
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let first = owner.issue_lease().unwrap();
    let second = owner.issue_lease().unwrap();

    assert_eq!(first.registered_binding(), second.registered_binding());
    assert_eq!(
        first.registered_verified_locator(),
        second.registered_verified_locator()
    );
    assert_eq!(first.opened_file_identity(), second.opened_file_identity());
    assert!(Arc::ptr_eq(&first.inner, &second.inner));
}

#[tokio::test]
async fn retained_daemon_database_refuses_writes_after_scope_drops() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("projects/project/tracedecay.db");
    let scope = crate::db::enter_daemon_database_scope(temp.path(), 9, "writer-scope").unwrap();
    let authority = DatabaseAuthority::acquire_daemon(&path, "writer-scope").unwrap();
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let database = owner.issue_lease().unwrap();
    let retained = database.clone();
    drop(scope);

    match retained
        .begin_write_transaction("write after scope drop")
        .await
    {
        Ok(_) => panic!("retained database began a write after its scope dropped"),
        Err(error) => assert!(error.to_string().contains("active daemon")),
    }
    match retained
        .execute_authority_revalidated_batch(
            "authority-revalidated write after scope drop",
            "CREATE TABLE revoked_authority_batch (id INTEGER NOT NULL)",
        )
        .await
    {
        Ok(()) => panic!("retained database wrote after its authority scope dropped"),
        Err(error) => assert!(error.to_string().contains("active daemon")),
    }
    assert_eq!(
        retained
            .query_scalar_i64(
                "verify revoked authority batch",
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'revoked_authority_batch'",
            )
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn authority_revalidated_batch_uses_the_canonical_long_lease_writer() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("authority-revalidated.db");
    let authority = DatabaseAuthority::acquire_test(&path, "authority-revalidated batch").unwrap();
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let database = owner.issue_lease().unwrap();

    database
        .execute_authority_revalidated_batch(
            "install authority-revalidated test table",
            "CREATE TABLE authority_revalidated_batch (value INTEGER NOT NULL);
             INSERT INTO authority_revalidated_batch(value) VALUES (7);",
        )
        .await
        .unwrap();
    assert_eq!(
        database
            .query_scalar_i64(
                "read authority-revalidated batch",
                "SELECT value FROM authority_revalidated_batch",
            )
            .await
            .unwrap(),
        7
    );
}

#[tokio::test]
async fn authority_revalidated_batch_rolls_back_when_the_batch_fails() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("authority-revalidated-rollback.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "authority-revalidated rollback").unwrap();
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let database = owner.issue_lease().unwrap();

    assert!(
        database
            .execute_authority_revalidated_batch(
                "fail authority-revalidated test batch",
                "CREATE TABLE authority_revalidated_rollback (value INTEGER NOT NULL);
                 this is not valid SQL;",
            )
            .await
            .is_err()
    );
    assert_eq!(
        database
            .query_scalar_i64(
                "verify authority-revalidated rollback",
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'authority_revalidated_rollback'",
            )
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn twelve_handles_serialize_isolated_writer_connections() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "twelve writers").unwrap();
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let first = owner.issue_lease().unwrap();
    first
        .writer_connection("create writer counter")
        .await
        .unwrap()
        .execute_batch(
            "CREATE TABLE writer_counter (value INTEGER NOT NULL);
                 INSERT INTO writer_counter(value) VALUES (0);",
        )
        .await
        .unwrap();

    let mut tasks = Vec::new();
    for _ in 0..12 {
        let handle = owner.issue_lease().unwrap();
        tasks.push(tokio::spawn(async move {
            handle
                .writer_connection("increment writer counter")
                .await
                .unwrap()
                .execute("UPDATE writer_counter SET value = value + 1", ())
                .await
                .unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }

    let mut rows = first
        .read_connection()
        .query("SELECT value FROM writer_counter", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        12
    );
}

#[tokio::test]
async fn retained_reader_never_observes_uncommitted_writer_state() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "paused writer read").unwrap();
    let (db, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    db.writer_connection("seed paused writer")
        .await
        .unwrap()
        .execute_batch(
            "CREATE TABLE paused_writer (value INTEGER NOT NULL);
                 INSERT INTO paused_writer(value) VALUES (0);",
        )
        .await
        .unwrap();

    let transaction = db.begin_write_transaction("pause writer").await.unwrap();
    transaction
        .execute("UPDATE paused_writer SET value = 1", ())
        .await
        .unwrap();

    let mut before = db
        .read_connection()
        .query("SELECT value FROM paused_writer", ())
        .await
        .unwrap();
    assert_eq!(
        before.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(before);
    transaction.commit().await.unwrap();

    let mut after = db
        .read_connection()
        .query("SELECT value FROM paused_writer", ())
        .await
        .unwrap();
    assert_eq!(
        after.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
}

#[tokio::test]
async fn memory_transactions_serialize_and_commit_through_the_final_writer() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "memory writer capability").unwrap();
    let (db, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let first = db
        .begin_memory_write_transaction("hold final memory writer")
        .await
        .unwrap();
    let mut second = Box::pin(db.begin_memory_write_transaction("commit final memory write"));

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut second)
            .await
            .is_err()
    );
    drop(first);
    let second = second.await.unwrap();
    second
        .execute(
            "INSERT INTO metadata(key, value) VALUES(?1, ?2)",
            crate::db::engine::params!["final-memory-writer", "committed"],
        )
        .await
        .unwrap();
    second.commit().await.unwrap();

    let mut rows = db
        .read_connection()
        .query(
            "SELECT value FROM metadata WHERE key = ?1",
            crate::db::engine::params!["final-memory-writer"],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "committed"
    );
}

#[tokio::test]
async fn cancelled_write_transaction_rolls_back_before_releasing_lane() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "cancelled writer").unwrap();
    let (db, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    db.writer_connection("seed cancelled writer")
        .await
        .unwrap()
        .execute_batch(
            "CREATE TABLE cancelled_writer (value INTEGER NOT NULL);
                 INSERT INTO cancelled_writer(value) VALUES (0);",
        )
        .await
        .unwrap();

    let task_db = db.clone();
    let (updated_tx, updated_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let transaction = task_db
            .begin_write_transaction("cancelled update")
            .await
            .unwrap();
        transaction
            .execute("UPDATE cancelled_writer SET value = 1", ())
            .await
            .unwrap();
        let _ = updated_tx.send(());
        std::future::pending::<()>().await;
        drop(transaction);
    });
    updated_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    let transaction = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        db.begin_write_transaction("writer after cancellation"),
    )
    .await
    .expect("writer lane remained locked after cancellation")
    .unwrap();
    let mut rows = transaction
        .query("SELECT value FROM cancelled_writer", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);
    transaction.commit().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn owner_leases_never_derive_a_second_database_from_a_symlink_alias() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "writer alias reuse").unwrap();
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let direct = owner.issue_lease().unwrap();
    let through_alias = owner.issue_lease().unwrap();

    assert!(Arc::ptr_eq(&direct.inner, &through_alias.inner));
}

#[cfg(windows)]
#[tokio::test]
async fn owner_leases_never_derive_a_second_database_from_a_case_alias() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("SlotCase");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("Graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "writer case reuse").unwrap();
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let direct = owner.issue_lease().unwrap();
    let through_alias = owner.issue_lease().unwrap();

    assert!(Arc::ptr_eq(&direct.inner, &through_alias.inner));
}

#[tokio::test]
async fn checkpoint_waits_for_shared_writer_lane() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "checkpoint writer lane").unwrap();
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let first = owner.issue_lease().unwrap();
    let second = owner.issue_lease().unwrap();
    let writer = first.writer().await;
    let mut checkpoint = tokio::spawn(async move { second.checkpoint().await });

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut checkpoint)
            .await
            .is_err()
    );
    drop(writer);
    checkpoint.await.unwrap().unwrap();
}

#[tokio::test]
async fn retained_database_guard_keeps_authority_alive_for_query_connection() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "dashboard guard").unwrap();
    let (db, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let raw = db.read_connection();
    let guard = Arc::new(db.clone());
    drop(db);
    drop(authority);

    assert!(matches!(
        crate::db::probe_writer_owner(&path).unwrap(),
        crate::db::WriterOwnership::Active(_)
    ));
    raw.query("SELECT 1", ()).await.unwrap();

    drop(raw);
    drop(guard);
    assert_eq!(
        crate::db::probe_writer_owner(&path).unwrap(),
        crate::db::WriterOwnership::Idle
    );
}

#[tokio::test]
async fn owner_leases_share_the_canonical_read_and_write_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "readonly upgrade").unwrap();
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let reader = owner.issue_lease().unwrap();
    let writer = owner.issue_lease().unwrap();
    assert!(Arc::ptr_eq(&reader.inner, &writer.inner));
    writer
        .writer_connection("reader isolation test")
        .await
        .unwrap()
        .execute("CREATE TABLE reader_did_not_poison_writer (id INTEGER)", ())
        .await
        .unwrap();
    let mut rows = reader
        .read_connection()
        .query("SELECT 1", ())
        .await
        .unwrap();
    assert!(rows.next().await.unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_issuance_and_retirement_fence_race_without_losing_the_exact_attachment() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "owner issuance race").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);
    let barrier = std::sync::Barrier::new(2);
    let (issued, target) = std::thread::scope(|scope| {
        let issuance = scope.spawn(|| {
            barrier.wait();
            owner.issue_lease()
        });
        barrier.wait();
        let target = owner
            .reserve_retirement()
            .unwrap()
            .into_store_retirement_target()
            .unwrap();
        (issuance.join().unwrap(), target)
    });

    match (
        issued,
        control.registry().reserve_retirement_batch(vec![target]),
    ) {
        (
            Ok(lease),
            crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(refusal),
        ) => {
            assert!(refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
                    count: 1,
                    ..
                }
            )));
            drop(lease);
        }
        (
            Err(DatabaseOwnerErrorV1::RetirementFenced),
            crate::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(reservation),
        ) => drop(reservation),
        _ => panic!(
            "issuance and fencing must linearize as one client blocker or one exact reservation"
        ),
    }
    assert!(owner.issue_lease().is_ok());
}

#[tokio::test]
async fn weak_owner_issuer_does_not_block_retirement_and_restores_ready_issuance() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "weak owner issuer retirement").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);
    let issuer = owner.weak_lease_issuer();
    let binding = owner.registered_binding().clone();
    let locator = owner.registered_verified_locator().clone();

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(reservation) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("a weak issuer must not retain a Store client or block retirement");
    };
    assert!(matches!(
        issuer.issue_lease(),
        Err(DatabaseOwnerWeakLeaseIssuerErrorV1::Retiring)
    ));
    drop(reservation);

    let restored = issuer.issue_lease().unwrap();
    assert_eq!(restored.registered_binding(), &binding);
    assert_eq!(restored.registered_verified_locator(), &locator);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn weak_owner_issuer_and_retirement_fence_race_as_client_or_reservation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "weak owner issuer race").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);
    let issuer = owner.weak_lease_issuer();
    let barrier = std::sync::Barrier::new(2);
    let (issued, target) = std::thread::scope(|scope| {
        let issuance = scope.spawn(|| {
            barrier.wait();
            issuer.issue_lease()
        });
        barrier.wait();
        let target = owner
            .reserve_retirement()
            .unwrap()
            .into_store_retirement_target()
            .unwrap();
        (issuance.join().unwrap(), target)
    });

    match (
        issued,
        control.registry().reserve_retirement_batch(vec![target]),
    ) {
        (
            Ok(lease),
            crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(refusal),
        ) => {
            assert!(refusal.blockers().iter().any(|blocker| matches!(
                blocker,
                crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
                    count: 1,
                    ..
                }
            )));
            drop(lease);
        }
        (
            Err(DatabaseOwnerWeakLeaseIssuerErrorV1::Retiring),
            crate::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(reservation),
        ) => drop(reservation),
        _ => panic!(
            "weak issuance and fencing must linearize as one client blocker or one exact reservation"
        ),
    }
    assert!(issuer.issue_lease().is_ok());
}

#[tokio::test]
async fn weak_owner_issuer_denies_terminal_and_dropped_owner() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "weak owner issuer terminal").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);
    let issuer = owner.weak_lease_issuer();
    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(mut reservation) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("a weak issuer must not block terminal retirement");
    };
    reservation.commit().unwrap();
    assert!(matches!(
        issuer.issue_lease(),
        Err(DatabaseOwnerWeakLeaseIssuerErrorV1::Terminal)
    ));

    let second_path = temp.path().join("dropped-owner.db");
    let second_authority =
        DatabaseAuthority::acquire_test(&second_path, "weak owner issuer dropped owner").unwrap();
    let second_fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &second_path,
        &second_authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::ProfileMemory,
    )
    .await
    .unwrap();
    let (second_owner, second_runtime, _second_control) = second_fixture.into_parts();
    let dropped_owner_issuer = second_owner.weak_lease_issuer();
    drop(second_owner);
    drop(second_runtime);
    assert!(matches!(
        dropped_owner_issuer.issue_lease(),
        Err(DatabaseOwnerWeakLeaseIssuerErrorV1::Unavailable)
    ));
}

#[tokio::test]
async fn owner_issued_database_clones_share_one_external_client_blocker() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "owner client clone token").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);
    let issued = owner.issue_lease().unwrap();
    let issued_clone = issued.clone();

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(refusal) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("a cloned database issuance must remain one external client blocker");
    };
    assert!(refusal.blockers().iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 1,
            ..
        }
    )));
    drop(refusal);
    drop(issued_clone);
    drop(issued);
    assert!(owner.issue_lease().is_ok());
}

#[tokio::test]
async fn derived_connection_snapshot_and_telemetry_retain_the_one_client_blocker() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "derived handle blocker").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);

    let database = owner.issue_lease().unwrap();
    let engine = database.read_connection();
    let background = engine.background();
    let _occupancy = background.reader_pool_occupancy();
    let snapshot = background.read_snapshot().await.unwrap();
    let telemetry = database.storage_telemetry_handle().unwrap();
    drop(database);

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(refusal) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("a derived database handle must retain the exact client token");
    };
    assert!(refusal.blockers().iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 1,
            ..
        }
    )));
    drop(refusal);

    drop(engine);
    drop(background);
    drop(snapshot);
    drop(telemetry);
    assert!(owner.issue_lease().is_ok());
}

#[tokio::test]
async fn runtime_client_clones_share_the_issuing_database_client_blocker() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "runtime client clone token").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, published, control) = fixture.into_parts();
    drop(published);

    let database = owner.issue_lease().unwrap();
    let runtime = database.runtime_client();
    assert_eq!(runtime.binding(), database.registered_binding());
    assert_eq!(
        &runtime.publication().binding,
        database.registered_binding()
    );
    assert_eq!(
        runtime.verified_locator(),
        database.registered_verified_locator()
    );
    assert_retained_purpose_adapter_blocks_one_client!(owner, control, database, runtime);
}

#[tokio::test]
async fn independently_issued_runtime_clients_are_separate_retirement_blockers() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "runtime client issuance tokens").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, published, control) = fixture.into_parts();
    drop(published);

    let first_database = owner.issue_lease().unwrap();
    let second_database = owner.issue_lease().unwrap();
    let first_runtime = first_database.runtime_client();
    let second_runtime = second_database.runtime_client();
    drop(first_database);
    drop(second_database);

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(refusal) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("independently issued runtime clients must block owner retirement");
    };
    assert!(refusal.blockers().iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 2,
            ..
        }
    )));
    drop(refusal);

    drop(first_runtime);
    drop(second_runtime);
    assert!(owner.issue_lease().is_ok());
}

#[tokio::test]
async fn work_storage_retains_one_issuing_client_until_all_clones_drop() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "Work storage guard").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);

    let database = owner.issue_lease().unwrap();
    let storage = database.work_storage().unwrap();
    assert_retained_purpose_adapter_blocks_one_client!(owner, control, database, storage);
}

#[tokio::test]
async fn workflow_storage_retains_one_issuing_client_until_all_clones_drop() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "workflow storage guard").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);

    let database = owner.issue_lease().unwrap();
    install_workflow_schema_for_purpose_test(&database).await;
    let storage = database.workflow_storage().unwrap();
    assert_retained_purpose_adapter_blocks_one_client!(owner, control, database, storage);
}

#[tokio::test]
async fn authorized_scope_set_storage_retains_one_issuing_client_until_all_clones_drop() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "scope-set storage guard").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);

    let database = owner.issue_lease().unwrap();
    let storage = database.authorized_scope_set_storage().unwrap();
    assert_retained_purpose_adapter_blocks_one_client!(owner, control, database, storage);
}

#[tokio::test]
async fn handoff_open_storage_validates_mounted_schema_and_retains_one_client() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "handoff storage guard").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);

    let database = owner.issue_lease().unwrap();
    database
        .execute_write_batch(
            "install handoff-open purpose test schema",
            tracedecay_rusqlite_runtime::handoff::HANDOFF_OPEN_SCHEMA_V1,
        )
        .await
        .unwrap();
    let mut rows = database
        .read_connection()
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            crate::db::engine::params!["handoff_open_grants_v1"],
        )
        .await
        .unwrap();
    assert!(rows.next().await.unwrap().is_some());
    let storage = database.handoff_open_storage().unwrap();
    assert_retained_purpose_adapter_blocks_one_client!(owner, control, database, storage);
}

#[tokio::test]
async fn remote_storage_factories_validate_and_retain_one_issuing_client() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("remote-node.db");
    let authority = DatabaseAuthority::acquire_test(&path, "Remote Brain storage guard").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::RemoteNode,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);

    let database = owner.issue_lease().unwrap();
    let provisioned = database
        .provision_remote_storage(purpose_test_remote_keyring())
        .unwrap();
    assert_eq!(provisioned.binding(), database.registered_binding());
    drop(provisioned);
    drop(database);

    let database = owner.issue_lease().unwrap();
    let storage = database
        .remote_storage(purpose_test_remote_keyring())
        .unwrap();
    assert_retained_purpose_adapter_blocks_one_client!(owner, control, database, storage);
}

#[tokio::test]
async fn graph_publication_storage_retains_the_issuing_client_token() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile-memory.db");
    let authority = DatabaseAuthority::acquire_test(&path, "graph publication guard").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::ProfileMemory,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);

    let database = owner.issue_lease().unwrap();
    let storage = database.graph_publication_storage().unwrap();
    drop(database);

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(refusal) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("graph publication storage must retain its issuing client token");
    };
    assert!(refusal.blockers().iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 1,
            ..
        }
    )));
    drop(refusal);

    drop(storage);
    assert!(owner.issue_lease().is_ok());
}

#[tokio::test]
async fn semantic_vector_staging_retains_the_issuing_client_token() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project.db");
    let authority = DatabaseAuthority::acquire_test(&path, "semantic staging guard").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Project {
            project_id: ProjectId::try_from("project.semantic-guard".to_owned()).unwrap(),
        },
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);

    let database = owner.issue_lease().unwrap();
    let storage = database.semantic_vector_publication_authority().unwrap();
    drop(database);

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(refusal) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("semantic staging must retain its issuing client token");
    };
    assert!(refusal.blockers().iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 1,
            ..
        }
    )));
    drop(refusal);

    drop(storage);
    assert!(owner.issue_lease().is_ok());
}

#[tokio::test]
async fn owner_reservation_restore_faults_when_the_exact_attachment_is_missing_or_stale() {
    let temp = tempfile::tempdir().unwrap();
    let authority =
        DatabaseAuthority::acquire_test(&temp.path().join("missing.db"), "missing attachment")
            .unwrap();
    let missing_owner = publish_fixture_owner_runtime(
        &temp.path().join("missing.db"),
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let missing = missing_owner.reserve_retirement().unwrap();
    missing.remove_attachment_for_test();
    drop(missing);
    assert!(matches!(
        missing_owner.issue_lease(),
        Err(DatabaseOwnerErrorV1::RetirementFaulted(
            crate::store_runtime::registry::StoreRuntimeRegistryFailure::DatabaseAttachmentReservationLost { .. }
        ))
    ));

    let stale_path = temp.path().join("stale.db");
    let stale_authority = DatabaseAuthority::acquire_test(&stale_path, "stale attachment").unwrap();
    let stale_owner = publish_fixture_owner_runtime(
        &stale_path,
        &stale_authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let stale = stale_owner.reserve_retirement().unwrap();
    stale.make_attachment_stale_for_test();
    drop(stale);
    assert!(matches!(
        stale_owner.issue_lease(),
        Err(DatabaseOwnerErrorV1::RetirementFaulted(
            crate::store_runtime::registry::StoreRuntimeRegistryFailure::DatabaseAttachmentReservationLost { .. }
        ))
    ));
}

#[tokio::test]
async fn read_write_owner_can_issue_independent_read_only_clients_without_escalation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "per-client access mode").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::ProfileMemory,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);

    let read_write = owner.issue_lease().unwrap();
    let read_only = owner.issue_read_only_lease().unwrap();
    let read_only_clone = read_only.clone();
    assert!(read_write.is_writable());
    assert!(!read_only.is_writable());
    assert!(!read_only_clone.is_writable());
    assert!(Arc::ptr_eq(&read_write.inner, &read_only.inner));
    assert!(read_only.write_authority().is_err());
    assert!(
        read_only
            .begin_write_transaction("read-only client cannot elevate")
            .await
            .is_err()
    );

    let graph_port: Arc<dyn VerifiedGraphRuntimePortV1> = Arc::new(OwnerAccessTestGraphRuntime {
        binding: read_write.registered_binding().clone(),
        locator: read_write.registered_verified_locator().clone(),
    });
    assert!(
        read_only
            .bind_memory_graph_runtime(Arc::clone(&graph_port))
            .is_err()
    );
    read_write
        .bind_memory_graph_runtime(graph_port)
        .expect("the read-write client retains graph-binding authority");

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(refusal) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("both independently issued clients must block owner retirement");
    };
    assert!(refusal.blockers().iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 2,
            ..
        }
    )));
}

#[tokio::test]
async fn read_only_owner_issuance_preserves_the_published_access_policy() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("readonly.db");
    let authority = DatabaseAuthority::acquire_test(&path, "readonly owner issuance").unwrap();
    let initialized = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    drop(initialized);
    let owner = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::ReadOnly,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();

    let database = owner.issue_lease().unwrap();
    assert!(!database.is_writable());
    assert!(database.write_authority().is_err());
    assert!(
        database
            .begin_write_transaction("reject read-only issued client")
            .await
            .is_err()
    );
    assert!(
        database
            .execute_authority_revalidated_batch(
                "reject read-only authority-revalidated batch",
                "CREATE TABLE forbidden_authority_revalidated_batch (id INTEGER NOT NULL)",
            )
            .await
            .is_err()
    );
    assert!(database.work_storage().is_err());
    assert!(database.workflow_storage().is_err());
    assert!(database.authorized_scope_set_storage().is_err());
    assert!(database.handoff_open_storage().is_err());
    assert!(
        database
            .remote_storage(purpose_test_remote_keyring())
            .is_err()
    );
    assert!(
        database
            .provision_remote_storage(purpose_test_remote_keyring())
            .is_err()
    );
    assert!(
        database
            .snapshot_to(&temp.path().join("readonly.snapshot"))
            .await
            .is_err()
    );
    assert!(
        database
            .snapshot_to_interruptible(
                &temp.path().join("readonly-interruptible.snapshot"),
                cancelled_snapshot_probe(),
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn read_write_database_snapshot_uses_the_canonical_writer_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("snapshot-source.db");
    let authority = DatabaseAuthority::acquire_test(&path, "database snapshot").unwrap();
    let (database, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    database
        .set_metadata("snapshot", "canonical")
        .await
        .unwrap();

    let destination = temp.path().join("snapshot-destination.db");
    let receipt = database
        .snapshot_to_interruptible(
            &destination,
            Arc::new(super::database_checkpoint_probe().unwrap()),
        )
        .await
        .unwrap();
    assert!(destination.is_file());
    assert!(receipt.destination_bytes > 0);

    let cancelled_destination = temp.path().join("snapshot-cancelled.db");
    assert!(
        database
            .snapshot_to_interruptible(&cancelled_destination, cancelled_snapshot_probe())
            .await
            .is_err()
    );
    assert!(!cancelled_destination.exists());
}

#[tokio::test]
async fn owner_reservation_rollback_preserves_database_inner_and_publication_identity() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "owner rollback identity").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);
    let first = owner.issue_lease().unwrap();
    let inner = Arc::as_ptr(&first.inner);
    let binding = first.registered_binding().clone();
    let locator = first.registered_verified_locator().clone();
    drop(first);

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(reservation) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("the exact owner attachment must be reservable without a count allowance");
    };
    drop(reservation);

    let restored = owner.issue_lease().unwrap();
    assert_eq!(Arc::as_ptr(&restored.inner), inner);
    assert_eq!(restored.registered_binding(), &binding);
    assert_eq!(restored.registered_verified_locator(), &locator);
}

#[tokio::test]
async fn paired_target_composition_refusal_preserves_exact_inputs_for_ready_restore_and_retry() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("readonly-paired-owner.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "paired target composition refusal").unwrap();
    let initialized = publish_fixture_owner_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    drop(initialized);
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::ReadOnly,
        TestDatabaseRuntimeScope::ProfileMemory,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);
    let key = crate::store_runtime::registry::StoreRuntimeKey::new(
        control.binding().shard_id.clone(),
        control.binding().incarnation,
    );
    let (graph_owner, graph_target) = control
        .registry()
        .attach_graph_store_owner(key)
        .await
        .unwrap();

    let refusal = match owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target_with_graph(graph_target)
    {
        Ok(_) => panic!("a read-only owner cannot compose a Store retirement target"),
        Err(refusal) => refusal,
    };
    assert!(matches!(
        refusal.error(),
        DatabaseOwnerErrorV1::MissingWriteAuthority
    ));
    let (_error, database_reservation, graph_target) = refusal.into_parts();
    assert!(matches!(
        owner.issue_lease(),
        Err(DatabaseOwnerErrorV1::RetirementFenced)
    ));
    drop(database_reservation);
    let restored = owner.issue_lease().unwrap();
    drop(restored);

    // The exact graph target survives the refusal. The test-only authority
    // lets the Store reservation exercise that same target without remounting
    // the graph; cancellation returns it through the normal paired handoff.
    let database_reservation = owner.reserve_retirement().unwrap();
    let retry_target =
        crate::store_runtime::registry::StoreRuntimeRetirementTarget::with_owner_attachments(
            owner.registered_binding().clone(),
            authority.clone(),
            Box::new(database_reservation),
            graph_target,
        );
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(mut reservation) =
        control
            .registry()
            .reserve_retirement_batch(vec![retry_target])
    else {
        panic!(
            "the graph target returned from composition refusal must reserve without remounting"
        );
    };
    let mut retry_targets = reservation.cancel().unwrap();
    assert_eq!(retry_targets.len(), 1);
    let graph_target = retry_targets
        .pop()
        .unwrap()
        .into_database_graph_owner_handoff()
        .unwrap()
        .cancel_to_ready_graph_target();
    let restored = owner.issue_lease().unwrap();
    drop(restored);
    drop(graph_target);
    drop(graph_owner);
}

#[tokio::test]
async fn paired_database_and_graph_owner_target_restores_the_exact_database_owner() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "paired owner bridge").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::ProfileMemory,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);
    let first = owner.issue_lease().unwrap();
    let inner = Arc::as_ptr(&first.inner);
    drop(first);

    let key = crate::store_runtime::registry::StoreRuntimeKey::new(
        control.binding().shard_id.clone(),
        control.binding().incarnation,
    );
    let (graph_owner, graph_target) = control
        .registry()
        .attach_graph_store_owner(key)
        .await
        .unwrap();
    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target_with_graph(graph_target)
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(mut reservation) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("paired owner target must reserve one exact Store entry");
    };
    assert!(matches!(
        owner.issue_lease(),
        Err(DatabaseOwnerErrorV1::RetirementFenced)
    ));

    reservation.cancel().unwrap();
    let restored = owner.issue_lease().unwrap();
    assert_eq!(Arc::as_ptr(&restored.inner), inner);
    drop(restored);
    drop(graph_owner);
}

#[tokio::test]
async fn paired_owner_target_handoff_restores_ready_and_retries_without_remounting() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "paired owner retry handoff").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::ProfileMemory,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);

    let issued = owner.issue_lease().unwrap();
    let inner = Arc::as_ptr(&issued.inner);
    drop(issued);
    let key = crate::store_runtime::registry::StoreRuntimeKey::new(
        control.binding().shard_id.clone(),
        control.binding().incarnation,
    );
    let (graph_owner, graph_target) = control
        .registry()
        .attach_graph_store_owner(key)
        .await
        .unwrap();
    let blocker = owner.issue_lease().unwrap();
    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target_with_graph(graph_target)
        .unwrap();

    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(refusal) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("a live client must block the exact paired owner target");
    };
    assert!(refusal.blockers().iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 1,
            ..
        }
    )));
    let (_, mut retry_targets) = refusal.into_parts();
    assert_eq!(retry_targets.len(), 1);
    let graph_target = retry_targets
        .pop()
        .unwrap()
        .into_database_graph_owner_handoff()
        .unwrap()
        .cancel_to_ready_graph_target();
    let restored = owner.issue_lease().unwrap();
    assert_eq!(Arc::as_ptr(&restored.inner), inner);
    drop(restored);
    drop(blocker);

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target_with_graph(graph_target)
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(mut reservation) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("the restored paired owner target must reserve without remounting");
    };
    let mut retry_targets = reservation.cancel().unwrap();
    assert_eq!(retry_targets.len(), 1);
    let graph_target = retry_targets
        .pop()
        .unwrap()
        .into_database_graph_owner_handoff()
        .unwrap()
        .cancel_to_ready_graph_target();
    let restored = owner.issue_lease().unwrap();
    assert_eq!(Arc::as_ptr(&restored.inner), inner);
    drop(restored);

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target_with_graph(graph_target)
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(reservation) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("the exact graph target must remain reusable after cancellation");
    };
    drop(reservation);

    let restored = owner.issue_lease().unwrap();
    assert_eq!(Arc::as_ptr(&restored.inner), inner);
    drop(restored);
    drop(graph_owner);
}

#[tokio::test]
async fn external_client_clone_and_operation_remain_typed_owner_retirement_blockers() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("registered.db");
    let authority = DatabaseAuthority::acquire_test(&path, "owner external blockers").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Profile,
    )
    .await
    .unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);
    let external = match control.registry().lookup(control.binding()) {
        crate::store_runtime::registry::StoreRuntimeLookup::Ready(lease) => lease,
        other => panic!("expected the exact registered runtime before retirement: {other:?}"),
    };
    let external_clone = external.clone();
    let operation = external.begin_operation().unwrap();
    drop(external);

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(refusal) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("external client and operation must block the exact owner reservation");
    };
    assert!(refusal.blockers().iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 1,
            ..
        }
    )));
    assert!(refusal.blockers().iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::OperationLeases {
            count: 1,
            ..
        }
    )));
    drop(refusal);

    drop(operation);
    drop(external_clone);
    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Reserved(mut reservation) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("releasing external client tokens must permit exact retirement");
    };
    let commit = reservation.commit().unwrap();
    assert!(
        matches!(
            commit.outcomes(),
            [crate::store_runtime::registry::StoreRuntimeRetirementOutcome::Closed { .. }]
        ),
        "unexpected retirement outcomes: {:?}",
        commit.outcomes()
    );
    assert!(matches!(
        owner.issue_lease(),
        Err(DatabaseOwnerErrorV1::RetirementTerminal)
    ));

    let reopened = control.reopen().await.unwrap();
    assert_ne!(reopened.binding(), control.binding());
    assert_eq!(reopened.binding().shard_id, control.binding().shard_id);
    assert_eq!(
        reopened.binding().incarnation,
        control.binding().incarnation
    );
    assert!(reopened.binding().authority_epoch > control.binding().authority_epoch);
    assert_eq!(reopened.verified_locator(), control.locator());
}
