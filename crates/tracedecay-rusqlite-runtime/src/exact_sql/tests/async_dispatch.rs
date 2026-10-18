use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Mutex, mpsc},
    task::Poll,
};

use super::*;

fn current_thread() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

struct IntentGate {
    intent: ExactSqlWriteIntent,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl ExactSqlWriteAuthority for IntentGate {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if intent == self.intent {
            let entered = self.entered.lock().unwrap().take();
            if let Some(entered) = entered {
                let _ = entered.send(());
                self.release.lock().unwrap().recv().map_err(|_| {
                    ExactSqlError::AuthorityDenied(
                        "test authority gate released by drop".to_owned(),
                    )
                })?;
            }
        }
        Ok(())
    }
}

fn gated_handle(
    fixture: &Fixture,
    intent: ExactSqlWriteIntent,
) -> (
    ExactSqlHandle,
    tokio::sync::oneshot::Receiver<()>,
    mpsc::Sender<()>,
) {
    let (entered, accepted) = tokio::sync::oneshot::channel();
    let (release, gate) = mpsc::channel();
    let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(Arc::new(IntentGate {
            intent,
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(gate),
        }))
        .unwrap();
    (handle, accepted, release)
}

async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    poll_fn(|context| Poll::Ready(future.as_mut().poll(context))).await
}

