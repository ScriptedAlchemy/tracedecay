use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlError, ExactSqlWriteAuthority, ExactSqlWriteIntent,
};

use super::{
    BackendKind, Error, NativeTestConnection, QueryExecutor, TransactionBehavior, WriteStatement,
};

#[tokio::test]
async fn native_facade_preserves_bound_values_rollback_snapshots_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native.db");
    let connection = NativeTestConnection::open(&path).unwrap();
    assert_eq!(
        QueryExecutor::backend_kind(&connection),
        BackendKind::NativeTurso
    );
    connection.execute_batch("CREATE TABLE records (id INTEGER PRIMARY KEY, body TEXT NOT NULL, payload BLOB, score REAL)").await.unwrap();
    connection
        .execute(
            "INSERT INTO records VALUES (?1, ?2, ?3, ?4)",
            (1_i64, "quoted ' value", vec![0_u8, 255], 1.25_f64),
        )
        .await
        .unwrap();
    let snapshot = connection.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query("SELECT body, payload, score FROM records", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "quoted ' value");
    assert_eq!(row.get::<Vec<u8>>(1).unwrap(), vec![0, 255]);
    assert_eq!(row.get::<f64>(2).unwrap(), 1.25);
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    transaction
        .execute("UPDATE records SET body = ?1", ("rolled back",))
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    assert_eq!(
        transaction
            .execute_statements(vec![
                WriteStatement::new("UPDATE records SET body = ?1", ("committed",)).unwrap()
            ])
            .await
            .unwrap(),
        vec![1]
    );
    transaction.commit().await.unwrap();
    let mut rows = snapshot
        .query("SELECT body FROM records", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "quoted ' value"
    );
    let error = connection
        .read_only()
        .query("DELETE FROM records RETURNING id", ())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::InvalidOperation(_)), "{error:?}");
    drop(snapshot);
    drop(connection);
    let reopened = NativeTestConnection::open(&path).unwrap();
    let mut rows = reopened
        .query("SELECT body FROM records", ())
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

struct Revocable(Arc<AtomicBool>);
impl ExactSqlWriteAuthority for Revocable {
    fn verify(&self, _intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if self.0.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(ExactSqlError::AuthorityDenied(
                "revoked test store".to_owned(),
            ))
        }
    }
}

