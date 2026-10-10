use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use super::*;

fn execution_guard() -> ExecutionGuard {
    ExecutionGuard::new(
        Some(Instant::now() + Duration::from_secs(30)),
        Arc::new(AtomicBool::new(false)),
        None,
    )
}

fn database() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(&directory.path().join("native.db")).unwrap();
    (directory, database)
}

#[test]
fn native_reader_and_transaction_boundaries_are_enforced() {
    let (_directory, database) = database();
    let mut writer = database.connect(Access::Writer).unwrap();
    let guard = execution_guard();
    writer
        .execute(
            "CREATE TABLE entries(id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
            &[],
            &guard,
        )
        .unwrap();
    let mut reader = database.connect(Access::Reader).unwrap();
    for sql in [
        "INSERT INTO entries(body) VALUES('forbidden')",
        "EXPLAIN DELETE FROM entries",
        "PRAGMA query_only=OFF",
        "PRAGMA foreign_keys=OFF",
        "BEGIN",
        "ATTACH ':memory:' AS other",
    ] {
        assert!(
            matches!(reader.query(sql, &[], &guard), Err(Error::Denied(_))),
            "{sql}"
        );
    }
    for sql in [
        "COMMIT",
        "END",
        "SAVEPOINT injected",
        "ROLLBACK",
        "PRAGMA writable_schema=ON",
        "CREATE TEMP TRIGGER bad AFTER INSERT ON entries BEGIN DELETE FROM entries; END",
        "CREATE TEMP VIEW bad AS SELECT * FROM entries",
    ] {
        assert!(
            matches!(writer.execute_batch(sql, &guard), Err(Error::Denied(_))),
            "{sql}"
        );
    }
    assert!(matches!(
        reader.begin(TransactionBehavior::Immediate, &guard),
        Err(Error::Denied(_))
    ));
    assert!(matches!(reader.checkpoint(&guard), Err(Error::Denied(_))));
    assert!(matches!(
        reader.set_full_durability(&guard),
        Err(Error::Denied(_))
    ));
    writer.set_full_durability(&guard).unwrap();
    assert!(matches!(
        writer.execute("PRAGMA secure_delete=ON", &[], &guard),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        reader.query("PRAGMA secure_delete", &[], &guard),
        Err(Error::Unsupported(_))
    ));
    writer
        .begin(TransactionBehavior::Immediate, &guard)
        .unwrap();
    writer
        .execute(
            "INSERT INTO entries(body) VALUES(?)",
            &[Value::Text("first".to_owned())],
            &guard,
        )
        .unwrap();
    writer.savepoint("owned_1", &guard).unwrap();
    writer
        .execute(
            "INSERT INTO entries(body) VALUES('rolled back')",
            &[],
            &guard,
        )
        .unwrap();
    writer.rollback_savepoint("owned_1").unwrap();
    writer.release_savepoint("owned_1", &guard).unwrap();
    writer.commit(&guard).unwrap();
    assert_eq!(
        reader
            .query("SELECT body FROM entries", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Text("first".to_owned())]]
    );
}

#[test]
fn native_stored_objects_and_nested_expressions_cannot_load_extensions() {
    let (_directory, database) = database();
    let mut writer = database.connect(Access::Writer).unwrap();
    let guard = execution_guard();
    writer
        .execute(
            "CREATE TABLE entries(id INTEGER PRIMARY KEY, body TEXT)",
            &[],
            &guard,
        )
        .unwrap();
    for sql in [
        "SELECT load_extension('missing')",
        "SELECT CASE WHEN 0 THEN load_extension('missing') ELSE 'safe' END",
        "SELECT \"load_extension\"('missing')",
    ] {
        assert!(writer.query(sql, &[], &guard).is_err(), "{sql}");
    }
    // A view can defer expression resolution until read. The policy is installed
    // on the native dialect, so the later compiled access still rejects loading.
    let created = writer.execute(
        "CREATE VIEW dangerous AS SELECT load_extension('missing') AS result",
        &[],
        &guard,
    );
    if created.is_ok() {
        assert!(
            writer
                .query("SELECT * FROM dangerous", &[], &guard)
                .is_err()
        );
    }
    let trigger = writer.execute("CREATE TRIGGER dangerous_insert AFTER INSERT ON entries BEGIN SELECT load_extension('missing'); END", &[], &guard);
    if trigger.is_ok() {
        assert!(
            writer
                .execute("INSERT INTO entries(body) VALUES('safe')", &[], &guard)
                .is_err()
        );
        assert_eq!(
            writer
                .query("SELECT count(*) FROM entries", &[], &guard)
                .unwrap()
                .values,
            vec![vec![Value::Integer(0)]]
        );
        writer
            .execute("DROP TRIGGER main.dangerous_insert", &[], &guard)
            .unwrap();
        writer
            .execute("INSERT INTO entries(body) VALUES('safe')", &[], &guard)
            .unwrap();
    }
}

