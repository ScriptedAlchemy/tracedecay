use std::{
    fs,
    sync::{
        Arc,
        atomic::Ordering,
        mpsc::{SyncSender, sync_channel},
    },
    time::Duration,
};

use rusqlite::{Savepoint, Transaction};
use tracedecay_rusqlite_runtime::{
    StorageOperationExecutor, open_immutable_reader,
    reader::{ReaderPool, ReaderQueryExecutor},
};
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeCancellationStageV1, RuntimeReadCoverageV1,
    RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeSubmitOutcomeV1,
    StorageRuntimeErrorV1,
};

use crate::cutover_support::{
    Probe, TestDatabase, fixture, outbox_request, read_request, reader_locator, run,
    writer_with_executor,
};

const LONG_QUERY: &str = "WITH RECURSIVE n(x) AS (
    VALUES(1)
    UNION ALL
    SELECT x + 1 FROM n WHERE x < 1000000000
) SELECT sum(x) FROM n";

#[test]
fn immutable_reader_authorizer_denies_mutation_without_changing_the_store() {
    let database = TestDatabase::new("connection-authorizer.sqlite3");
    database
        .connect()
        .execute_batch(
            "CREATE TABLE protected(value INTEGER NOT NULL);
             INSERT INTO protected(value) VALUES (1);",
        )
        .expect("seed protected database");
    let before = fs::read(&database.path).expect("read database before immutable access");

    let reader = open_immutable_reader(&database.path).expect("open immutable runtime reader");
    assert_eq!(
        reader
            .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .expect("run authorized integrity diagnostic"),
        "ok"
    );
    assert!(
        reader
            .execute("INSERT INTO protected(value) VALUES (2)", [])
            .is_err()
    );
    assert!(reader.execute_batch("PRAGMA user_version = 1").is_err());
    drop(reader);

    assert_eq!(
        fs::read(&database.path).expect("read database after immutable access"),
        before
    );
}

#[test]
fn reader_cancellation_interrupts_a_live_sqlite_query() {
    let binding = fixture().s5.binding;
    let database = TestDatabase::new("reader-interrupt.sqlite3");
    database
        .connect()
        .execute_batch("CREATE TABLE acceptance_rows(value INTEGER NOT NULL)")
        .expect("seed reader database");
    let (entered_tx, entered_rx) = sync_channel(1);
    let pool = ReaderPool::start(
        reader_locator(&binding, &database.path),
        AdmissionConfigV1::default().readers,
        LongRead {
            entered: entered_tx,
        },
    )
    .expect("start runtime reader pool");
    let request = read_request(&binding, "foreground");
    let (probe, cancellation) = Probe::controllable_for_read(&request);
    let mut lease = pool
        .acquire(&request, &probe, Duration::ZERO)
        .expect("acquire runtime reader");
    let mut snapshot = lease.begin_snapshot().expect("begin runtime snapshot");
    let cancel = std::thread::spawn(move || {
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("long query entered");
        cancellation.store(1, Ordering::Release);
    });

    let outcome = snapshot
        .execute(request, &probe)
        .expect("cancellation returns a typed read outcome");
    assert!(matches!(
        outcome.coverage(),
        RuntimeReadCoverageV1::Unavailable {
            reason: tracedecay_store::UnavailableReasonV1::Cancelled,
            ..
        }
    ));
    cancel.join().expect("join reader cancellation");
}

#[test]
fn writer_progress_cancellation_rolls_back_the_active_transaction() {
    let fixture = fixture();
    let database = TestDatabase::new("writer-progress-cancellation.sqlite3");
    let request = outbox_request(
        &fixture.s9.origin_binding,
        &fixture.s9.target_binding,
        "operation.engine.progress-cancellation",
        "effect.engine.progress-cancellation",
        "ordering.engine.progress-cancellation",
    );
    let (entered_tx, entered_rx) = sync_channel(1);
    let writer = Arc::new(writer_with_executor(
        &database,
        &fixture.s9.origin_binding,
        LongWrite {
            entered: entered_tx,
        },
    ));
    let (probe, cancellation) = Probe::controllable_for_submit(&request);

    let outcome = run(async {
        let task_writer = Arc::clone(&writer);
        let task = tokio::spawn(async move { task_writer.submit(request, probe).await });
        tokio::task::yield_now().await;
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("long write entered");
        cancellation.store(1, Ordering::Release);
        task.await
            .expect("join interrupted write")
            .expect("settle interrupted write")
    });
    assert!(matches!(
        outcome,
        RuntimeSubmitOutcomeV1::CancelledBeforeCommit {
            stage: RuntimeCancellationStageV1::BeforeCommit,
            ..
        }
    ));

    Arc::try_unwrap(writer)
        .unwrap_or_else(|_| panic!("submit retained the engine-contract writer"))
        .shutdown_and_join()
        .expect("close engine-contract writer");
    assert_eq!(
        database
            .connect()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name = 'progress_marker'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("inspect rolled-back marker"),
        0
    );
}

#[derive(Clone)]
struct LongRead {
    entered: SyncSender<()>,
}

impl ReaderQueryExecutor for LongRead {
    fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        self.entered
            .send(())
            .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
                operation: format!("signal long read: {error}"),
            })?;
        let value = snapshot
            .query_row(LONG_QUERY, [], |row| row.get::<_, i64>(0))
            .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
                operation: format!("execute long read: {error}"),
            })?;
        RuntimeReadOutcomeV1::new(
            Some(RuntimeReadResultV1::GraphQuickCheck { healthy: value > 0 }),
            RuntimeReadCoverageV1::Latest { observed: None },
        )
        .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
            operation: format!("construct long-read outcome: {error}"),
        })
    }
}

struct LongWrite {
    entered: SyncSender<()>,
}

impl StorageOperationExecutor for LongWrite {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        savepoint.execute_batch(
            "CREATE TABLE progress_marker(value INTEGER NOT NULL);
             INSERT INTO progress_marker(value) VALUES (1);",
        )?;
        self.entered
            .send(())
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        savepoint.query_row(LONG_QUERY, [], |row| row.get::<_, i64>(0))?;
        Ok(())
    }
}
