use super::test_runtime::{publish_fixture_owner_runtime, test_code_shard};
use super::{
    Arc, Database, DatabaseAuthority, DatabaseOwnerErrorV1, TestDatabaseRuntimeMode,
    TestDatabaseRuntimeScope, adaptive_cache_sizes, platform_safe_mmap_size,
};
use tracedecay_store::ProjectId;

const KB: u64 = 1024;
const MB: u64 = 1024 * 1024;

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
    assert!(first.inner.writable);
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
        .engine_conn()
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
        .engine_conn()
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
        .engine_conn()
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
        .engine_conn()
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
    let raw = db.engine_conn();
    let guard = Arc::new(db.clone());
    drop(db);
    drop(authority);

    assert!(matches!(
        crate::db::probe_writer_owner(&path).unwrap(),
        crate::db::WriterOwnership::Active(_)
    ));
    raw.query("SELECT 1", ()).await.unwrap();

    drop(guard);
    assert_eq!(
        crate::db::probe_writer_owner(&path).unwrap(),
        crate::db::WriterOwnership::Idle
    );
    drop(raw);
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
    assert!(
        writer
            .engine_conn()
            .execute("CREATE TABLE forbidden_retained_write (id INTEGER)", ())
            .await
            .is_err()
    );
    assert!(
        reader
            .engine_conn()
            .execute("CREATE TABLE forbidden_reader_write (id INTEGER)", ())
            .await
            .is_err()
    );
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
            crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(blockers),
        ) => {
            assert!(blockers.iter().any(|blocker| matches!(
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
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(blockers) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("a cloned database issuance must remain one external client blocker");
    };
    assert!(blockers.iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 1,
            ..
        }
    )));
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
    let engine = database.engine_conn();
    let snapshot = database
        .begin_engine_read_snapshot("derived snapshot blocker")
        .await
        .unwrap();
    let telemetry = database.storage_telemetry_handle().unwrap();
    drop(database);

    let target = owner
        .reserve_retirement()
        .unwrap()
        .into_store_retirement_target()
        .unwrap();
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(blockers) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("a derived database handle must retain the exact client token");
    };
    assert!(blockers.iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 1,
            ..
        }
    )));

    drop(engine);
    drop(snapshot);
    drop(telemetry);
    assert!(owner.issue_lease().is_ok());
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
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(blockers) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("graph publication storage must retain its issuing client token");
    };
    assert!(blockers.iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 1,
            ..
        }
    )));

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
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(blockers) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("semantic staging must retain its issuing client token");
    };
    assert!(blockers.iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 1,
            ..
        }
    )));

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
    let crate::store_runtime::registry::StoreRuntimeRetirementResult::Blocked(blockers) =
        control.registry().reserve_retirement_batch(vec![target])
    else {
        panic!("external client and operation must block the exact owner reservation");
    };
    assert!(blockers.iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::ClientLeases {
            count: 1,
            ..
        }
    )));
    assert!(blockers.iter().any(|blocker| matches!(
        blocker,
        crate::store_runtime::registry::StoreRuntimeRetirementBlocker::OperationLeases {
            count: 1,
            ..
        }
    )));

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
    assert!(matches!(
        commit.outcomes(),
        [crate::store_runtime::registry::StoreRuntimeRetirementOutcome::Closed { .. }]
    ));
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