#[test]
fn cancellation_and_revocation_interrupt_native_vm_and_preserve_rollback() {
    let (_directory, database) = database();
    let mut writer = database.connect(Access::Writer).unwrap();
    let guard = execution_guard();
    writer
        .execute(
            "CREATE TABLE entries(id INTEGER PRIMARY KEY, body TEXT)",
            &[],
            &guard,
        )
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let authority_calls = Arc::clone(&calls);
    let revoked = ExecutionGuard::new(
        Some(Instant::now() + Duration::from_secs(30)),
        Arc::new(AtomicBool::new(false)),
        Some(Arc::new(move || {
            if authority_calls.fetch_add(1, Ordering::SeqCst) >= 4 {
                Err("revoked".to_owned())
            } else {
                Ok(())
            }
        })),
    );
    let result = writer.query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000000) SELECT sum(x) FROM n", &[], &revoked);
    assert!(matches!(result, Err(Error::Authority(message)) if message == "revoked"));
    assert!(calls.load(Ordering::SeqCst) >= 5);
    writer
        .begin(TransactionBehavior::Immediate, &guard)
        .unwrap();
    writer
        .execute("INSERT INTO entries(body) VALUES('rollback')", &[], &guard)
        .unwrap();
    let cancelled = guard.clone();
    cancelled.cancelled.store(true, Ordering::Release);
    assert!(matches!(writer.commit(&cancelled), Err(Error::Cancelled)));
    writer.rollback().unwrap();
    assert_eq!(
        writer
            .query("SELECT count(*) FROM entries", &[], &execution_guard())
            .unwrap()
            .values,
        vec![vec![Value::Integer(0)]]
    );
    let expired = ExecutionGuard::new(Some(Instant::now()), Arc::new(AtomicBool::new(false)), None);
    assert!(matches!(
        writer.query("SELECT 1", &[], &expired),
        Err(Error::DeadlineExceeded)
    ));
}

#[test]
fn whole_batch_is_authorized_before_first_write_and_results_are_bounded() {
    let (_directory, database) = database();
    let mut writer = database.connect(Access::Writer).unwrap();
    let guard = execution_guard();
    writer
        .execute(
            "CREATE TABLE entries(id INTEGER PRIMARY KEY, body TEXT)",
            &[],
            &guard,
        )
        .unwrap();
    assert!(matches!(
        writer.execute_batch(
            "INSERT INTO entries(body) VALUES('must not apply'); COMMIT",
            &guard
        ),
        Err(Error::Denied(_))
    ));
    assert_eq!(
        writer
            .query("SELECT count(*) FROM entries", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Integer(0)]]
    );
    assert!(matches!(writer.query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10001) SELECT x FROM n", &[], &guard), Err(Error::QueryLimitExceeded)));
    assert_eq!(
        writer.query("SELECT 1", &[], &guard).unwrap().values,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn drop_rolls_back_and_reopen_retains_only_committed_values() {
    let (directory, database) = database();
    let guard = execution_guard();
    {
        let mut writer = database.connect(Access::Writer).unwrap();
        writer
            .execute(
                "CREATE TABLE entries(id INTEGER PRIMARY KEY, body TEXT)",
                &[],
                &guard,
            )
            .unwrap();
        writer
            .execute("INSERT INTO entries(body) VALUES('committed')", &[], &guard)
            .unwrap();
        writer
            .begin(TransactionBehavior::Immediate, &guard)
            .unwrap();
        writer
            .execute("INSERT INTO entries(body) VALUES('dropped')", &[], &guard)
            .unwrap();
    }
    drop(database);
    let reopened = Database::open(&directory.path().join("native.db")).unwrap();
    let mut reader = reopened.connect(Access::Reader).unwrap();
    assert_eq!(
        reader
            .query("SELECT body FROM entries", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Text("committed".to_owned())]]
    );
    assert_eq!(
        reader
            .query("PRAGMA integrity_check", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Text("ok".to_owned())]]
    );
}