#[tokio::test]
async fn native_facade_denies_revoked_commit_and_keeps_rollback_available() {
    let directory = tempfile::tempdir().unwrap();
    let allowed = Arc::new(AtomicBool::new(true));
    let connection = NativeTestConnection::open_with_write_authority(
        &directory.path().join("authority.db"),
        Arc::new(Revocable(Arc::clone(&allowed))),
    )
    .unwrap();
    connection
        .execute_batch("CREATE TABLE records (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    transaction
        .execute("INSERT INTO records VALUES (1)", ())
        .await
        .unwrap();
    allowed.store(false, Ordering::Release);
    assert!(matches!(
        transaction.commit().await,
        Err(Error::InvalidOperation(_))
    ));
    allowed.store(true, Ordering::Release);
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    transaction
        .execute("INSERT INTO records VALUES (2)", ())
        .await
        .unwrap();
    allowed.store(false, Ordering::Release);
    transaction.rollback().await.unwrap();
    allowed.store(true, Ordering::Release);
    let mut rows = connection
        .query("SELECT count(*) FROM records", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
}

struct RevokeDuringQuery {
    enabled: Arc<AtomicBool>,
    checks: Arc<std::sync::atomic::AtomicUsize>,
}

impl ExactSqlWriteAuthority for RevokeDuringQuery {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if matches!(
            intent,
            ExactSqlWriteIntent::Query | ExactSqlWriteIntent::Execute
        ) && self.enabled.load(Ordering::Acquire)
            && self.checks.fetch_add(1, Ordering::AcqRel) >= 16
        {
            return Err(ExactSqlError::AuthorityDenied(
                "revoked during native SQL".to_owned(),
            ));
        }
        Ok(())
    }
}

#[tokio::test]
async fn native_transaction_revoked_during_sql_rolls_back_and_refuses_followup() {
    let directory = tempfile::tempdir().unwrap();
    let enabled = Arc::new(AtomicBool::new(false));
    let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let connection = NativeTestConnection::open_with_write_authority(
        &directory.path().join("mid-query.db"),
        Arc::new(RevokeDuringQuery {
            enabled: Arc::clone(&enabled),
            checks: Arc::clone(&checks),
        }),
    )
    .unwrap();
    connection
        .execute_batch("CREATE TABLE records (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    transaction
        .execute("INSERT INTO records VALUES (1)", ())
        .await
        .unwrap();
    enabled.store(true, Ordering::Release);
    let error = transaction.query("WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM numbers WHERE n < 100000) SELECT sum(n) FROM numbers", ()).await.unwrap_err();
    assert!(matches!(error, Error::InvalidOperation(_)), "{error:?}");
    assert!(checks.load(Ordering::Acquire) > 16);
    enabled.store(false, Ordering::Release);
    assert!(matches!(
        transaction
            .execute("INSERT INTO records VALUES (2)", ())
            .await,
        Err(Error::TransactionClosed)
    ));
    let mut rows = connection
        .query("SELECT count(*) FROM records", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
}

#[tokio::test]
async fn native_background_snapshot_leases_reserve_foreground_and_release_on_drop() {
    let directory = tempfile::tempdir().unwrap();
    let connection =
        NativeTestConnection::open(&directory.path().join("reader-budget.db")).unwrap();
    connection
        .execute_batch("CREATE TABLE records (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();
    let background = connection.read_only().background();
    let mut held_background = Vec::new();
    let budget = tracedecay_store::AdmissionConfigV1::default().readers;
    for _ in 0..budget.max_per_hot_shard.saturating_sub(2).max(1) {
        held_background.push(background.read_snapshot().await.unwrap());
    }
    let (started, observing) = tokio::sync::oneshot::channel();
    let mut waiting = tokio::spawn(async move {
        started.send(()).unwrap();
        background.read_snapshot().await
    });
    observing.await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut waiting)
            .await
            .is_err()
    );
    let first = connection.read_snapshot().await.unwrap();
    let second = connection.read_snapshot().await.unwrap();
    let mut rows = first
        .query("SELECT count(*) FROM records", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(first);
    drop(second);
    held_background.pop();
    let admitted = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(admitted);
}

struct NotifyQueryProgress {
    active: Arc<AtomicBool>,
    checks: std::sync::atomic::AtomicUsize,
    started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl ExactSqlWriteAuthority for NotifyQueryProgress {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if matches!(intent, ExactSqlWriteIntent::Query)
            && self.active.load(Ordering::Acquire)
            && self.checks.fetch_add(1, Ordering::AcqRel) >= 16
            && let Some(started) = self.started.lock().unwrap().take()
        {
            started.send(()).unwrap();
        }
        Ok(())
    }
}

#[tokio::test]
async fn dropping_native_query_future_cancels_and_rolls_back_its_owned_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let active = Arc::new(AtomicBool::new(false));
    let (started, observing) = tokio::sync::oneshot::channel();
    let connection = NativeTestConnection::open_with_write_authority(
        &directory.path().join("cancelled-query.db"),
        Arc::new(NotifyQueryProgress {
            active: Arc::clone(&active),
            checks: std::sync::atomic::AtomicUsize::new(0),
            started: std::sync::Mutex::new(Some(started)),
        }),
    )
    .unwrap();
    connection
        .execute_batch("CREATE TABLE records (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();
    let transaction = Arc::new(
        connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap(),
    );
    transaction
        .execute("INSERT INTO records VALUES (1)", ())
        .await
        .unwrap();
    active.store(true, Ordering::Release);
    let running = Arc::clone(&transaction);
    let query = tokio::spawn(async move {
        running.query("WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM numbers WHERE n < 1000000000) SELECT sum(n) FROM numbers", ()).await
    });
    observing.await.unwrap();
    query.abort();
    assert!(query.await.unwrap_err().is_cancelled());
    active.store(false, Ordering::Release);
    assert!(matches!(
        transaction
            .execute("INSERT INTO records VALUES (2)", ())
            .await,
        Err(Error::TransactionClosed)
    ));
    let mut rows = connection
        .query("SELECT count(*) FROM records", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
}

struct BatchReleaseOnDrop(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
impl Drop for BatchReleaseOnDrop {
    fn drop(&mut self) {
        *self
            .0
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.0.1.notify_all();
    }
}

struct PausedBatchAdmission {
    skip_first_check: bool,
    checks: std::sync::atomic::AtomicUsize,
    enabled: Arc<AtomicBool>,
    admitted: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    gate: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl ExactSqlWriteAuthority for PausedBatchAdmission {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if matches!(intent, ExactSqlWriteIntent::Execute) && self.enabled.load(Ordering::Acquire) {
            if self.skip_first_check && self.checks.fetch_add(1, Ordering::AcqRel) == 0 {
                return Ok(());
            }
            if let Some(admitted) = self.admitted.lock().unwrap().take() {
                admitted.send(()).unwrap();
            }
            let (released, wake) = &*self.gate;
            let mut released = released.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
        }
        Ok(())
    }
}

#[tokio::test]
async fn native_admitted_batch_finishes_after_its_waiter_is_aborted() {
    let directory = tempfile::tempdir().unwrap();
    let enabled = Arc::new(AtomicBool::new(false));
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let release = BatchReleaseOnDrop(Arc::clone(&gate));
    let (admitted, observing) = tokio::sync::oneshot::channel();
    let connection = NativeTestConnection::open_with_write_authority(
        &directory.path().join("admitted-batch.db"),
        Arc::new(PausedBatchAdmission {
            skip_first_check: false,
            checks: std::sync::atomic::AtomicUsize::new(0),
            enabled: Arc::clone(&enabled),
            admitted: std::sync::Mutex::new(Some(admitted)),
            gate: Arc::clone(&gate),
        }),
    )
    .unwrap();
    connection
        .execute_batch("CREATE TABLE records (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();
    let transaction = Arc::new(
        connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap(),
    );
    enabled.store(true, Ordering::Release);
    let waiting = Arc::clone(&transaction);
    let batch = tokio::spawn(async move {
        waiting
            .execute_statements(vec![
                WriteStatement::new("INSERT INTO records VALUES (?1)", (1_i64,)).unwrap(),
                WriteStatement::new("INSERT INTO records VALUES (?1)", (2_i64,)).unwrap(),
            ])
            .await
    });
    observing.await.unwrap();
    batch.abort();
    assert!(batch.await.unwrap_err().is_cancelled());
    drop(release);
    let mut rows = transaction
        .query("SELECT count(*) FROM records", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        2
    );
    Arc::try_unwrap(transaction)
        .ok()
        .unwrap()
        .commit()
        .await
        .unwrap();
    let mut rows = connection
        .query("SELECT count(*) FROM records", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        2
    );
}

#[tokio::test]
async fn native_admitted_batch_stops_on_revocation_and_rolls_back_all_stages() {
    let directory = tempfile::tempdir().unwrap();
    let enabled = Arc::new(AtomicBool::new(false));
    let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let connection = NativeTestConnection::open_with_write_authority(
        &directory.path().join("revoked-batch.db"),
        Arc::new(RevokeDuringQuery {
            enabled: Arc::clone(&enabled),
            checks,
        }),
    )
    .unwrap();
    connection
        .execute_batch("CREATE TABLE records (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    transaction
        .execute("INSERT INTO records VALUES (-1)", ())
        .await
        .unwrap();
    enabled.store(true, Ordering::Release);
    let error = transaction.execute_statements(vec![
        WriteStatement::new("WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM numbers WHERE n < 100000) INSERT INTO records SELECT n FROM numbers", ()).unwrap(),
        WriteStatement::new("INSERT INTO records VALUES (-2)", ()).unwrap(),
    ]).await.unwrap_err();
    assert!(
        matches!(error, Error::StatementBatch { index: 0, .. }),
        "{error:?}"
    );
    enabled.store(false, Ordering::Release);
    assert!(matches!(
        transaction
            .execute("INSERT INTO records VALUES (-3)", ())
            .await,
        Err(Error::TransactionClosed)
    ));
    let mut rows = connection
        .query("SELECT count(*) FROM records", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
}

#[tokio::test]
async fn native_returned_insert_ids_remain_bound_after_other_writes_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("returned-native-ids.db");
    let connection = NativeTestConnection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE records(id INTEGER PRIMARY KEY, label TEXT NOT NULL UNIQUE)")
        .await
        .unwrap();
    let mut inserted = Vec::new();
    for label in ["first", "second"] {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let mut returned = transaction
            .query(
                "INSERT INTO records(label) VALUES (?1) RETURNING id",
                (label,),
            )
            .await
            .unwrap();
        let id = returned
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap();
        assert!(returned.next().await.unwrap().is_none());
        transaction.commit().await.unwrap();
        inserted.push((label, id));
    }
    assert_ne!(inserted[0].1, inserted[1].1);
    connection
        .execute(
            "UPDATE records SET label = ?1 WHERE id = ?2",
            ("updated", inserted[0].1),
        )
        .await
        .unwrap();
    inserted[0].0 = "updated";
    drop(connection);
    let reopened = NativeTestConnection::open(&path).unwrap();
    for (label, id) in inserted {
        let mut rows = reopened
            .query("SELECT label FROM records WHERE id = ?1", (id,))
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            label
        );
        assert!(rows.next().await.unwrap().is_none());
    }
}

#[tokio::test]
async fn native_autocommit_returning_binds_ids_to_each_writer_statement() {
    let directory = tempfile::tempdir().unwrap();
    let connection =
        NativeTestConnection::open(&directory.path().join("autocommit-returning.db")).unwrap();
    connection
        .execute_batch("CREATE TABLE records(id INTEGER PRIMARY KEY, label TEXT NOT NULL UNIQUE)")
        .await
        .unwrap();
    let mut first = connection
        .execute_returning(
            "INSERT INTO records(label) VALUES (?1) RETURNING id",
            ("first",),
        )
        .await
        .unwrap();
    let first_id = first.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    let mut second = connection
        .execute_returning(
            "INSERT INTO records(label) VALUES (?1) RETURNING id",
            ("second",),
        )
        .await
        .unwrap();
    let second_id = second.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    assert_ne!(first_id, second_id);
    assert!(first.next().await.unwrap().is_none());
    assert!(second.next().await.unwrap().is_none());
    let mut ignored = connection
        .execute_returning(
            "INSERT OR IGNORE INTO records(label) VALUES (?1) RETURNING id",
            ("first",),
        )
        .await
        .unwrap();
    assert!(ignored.next().await.unwrap().is_none());
    for (id, label) in [(first_id, "first"), (second_id, "second")] {
        let mut rows = connection
            .query("SELECT label FROM records WHERE id = ?1", (id,))
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            label
        );
    }
}

#[tokio::test]
async fn native_admitted_returning_write_persists_after_its_waiter_is_aborted() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("abandoned-returning.db");
    let enabled = Arc::new(AtomicBool::new(false));
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let release = BatchReleaseOnDrop(Arc::clone(&gate));
    let (admitted, observing) = tokio::sync::oneshot::channel();
    let connection = NativeTestConnection::open_with_write_authority(
        &path,
        Arc::new(PausedBatchAdmission {
            skip_first_check: true,
            checks: std::sync::atomic::AtomicUsize::new(0),
            enabled: Arc::clone(&enabled),
            admitted: std::sync::Mutex::new(Some(admitted)),
            gate,
        }),
    )
    .unwrap();
    connection
        .execute_batch("CREATE TABLE records(id INTEGER PRIMARY KEY, label TEXT NOT NULL UNIQUE)")
        .await
        .unwrap();
    enabled.store(true, Ordering::Release);
    let waiting: super::Connection = connection.clone();
    let write = tokio::spawn(async move {
        waiting
            .execute_returning(
                "INSERT INTO records(label) VALUES (?1) RETURNING id",
                ("abandoned",),
            )
            .await
    });
    observing.await.unwrap();
    write.abort();
    assert!(write.await.unwrap_err().is_cancelled());
    drop(release);
    let id = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let mut rows = connection
                .query("SELECT id FROM records WHERE label = ?1", ("abandoned",))
                .await
                .unwrap();
            if let Some(row) = rows.next().await.unwrap() {
                break row.get::<i64>(0).unwrap();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    enabled.store(false, Ordering::Release);
    let mut other = connection
        .execute_returning(
            "INSERT INTO records(label) VALUES (?1) RETURNING id",
            ("other",),
        )
        .await
        .unwrap();
    assert_ne!(
        other.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        id
    );
    drop(connection);
    let reopened = NativeTestConnection::open(&path).unwrap();
    let mut rows = reopened
        .query("SELECT label FROM records WHERE id = ?1", (id,))
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "abandoned"
    );
}

#[tokio::test]
async fn native_returning_row_budget_refusal_rolls_back_before_reopen() {
    assert_native_returning_budget_rollback(
        "returning-row-budget.db",
        10_001,
        "INSERT INTO records(id) SELECT id FROM seed RETURNING id",
    )
    .await;
}

#[tokio::test]
async fn native_returning_byte_budget_refusal_rolls_back_before_reopen() {
    assert_native_returning_budget_rollback(
        "returning-byte-budget.db",
        100,
        "INSERT INTO records(id) SELECT id FROM seed RETURNING zeroblob(1024 * 1024)",
    )
    .await;
}

async fn assert_native_returning_budget_rollback(name: &str, seed_rows: usize, sql: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(name);
    let connection = NativeTestConnection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE seed(id INTEGER PRIMARY KEY); CREATE TABLE records(id INTEGER PRIMARY KEY)",
        )
        .await
        .unwrap();
    let values = (1..=seed_rows)
        .map(|id| format!("({id})"))
        .collect::<Vec<_>>()
        .join(",");
    connection
        .execute(&format!("INSERT INTO seed VALUES {values}"), ())
        .await
        .unwrap();
    let error = connection.execute_returning(sql, ()).await.unwrap_err();
    assert!(
        matches!(error, Error::InvalidOperation(ref message) if message == "native query materialization limit exceeded"),
        "{error:?}"
    );
    let mut rows = connection
        .read_only()
        .query("SELECT count(*) FROM records", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);
    drop(connection);
    let reopened = NativeTestConnection::open(&path).unwrap();
    let mut rows = reopened
        .query("SELECT count(*) FROM records", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    let mut returned = reopened
        .execute_returning("INSERT INTO records(id) VALUES (-1) RETURNING id", ())
        .await
        .unwrap();
    assert_eq!(
        returned
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        -1
    );
    assert!(returned.next().await.unwrap().is_none());
    let mut rows = reopened.query("SELECT id FROM records", ()).await.unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        -1
    );
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn native_snapshot_is_pinned_before_its_first_caller_query() {
    let directory = tempfile::tempdir().unwrap();
    let connection =
        NativeTestConnection::open(&directory.path().join("snapshot-admission.db")).unwrap();
    connection.execute_batch("CREATE TABLE records(id INTEGER PRIMARY KEY, label TEXT NOT NULL); INSERT INTO records VALUES(1, 'before')").await.unwrap();
    let snapshot = connection.read_snapshot().await.unwrap();
    connection
        .execute(
            "UPDATE records SET label = ?1 WHERE id = ?2",
            ("after", 1_i64),
        )
        .await
        .unwrap();
    let mut rows = snapshot
        .query("SELECT label FROM records WHERE id = ?1", (1_i64,))
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "before"
    );
    let mut current = connection
        .query("SELECT label FROM records WHERE id = ?1", (1_i64,))
        .await
        .unwrap();
    assert_eq!(
        current
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "after"
    );
}
