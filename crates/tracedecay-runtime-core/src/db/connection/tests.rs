use super::test_runtime::test_code_shard;
use super::{
    Arc, Database, DatabaseAccessMode, DatabaseAuthority, TestDatabaseRuntimeMode,
    adaptive_cache_sizes, platform_safe_mmap_size,
};

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
async fn repeated_authorized_opens_share_one_physical_connection() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "connection reuse").unwrap();
    let (first, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let (second, _) = Database::open(&path, &authority).await.unwrap();
    let mut readers = Vec::new();
    for _ in 0..12 {
        readers.push(Database::open_read_only(&path, &authority).await.unwrap().0);
    }

    assert!(Arc::ptr_eq(&first.inner, &second.inner));
    assert!(
        readers
            .iter()
            .all(|reader| !Arc::ptr_eq(&first.inner, &reader.inner))
    );
    assert!(readers.iter().all(|reader| !reader.inner.writable));
    assert!(first.inner.writable);
}

#[tokio::test]
async fn repeated_authorized_opens_share_one_writer_lane() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "writer reuse").unwrap();
    let (first, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let (second, _) = Database::open(&path, &authority).await.unwrap();

    let first_writer = first.writer().await;
    assert!(second.inner.writer.try_lock().is_err());
    drop(first_writer);
    assert!(second.inner.writer.try_lock().is_ok());
}

#[tokio::test]
async fn read_only_preflight_and_writable_mount_share_one_registered_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "preflight reuse").unwrap();
    let (writer, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .unwrap();
    let reader = Database::publish_runtime(
        writer.retained_runtime().clone(),
        DatabaseAccessMode::ReadOnly,
    )
    .await
    .unwrap();
    let remounted_writer = Database::publish_runtime(
        reader.retained_runtime().clone(),
        DatabaseAccessMode::ReadWrite,
    )
    .await
    .unwrap();

    assert!(Arc::ptr_eq(
        writer.retained_runtime().runtime(),
        reader.retained_runtime().runtime()
    ));
    assert_eq!(writer.opened_file_identity(), reader.opened_file_identity());
    assert!(Arc::ptr_eq(&writer.inner, &remounted_writer.inner));
}

#[tokio::test]
async fn retained_daemon_database_refuses_writes_after_scope_drops() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("projects/project/tracedecay.db");
    let scope = crate::db::enter_daemon_database_scope(temp.path(), 9, "writer-scope").unwrap();
    let authority = DatabaseAuthority::acquire_daemon(&path, "writer-scope").unwrap();
    let (database, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
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
    let (first, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
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
        let handle = Database::open(&path, &authority).await.unwrap().0;
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
        .conn()
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
        .conn()
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
        .conn()
        .query("SELECT value FROM paused_writer", ())
        .await
        .unwrap();
    assert_eq!(
        after.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
}

#[tokio::test]
async fn opaque_memory_writer_serializes_and_mutates_without_raw_connection_access() {
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
    let first = db.memory_writer().await.unwrap();
    let mut second = Box::pin(db.memory_writer());

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut second)
            .await
            .is_err()
    );
    drop(first);
    let second = second.await.unwrap();
    second
        .store()
        .add_fact(
            crate::memory::types::AddFactRequest {
                content: "opaque writer fixture".to_string(),
                category: crate::memory::types::MemoryCategory::General,
                source: Some("test".to_string()),
                tags: Vec::new(),
                entities: Vec::new(),
                trust: None,
                metadata: serde_json::json!({}),
            },
            crate::memory::trust::DEFAULT_TRUST,
        )
        .await
        .unwrap();
    drop(second);

    let mut rows = db
        .conn()
        .query("SELECT COUNT(*) FROM memory_facts", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
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
async fn writable_symlink_aliases_share_one_database_slot() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "writer alias reuse").unwrap();
    let (direct, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let alias = temp.path().join("graph-alias.db");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    let (through_alias, _) = Database::open(&alias, &authority).await.unwrap();

    assert!(Arc::ptr_eq(&direct.inner, &through_alias.inner));
}

#[cfg(windows)]
#[tokio::test]
async fn writable_case_aliases_share_one_database_slot() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("SlotCase");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("Graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "writer case reuse").unwrap();
    let (direct, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let alias = directory.join("graph.db");
    let (through_alias, _) = Database::open(&alias, &authority).await.unwrap();

    assert!(Arc::ptr_eq(&direct.inner, &through_alias.inner));
}

#[tokio::test]
async fn checkpoint_waits_for_shared_writer_lane() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "checkpoint writer lane").unwrap();
    let (first, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let (second, _) = Database::open(&path, &authority).await.unwrap();
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
    let raw = db.conn().clone();
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
async fn read_only_first_open_does_not_block_writable_owner() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("graph.db");
    let authority = DatabaseAuthority::acquire_test(&path, "readonly upgrade").unwrap();
    let (seed, _) = Database::publish_fixture_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        test_code_shard().unwrap(),
    )
    .await
    .unwrap();
    let runtime = seed.retained_runtime().clone();
    drop(seed);

    let reader = Database::publish_runtime(runtime.clone(), DatabaseAccessMode::ReadOnly)
        .await
        .unwrap();
    let writer = Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite)
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&reader.inner, &writer.inner));
    writer
        .writer_connection("reader isolation test")
        .await
        .unwrap()
        .execute("CREATE TABLE reader_did_not_poison_writer (id INTEGER)", ())
        .await
        .unwrap();
    assert!(
        writer
            .conn()
            .execute("CREATE TABLE forbidden_retained_write (id INTEGER)", ())
            .await
            .is_err()
    );
    assert!(
        reader
            .conn()
            .execute("CREATE TABLE forbidden_reader_write (id INTEGER)", ())
            .await
            .is_err()
    );
}