#[test]
fn foreign_keys_and_constraint_refusals_remain_typed() {
    let (_directory, database) = database();
    let mut writer = database.connect(Access::Writer).unwrap();
    let guard = execution_guard();
    writer.execute_batch("CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parents(id))", &guard).unwrap();
    let error = writer
        .execute(
            "INSERT INTO children(id,parent_id) VALUES(1,9)",
            &[],
            &guard,
        )
        .unwrap_err();
    assert!(error.is_deterministic_refusal());
    assert!(!error.is_transient_read_failure());
    assert!(!error.requires_transaction_retry());
    writer
        .execute("INSERT INTO parents(id) VALUES(9)", &[], &guard)
        .unwrap();
    writer
        .execute(
            "INSERT INTO children(id,parent_id) VALUES(1,9)",
            &[],
            &guard,
        )
        .unwrap();
    let error = writer
        .execute(
            "INSERT INTO children(id,parent_id) VALUES(1,9)",
            &[],
            &guard,
        )
        .unwrap_err();
    assert!(error.is_deterministic_refusal());
    assert!(
        writer
            .query("PRAGMA foreign_key_check", &[], &guard)
            .unwrap()
            .values
            .is_empty()
    );
    assert!(Error::Engine(turso_core::LimboError::WriteWriteConflict).requires_transaction_retry());
    assert!(!Error::Engine(turso_core::LimboError::WriteWriteConflict).is_deterministic_refusal());
}

#[cfg(unix)]
#[test]
fn native_pinned_file_identity_rejects_replacement_and_unlink_recreate() {
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("held.db");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let metadata = file.metadata().unwrap();
    let database = Database::open_pinned(&path, file).unwrap();
    assert_eq!(
        database.opened_file_identity(),
        Some((metadata.dev(), metadata.ino()))
    );
    let guard = execution_guard();
    let mut writer = database.connect(Access::Writer).unwrap();
    writer
        .execute(
            "CREATE TABLE entries(id INTEGER PRIMARY KEY, body TEXT)",
            &[],
            &guard,
        )
        .unwrap();
    writer
        .execute("INSERT INTO entries(body) VALUES('original')", &[], &guard)
        .unwrap();
    // The temporary renamed file exists solely while exercising replacement,
    // and TempDir removes the entire isolated fixture at the end of this test.
    let staged = directory.path().join("renamed.db");
    std::fs::rename(&path, &staged).unwrap();
    std::fs::write(&path, b"replacement must never become the held database").unwrap();
    assert!(matches!(
        database.connect(Access::Reader),
        Err(Error::Denied(_))
    ));
    assert!(matches!(
        writer.query("SELECT * FROM entries", &[], &guard),
        Err(Error::Denied(_))
    ));
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&staged, &path).unwrap();
    assert_eq!(
        writer
            .query("SELECT body FROM entries", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Text("original".to_owned())]]
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"new inode").unwrap();
    assert!(matches!(
        writer.execute(
            "INSERT INTO entries(body) VALUES('must not apply')",
            &[],
            &guard
        ),
        Err(Error::Denied(_))
    ));
}

#[cfg(unix)]
#[test]
fn pathname_only_registry_entry_cannot_satisfy_descriptor_bound_attachment() {
    let (directory, pathname_database) = database();
    let path = directory.path().join("native.db");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    assert!(matches!(
        Database::open_pinned(&path, file),
        Err(Error::Engine(turso_core::LimboError::InvalidArgument(_)))
    ));
    drop(pathname_database);
}