#[test]
fn synchronous_adapters_remain_safe_inside_current_thread_tokio() {
    current_thread().block_on(async {
        let fixture = fixture('a', 'a');
        let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        handle
            .execute_batch("CREATE TABLE sync_adapter (value INTEGER)".to_owned())
            .unwrap();
        handle
            .validate(statement(
                "INSERT INTO sync_adapter VALUES (?)",
                vec![ExactSqlValue::Integer(1)],
            ))
            .unwrap();
        assert_eq!(
            handle
                .execute(statement("INSERT INTO sync_adapter VALUES (1)", vec![]))
                .unwrap()
                .changed_rows,
            1
        );
        let transaction = handle.begin_immediate().unwrap();
        transaction
            .execute(statement("INSERT INTO sync_adapter VALUES (2)", vec![]))
            .unwrap();
        assert_eq!(
            transaction
                .query(statement("SELECT count(*) FROM sync_adapter", vec![]))
                .unwrap()
                .rows[0]
                .values,
            vec![ExactSqlValue::Integer(2)]
        );
        transaction.commit().unwrap();
        let transaction = handle.begin_deferred().unwrap();
        transaction
            .execute(statement("INSERT INTO sync_adapter VALUES (3)", vec![]))
            .unwrap();
        transaction.rollback().unwrap();
        let rows = handle
            .query(
                statement("SELECT value FROM sync_adapter ORDER BY value", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            rows.rows
                .iter()
                .map(|row| row.values.clone())
                .collect::<Vec<_>>(),
            vec![
                vec![ExactSqlValue::Integer(1)],
                vec![ExactSqlValue::Integer(2)]
            ]
        );
    });
}

#[test]
fn async_execute_drop_before_poll_does_not_admit_a_write() {
    current_thread().block_on(async {
        let fixture = fixture('a', 'a');
        let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        handle
            .execute_batch_async("CREATE TABLE unpolled (value INTEGER)".to_owned())
            .await
            .unwrap();
        let unpolled = handle.execute_async(statement("INSERT INTO unpolled VALUES (1)", vec![]));
        drop(unpolled);
        // The awaited write also fences any command mistakenly admitted eagerly.
        handle
            .execute_async(statement("INSERT INTO unpolled VALUES (2)", vec![]))
            .await
            .unwrap();
        let rows = handle
            .query(
                statement("SELECT value FROM unpolled", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            rows.rows,
            vec![ExactSqlRow {
                values: vec![ExactSqlValue::Integer(2)]
            }]
        );
    });
}

#[test]
fn async_execute_drop_after_acceptance_preserves_the_admitted_effect() {
    current_thread().block_on(async {
        let fixture = fixture('a', 'a');
        let base = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        base.execute_batch("CREATE TABLE accepted (value INTEGER UNIQUE)".to_owned())
            .unwrap();
        let (handle, accepted, release) = gated_handle(&fixture, ExactSqlWriteIntent::Execute);
        let mut pending =
            Box::pin(handle.execute_async(statement("INSERT INTO accepted VALUES (1)", vec![])));
        assert!(poll_once(pending.as_mut()).await.is_pending());
        accepted.await.unwrap();
        drop(pending);
        release.send(()).unwrap();
        base.execute_async(statement("INSERT INTO accepted VALUES (2)", vec![]))
            .await
            .unwrap();
        let rows = base
            .query(
                statement("SELECT value FROM accepted ORDER BY value", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            rows.rows,
            vec![
                ExactSqlRow {
                    values: vec![ExactSqlValue::Integer(1)]
                },
                ExactSqlRow {
                    values: vec![ExactSqlValue::Integer(2)]
                }
            ]
        );
    });
}

#[test]
fn async_transactions_commit_rollback_and_drop_have_durable_boundaries() {
    current_thread().block_on(async {
        let fixture = fixture('a', 'a');
        let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        handle
            .execute_batch_async("CREATE TABLE async_transaction (value INTEGER)".to_owned())
            .await
            .unwrap();
        handle
            .validate_async(statement(
                "INSERT INTO async_transaction VALUES (?)",
                vec![ExactSqlValue::Integer(1)],
            ))
            .await
            .unwrap();
        let transaction = handle.begin_immediate_async().await.unwrap();
        transaction
            .execute_async(statement(
                "INSERT INTO async_transaction VALUES (1)",
                vec![],
            ))
            .await
            .unwrap();
        assert_eq!(
            transaction
                .query_async(statement("SELECT value FROM async_transaction", vec![]))
                .await
                .unwrap()
                .rows[0]
                .values,
            vec![ExactSqlValue::Integer(1)]
        );
        transaction.commit_async().await.unwrap();
        let transaction = handle.begin_deferred_async().await.unwrap();
        transaction
            .execute_async(statement(
                "INSERT INTO async_transaction VALUES (2)",
                vec![],
            ))
            .await
            .unwrap();
        transaction.rollback_async().await.unwrap();
        let transaction = handle.begin_immediate_async().await.unwrap();
        transaction
            .execute_batch_async("INSERT INTO async_transaction VALUES (3)".to_owned())
            .await
            .unwrap();
        drop(transaction);
        // A subsequent writer round trip cannot finish until the dropped lease rolls back.
        handle
            .execute_async(statement(
                "INSERT INTO async_transaction VALUES (4)",
                vec![],
            ))
            .await
            .unwrap();
        let rows = handle
            .query(
                statement("SELECT value FROM async_transaction ORDER BY value", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            rows.rows,
            vec![
                ExactSqlRow {
                    values: vec![ExactSqlValue::Integer(1)]
                },
                ExactSqlRow {
                    values: vec![ExactSqlValue::Integer(4)]
                }
            ]
        );
    });
}

#[test]
fn async_transaction_queue_saturation_refuses_only_the_unadmitted_command() {
    current_thread().block_on(async {
        let fixture = fixture('a', 'a');
        let base = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        base.execute_batch("CREATE TABLE saturated (value INTEGER)".to_owned())
            .unwrap();
        let (handle, accepted, release) = gated_handle(&fixture, ExactSqlWriteIntent::Execute);
        let transaction = handle.begin_immediate_async().await.unwrap();
        let mut first = Box::pin(
            transaction.execute_async(statement("INSERT INTO saturated VALUES (1)", vec![])),
        );
        assert!(poll_once(first.as_mut()).await.is_pending());
        accepted.await.unwrap();
        let mut queued = Box::pin(
            transaction.execute_async(statement("INSERT INTO saturated VALUES (2)", vec![])),
        );
        assert!(poll_once(queued.as_mut()).await.is_pending());
        let rejected = transaction
            .execute_async(statement("INSERT INTO saturated VALUES (3)", vec![]))
            .await
            .unwrap_err();
        assert!(matches!(rejected, ExactSqlError::Busy));
        release.send(()).unwrap();
        first.await.unwrap();
        queued.await.unwrap();
        transaction.commit_async().await.unwrap();
        let rows = base
            .query(
                statement("SELECT value FROM saturated ORDER BY value", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            rows.rows,
            vec![
                ExactSqlRow {
                    values: vec![ExactSqlValue::Integer(1)]
                },
                ExactSqlRow {
                    values: vec![ExactSqlValue::Integer(2)]
                }
            ]
        );
    });
}

#[test]
fn async_begin_drop_while_queued_releases_the_abandoned_transaction() {
    current_thread().block_on(async {
        let fixture = fixture('a', 'a');
        let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        handle
            .execute_batch_async("CREATE TABLE abandoned_begin (value INTEGER)".to_owned())
            .await
            .unwrap();
        let holder = handle.begin_immediate_async().await.unwrap();
        holder
            .execute_async(statement("INSERT INTO abandoned_begin VALUES (1)", vec![]))
            .await
            .unwrap();
        // The held lease prevents the admitted BEGIN from replying before cancellation.
        let mut pending = Box::pin(handle.begin_immediate_async());
        assert!(poll_once(pending.as_mut()).await.is_pending());
        drop(pending);
        holder.rollback_async().await.unwrap();
        // This lease cannot open until the abandoned BEGIN has released its transaction.
        let successor = handle.begin_immediate_async().await.unwrap();
        successor
            .execute_async(statement("INSERT INTO abandoned_begin VALUES (2)", vec![]))
            .await
            .unwrap();
        successor.commit_async().await.unwrap();
        let rows = handle
            .query(
                statement("SELECT value FROM abandoned_begin", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            rows.rows,
            vec![ExactSqlRow {
                values: vec![ExactSqlValue::Integer(2)]
            }]
        );
    });
}

#[test]
fn async_commit_drop_after_admission_preserves_committed_data() {
    current_thread().block_on(async {
        let fixture = fixture('a', 'a');
        let base = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        base.execute_batch("CREATE TABLE abandoned_commit (value INTEGER UNIQUE)".to_owned())
            .unwrap();
        let (handle, accepted, release) = gated_handle(&fixture, ExactSqlWriteIntent::Commit);
        let transaction = handle.begin_immediate_async().await.unwrap();
        transaction
            .execute_async(statement("INSERT INTO abandoned_commit VALUES (1)", vec![]))
            .await
            .unwrap();
        let mut pending = Box::pin(transaction.commit_async());
        assert!(poll_once(pending.as_mut()).await.is_pending());
        accepted.await.unwrap();
        drop(pending);
        release.send(()).unwrap();
        // The next writer round trip fences the commit whose receiver was dropped.
        base.execute_async(statement("INSERT INTO abandoned_commit VALUES (2)", vec![]))
            .await
            .unwrap();
        let rows = base
            .query(
                statement("SELECT value FROM abandoned_commit ORDER BY value", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            rows.rows,
            vec![
                ExactSqlRow {
                    values: vec![ExactSqlValue::Integer(1)]
                },
                ExactSqlRow {
                    values: vec![ExactSqlValue::Integer(2)]
                }
            ]
        );
    });
}

#[test]
fn admitted_execute_returning_completes_after_reply_waiter_is_abandoned() {
    current_thread().block_on(async {
        let fixture = fixture('a', 'a');
        let base = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        base.execute_batch(
            "CREATE TABLE returning_abandoned (id INTEGER PRIMARY KEY, value TEXT)".to_owned(),
        )
        .unwrap();
        let (handle, accepted, release) = gated_handle(&fixture, ExactSqlWriteIntent::Execute);
        let mut pending = Box::pin(handle.execute_returning_async(statement(
            "INSERT INTO returning_abandoned(value) VALUES ('admitted') RETURNING id",
            vec![],
        )));
        assert!(poll_once(pending.as_mut()).await.is_pending());
        accepted.await.unwrap();
        drop(pending);
        release.send(()).unwrap();
        let rows = base
            .execute_returning_async(statement(
                "INSERT INTO returning_abandoned(value) VALUES ('barrier') RETURNING id",
                vec![],
            ))
            .await
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(2)]);
        let persisted = base
            .query(
                statement(
                    "SELECT id, value FROM returning_abandoned ORDER BY id",
                    vec![],
                ),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            persisted.rows,
            vec![
                ExactSqlRow {
                    values: vec![
                        ExactSqlValue::Integer(1),
                        ExactSqlValue::Text("admitted".to_owned())
                    ]
                },
                ExactSqlRow {
                    values: vec![
                        ExactSqlValue::Integer(2),
                        ExactSqlValue::Text("barrier".to_owned())
                    ]
                },
            ]
        );
    });
}

/// Serializes the tests that read the process-wide wait counter.
static NATIVE_WAIT_COUNTER: Mutex<()> = Mutex::new(());

#[test]
fn a_writer_wait_on_a_native_worker_is_counted() {
    let _serial = NATIVE_WAIT_COUNTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = fixture('a', 'a');
    let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let before = native_thread_writer_waits();
    let task = crate::native_runtime::NativeTask::spawn(async move {
        handle
            .execute_batch("CREATE TABLE native_worker (value INTEGER)".to_owned())
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .unwrap();
    let outcome = crate::native_runtime::block_on(task.wait()).unwrap();
    assert_eq!(
        outcome,
        crate::native_runtime::NativeTaskOutcome::Completed(Ok(()))
    );
    assert_eq!(native_thread_writer_waits() - before, 1);
}

#[test]
fn a_writer_wait_on_the_native_blocking_pool_is_not_counted() {
    let _serial = NATIVE_WAIT_COUNTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = fixture('a', 'a');
    let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let before = native_thread_writer_waits();
    crate::native_runtime::block_on(crate::native_runtime::spawn_blocking(move || {
        handle.execute_batch("CREATE TABLE blocking_pool (value INTEGER)".to_owned())
    }))
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(native_thread_writer_waits() - before, 0);
}

#[test]
fn a_reader_wait_on_a_native_worker_is_counted() {
    let _serial = NATIVE_WAIT_COUNTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = fixture('a', 'a');
    let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let before = native_thread_reader_waits();
    let task = crate::native_runtime::NativeTask::spawn(async move {
        let rows = handle
            .query(statement("SELECT 1", vec![]), Duration::from_secs(5))
            .map(|rows| rows.rows.len())
            .map_err(|error| error.to_string());
        let snapshot = handle
            .begin_read_snapshot(Duration::from_secs(5))
            .and_then(|snapshot| snapshot.query(statement("SELECT 1", vec![])))
            .map(|rows| rows.rows.len())
            .map_err(|error| error.to_string());
        (rows, snapshot)
    })
    .unwrap();
    let outcome = crate::native_runtime::block_on(task.wait()).unwrap();
    assert_eq!(
        outcome,
        crate::native_runtime::NativeTaskOutcome::Completed((Ok(1), Ok(1)))
    );
    assert_eq!(
        native_thread_reader_waits() - before,
        3,
        "the query, the snapshot open and the snapshot query each parked the worker"
    );
}

#[test]
fn a_reader_wait_on_the_native_blocking_pool_is_not_counted() {
    let _serial = NATIVE_WAIT_COUNTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = fixture('a', 'a');
    let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let before = native_thread_reader_waits();
    let rows = crate::native_runtime::block_on(crate::native_runtime::spawn_blocking(move || {
        handle.query(statement("SELECT 1", vec![]), Duration::from_secs(5))
    }))
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(native_thread_reader_waits() - before, 0);
}

/// Collects the `call_site` of every storage-wait log record, process-wide,
/// because the records are emitted from native worker threads.
struct WaitLogCapture {
    call_sites: Mutex<Vec<(tracing::Level, String)>>,
}

impl WaitLogCapture {
    fn install() -> &'static Self {
        static CAPTURE: std::sync::OnceLock<&'static WaitLogCapture> = std::sync::OnceLock::new();
        CAPTURE.get_or_init(|| {
            let capture: &'static WaitLogCapture = Box::leak(Box::new(WaitLogCapture {
                call_sites: Mutex::new(Vec::new()),
            }));
            tracing::dispatcher::set_global_default(tracing::Dispatch::new(CaptureHandle(capture)))
                .expect("no other test installs a global tracing subscriber");
            capture
        })
    }

    fn records_for(&self, call_site: &str) -> Vec<tracing::Level> {
        self.call_sites
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, site)| site == call_site)
            .map(|(level, _)| *level)
            .collect()
    }
}

struct CaptureHandle(&'static WaitLogCapture);

struct CallSiteVisitor(Option<String>);

impl tracing::field::Visit for CallSiteVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "call_site" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

impl tracing::Subscriber for CaptureHandle {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = CallSiteVisitor(None);
        event.record(&mut visitor);
        if let Some(site) = visitor.0 {
            self.0
                .call_sites
                .lock()
                .unwrap()
                .push((*event.metadata().level(), site));
        }
    }
}

#[test]
fn repeated_reader_waits_from_one_site_are_counted_but_logged_once() {
    let _serial = NATIVE_WAIT_COUNTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let capture = WaitLogCapture::install();
    let fixture = fixture('a', 'a');
    let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let before = native_thread_reader_waits();
    let sites_before = native_thread_storage_wait_sites();
    const READS: u64 = 5;
    let task = crate::native_runtime::NativeTask::spawn(async move {
        let mut rows = 0;
        for _ in 0..READS {
            rows += handle
                .query(statement("SELECT 1", vec![]), Duration::from_secs(5))
                .unwrap()
                .rows
                .len();
        }
        rows
    })
    .unwrap();
    let outcome = crate::native_runtime::block_on(task.wait()).unwrap();
    assert_eq!(
        outcome,
        crate::native_runtime::NativeTaskOutcome::Completed(READS as usize)
    );
    assert_eq!(native_thread_reader_waits() - before, READS);
    let new_sites: Vec<_> = native_thread_storage_wait_sites()
        .into_iter()
        .filter(|site| !sites_before.contains(site))
        .collect();
    assert_eq!(new_sites.len(), 1, "one call site made every read");
    assert_eq!(new_sites[0].hits, READS);
    assert_eq!(
        capture.records_for(&new_sites[0].call_site),
        vec![tracing::Level::WARN],
        "the first hit logs at WARN and later hits log nothing"
    );
}

fn job_count(handle: &ExactSqlHandle) -> i64 {
    match crate::native_runtime::block_on(handle.query_async(
        statement("SELECT COUNT(*) FROM job_rows", vec![]),
        Duration::from_secs(5),
    ))
    .unwrap()
    .unwrap()
    .rows[0]
        .values[0]
    {
        ExactSqlValue::Integer(count) => count,
        ref other => panic!("count is an integer, got {other:?}"),
    }
}

fn job_fixture() -> (Fixture, ExactSqlHandle) {
    let fixture = fixture('a', 'a');
    let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    crate::native_runtime::block_on(
        handle.execute_batch_async("CREATE TABLE job_rows (value INTEGER NOT NULL)".to_owned()),
    )
    .unwrap()
    .unwrap();
    (fixture, handle)
}

#[test]
fn a_write_job_and_an_awaited_read_on_a_native_worker_park_no_native_thread() {
    let _serial = NATIVE_WAIT_COUNTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (_fixture, handle) = job_fixture();
    let writer_waits = native_thread_writer_waits();
    let reader_waits = native_thread_reader_waits();
    let worker_handle = handle.clone();
    let task = crate::native_runtime::NativeTask::spawn(async move {
        let committed = worker_handle
            .transact(|transaction| {
                transaction.execute(statement(
                    "INSERT INTO job_rows (value) VALUES (?1)",
                    vec![ExactSqlValue::Integer(7)],
                ))?;
                transaction
                    .query(statement("SELECT COUNT(*) FROM job_rows", vec![]))
                    .map(|rows| rows.rows.len())
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error: ExactSqlError| error.to_string())?;
        let read = worker_handle
            .query_async(
                statement("SELECT value FROM job_rows", vec![]),
                Duration::from_secs(5),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((committed, read.rows.len()))
    })
    .unwrap();
    let outcome = crate::native_runtime::block_on(task.wait()).unwrap();
    assert_eq!(
        outcome,
        crate::native_runtime::NativeTaskOutcome::Completed(Ok((1, 1)))
    );
    assert_eq!(
        native_thread_writer_waits() - writer_waits,
        0,
        "the job ran on the writer thread without parking the worker"
    );
    assert_eq!(
        native_thread_reader_waits() - reader_waits,
        0,
        "the awaited read ran on the blocking pool"
    );
}

#[test]
fn a_write_job_commits_on_ok_and_rolls_back_on_err() {
    let (_fixture, handle) = job_fixture();
    let insert = |value| {
        statement(
            "INSERT INTO job_rows (value) VALUES (?1)",
            vec![ExactSqlValue::Integer(value)],
        )
    };

    let refused = crate::native_runtime::block_on(handle.transact(move |transaction| {
        transaction
            .execute(insert(1))
            .map_err(|error| error.to_string())?;
        Err::<(), _>("refused after its write".to_owned())
    }))
    .unwrap()
    .unwrap();
    assert_eq!(refused, Err("refused after its write".to_owned()));
    assert_eq!(job_count(&handle), 0, "a refused job leaves no row");

    let committed = crate::native_runtime::block_on(handle.transact(move |transaction| {
        transaction
            .execute(insert(2))
            .map(|result| result.changed_rows)
    }))
    .unwrap()
    .unwrap();
    assert_eq!(committed, Ok(1));
    assert_eq!(job_count(&handle), 1, "an accepted job commits its row");
}

fn insert_job_row(value: i64) -> ExactSqlStatement {
    statement(
        "INSERT INTO job_rows (value) VALUES (?1)",
        vec![ExactSqlValue::Integer(value)],
    )
}

#[test]
fn a_job_body_that_awaits_a_foreign_future_is_refused_and_rolled_back() {
    let (_fixture, handle) = job_fixture();

    let refused = crate::native_runtime::block_on(handle.transact_async(
        JobPolicy::Bounded,
        async |transaction| {
            transaction
                .execute(insert_job_row(1))
                .map_err(|error| error.to_string())?;
            crate::storage_pool::sleep(Duration::from_millis(1)).await;
            Ok::<_, String>(())
        },
    ))
    .unwrap();

    assert_eq!(refused, Err(ExactSqlError::JobAwaitedForeignFuture));
    assert_eq!(job_count(&handle), 0, "the refused job's row rolled back");
}

#[test]
fn an_async_job_body_over_several_statements_commits_and_returns_its_value() {
    let (_fixture, handle) = job_fixture();
    async fn insert(
        transaction: &ExactSqlJobTransaction<'_>,
        value: i64,
    ) -> Result<usize, ExactSqlError> {
        transaction
            .execute(insert_job_row(value))
            .map(|result| result.changed_rows)
    }

    let total = crate::native_runtime::block_on(handle.transact_async(
        JobPolicy::Bounded,
        async |transaction| {
            insert(transaction, 1).await?;
            insert(transaction, 2).await?;
            transaction.execute_batch("INSERT INTO job_rows (value) VALUES (3);".to_owned())?;
            let sum = transaction.query(statement("SELECT SUM(value) FROM job_rows", vec![]))?;
            Ok::<_, ExactSqlError>(sum.rows[0].values[0].clone())
        },
    ))
    .unwrap()
    .unwrap();

    assert_eq!(total, Ok(ExactSqlValue::Integer(6)));
    assert_eq!(
        job_count(&handle),
        3,
        "every statement of the body committed"
    );
}

#[test]
fn a_long_lease_job_renews_its_lease_after_each_statement() {
    let fixture = fixture('a', 'a');
    let clock = FakeClock::frozen();
    let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(Arc::new(FakeClockAuthority {
            clock: Arc::clone(&clock),
            advance: Some((
                ExactSqlWriteIntent::Query,
                EXACT_SQL_TRANSACTION_LIMIT * 2 / 3,
            )),
        }))
        .unwrap();
    let three_reads = |policy| {
        crate::native_runtime::block_on(handle.transact_with_policy(policy, |transaction| {
            (0..3).try_for_each(|_| transaction.query(statement("SELECT 1", vec![])).map(|_| ()))
        }))
        .unwrap()
        .unwrap()
    };

    assert_eq!(three_reads(JobPolicy::AuthorizedLongLease), Ok(()));
    assert!(
        three_reads(JobPolicy::Bounded).is_err(),
        "a bounded job outlives its one lease"
    );
}

#[test]
fn abandoned_commit_retains_completion_until_writer_settlement() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Resource(Arc<AtomicUsize>);
    impl Drop for Resource {
        fn drop(&mut self) {
            self.0.fetch_add(10, Ordering::AcqRel);
        }
    }
    for commit_succeeds in [true, false] {
        current_thread().block_on(async {
            let fixture = fixture('a', 'a');
            let base = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
            base.execute_batch("CREATE TABLE owned_commit (value INTEGER UNIQUE)".to_owned())
                .unwrap();
            let (handle, accepted, release) = gated_handle(&fixture, ExactSqlWriteIntent::Commit);
            let transaction = handle.begin_immediate_async().await.unwrap();
            transaction
                .execute_async(statement("INSERT INTO owned_commit VALUES (1)", vec![]))
                .await
                .unwrap();
            let state = Arc::new(AtomicUsize::new(0));
            let resource = Resource(Arc::clone(&state));
            let mut pending = Box::pin(transaction.commit_async_with_on_commit(move || {
                resource.0.fetch_add(1, Ordering::AcqRel);
                drop(resource);
            }));
            assert!(poll_once(pending.as_mut()).await.is_pending());
            accepted.await.unwrap();
            drop(pending);
            assert_eq!(state.load(Ordering::Acquire), 0);
            if commit_succeeds {
                release.send(()).unwrap();
            } else {
                drop(release);
            }
            base.execute_async(statement("INSERT INTO owned_commit VALUES (2)", vec![]))
                .await
                .unwrap();
            assert_eq!(
                state.load(Ordering::Acquire),
                if commit_succeeds { 11 } else { 10 }
            );
            let rows = base
                .query(
                    statement("SELECT value FROM owned_commit ORDER BY value", vec![]),
                    Duration::from_secs(1),
                )
                .unwrap();
            assert_eq!(rows.rows.len(), if commit_succeeds { 2 } else { 1 });
        });
    }
}