#[test]
fn opt_in_native_mvcc_overlaps_same_shard_writers_and_keeps_read_snapshots() {
    let (_directory, database) = database();
    let guard = execution_guard();
    let mut left = database.connect(Access::Writer).unwrap();
    let mut reader = database.connect(Access::Reader).unwrap();
    assert!(matches!(
        reader.enable_concurrent_transactions(&guard),
        Err(Error::Denied(_))
    ));
    assert!(matches!(
        left.execute("PRAGMA journal_mode=mvcc", &[], &guard),
        Err(Error::Denied(_))
    ));
    left.enable_concurrent_transactions(&guard).unwrap();
    left.execute(
        "CREATE TABLE overlap(id INTEGER PRIMARY KEY, body TEXT)",
        &[],
        &guard,
    )
    .unwrap();
    let mut right = database.connect(Access::Writer).unwrap();
    reader.begin(TransactionBehavior::Deferred, &guard).unwrap();
    assert_eq!(
        reader
            .query("SELECT count(*) FROM overlap", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Integer(0)]]
    );
    // Both database transactions begin before either write, on the same file.
    left.begin(TransactionBehavior::Concurrent, &guard).unwrap();
    right
        .begin(TransactionBehavior::Concurrent, &guard)
        .unwrap();
    assert_eq!(
        left.query(
            "INSERT INTO overlap VALUES(11, 'left') RETURNING id",
            &[],
            &guard
        )
        .unwrap()
        .values,
        vec![vec![Value::Integer(11)]]
    );
    assert_eq!(
        right
            .query(
                "INSERT INTO overlap VALUES(22, 'right') RETURNING id",
                &[],
                &guard
            )
            .unwrap()
            .values,
        vec![vec![Value::Integer(22)]]
    );
    left.commit(&guard).unwrap();
    right.commit(&guard).unwrap();
    assert_eq!(
        reader
            .query("SELECT count(*) FROM overlap", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Integer(0)]]
    );
    reader.commit(&guard).unwrap();
    assert_eq!(
        reader
            .query("SELECT id FROM overlap ORDER BY id", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Integer(11)], vec![Value::Integer(22)]]
    );
}

#[test]
fn opt_in_native_mvcc_conflict_rolls_back_and_retries_in_a_fresh_transaction() {
    let (directory, database) = database();
    let guard = execution_guard();
    let mut left = database.connect(Access::Writer).unwrap();
    left.enable_concurrent_transactions(&guard).unwrap();
    left.execute_batch("CREATE TABLE checkpoint(id INTEGER PRIMARY KEY, sequence INTEGER NOT NULL); INSERT INTO checkpoint VALUES(1, 0)", &guard).unwrap();
    let mut right = database.connect(Access::Writer).unwrap();
    left.begin(TransactionBehavior::Concurrent, &guard).unwrap();
    right
        .begin(TransactionBehavior::Concurrent, &guard)
        .unwrap();
    assert_eq!(
        left.query("SELECT sequence FROM checkpoint", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Integer(0)]]
    );
    assert_eq!(
        right
            .query("SELECT sequence FROM checkpoint", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Integer(0)]]
    );
    left.execute(
        "UPDATE checkpoint SET sequence=sequence+1 WHERE id=1",
        &[],
        &guard,
    )
    .unwrap();
    let right_update = right.execute(
        "UPDATE checkpoint SET sequence=sequence+1 WHERE id=1",
        &[],
        &guard,
    );
    left.commit(&guard).unwrap();
    let conflict = match right_update {
        Ok(_) => right.commit(&guard).unwrap_err(),
        Err(error) => error,
    };
    assert!(conflict.requires_transaction_retry(), "{conflict}");
    assert!(!conflict.is_deterministic_refusal());
    right.rollback().unwrap();
    assert!(right.is_autocommit());
    // Never repeat the UPDATE inside the failed transaction's stale snapshot.
    right
        .begin(TransactionBehavior::Concurrent, &guard)
        .unwrap();
    assert_eq!(
        right
            .query("SELECT sequence FROM checkpoint", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Integer(1)]]
    );
    right
        .execute(
            "UPDATE checkpoint SET sequence=sequence+1 WHERE id=1",
            &[],
            &guard,
        )
        .unwrap();
    right.commit(&guard).unwrap();
    drop(right);
    drop(left);
    drop(database);
    let reopened = Database::open(&directory.path().join("native.db")).unwrap();
    let mut reader = reopened.connect(Access::Reader).unwrap();
    assert_eq!(
        reader
            .query("SELECT sequence FROM checkpoint", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Integer(2)]]
    );
}
