use asupersync::{
    Cx,
    channel::{mpsc as async_mpsc, oneshot},
    runtime::RuntimeBuilder,
};
use std::{
    future::{Future, poll_fn},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    task::Poll,
    time::Duration,
};

use crate::db::engine::{TestConnection, WriteStatement};
use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlError, ExactSqlWriteAuthority, ExactSqlWriteIntent,
};

struct WriteGate {
    remaining: AtomicUsize,
    entered: async_mpsc::UnboundedSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl ExactSqlWriteAuthority for WriteGate {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if intent == ExactSqlWriteIntent::Execute
            && self
                .remaining
                .try_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
        {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv()
                .map_err(|_| ExactSqlError::AuthorityDenied("fixture gate closed".to_owned()))?;
        }
        Ok(())
    }
}

async fn fixture() -> (
    tempfile::TempDir,
    Arc<TestConnection>,
    Arc<WriteGate>,
    async_mpsc::UnboundedReceiver<()>,
    mpsc::Sender<()>,
) {
    let directory = tempfile::TempDir::new().unwrap();
    let (entered, accepted) = async_mpsc::unbounded_channel();
    let (release, receive) = mpsc::channel();
    let gate = Arc::new(WriteGate {
        remaining: AtomicUsize::new(0),
        entered,
        release: Mutex::new(receive),
    });
    let connection = Arc::new(TestConnection::open_with_write_authority(
        &directory.path().join("writer.sqlite3"),
        gate.clone(),
    ));
    connection
        .execute(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)",
            (),
        )
        .await
        .unwrap();
    (directory, connection, gate, accepted, release)
}

#[test]
fn ordinary_writer_progress_does_not_require_a_blocking_pool_slot() {
    let runtime = RuntimeBuilder::current_thread()
        .blocking_threads(1, 1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (_directory, connection, _gate, _accepted, _release) = fixture().await;
        let cx = Cx::current().unwrap();
        let (entered, mut ready) = oneshot::channel();
        let (release, held) = mpsc::channel();
        let mut blocker = cx
            .spawn_blocking(move |_cx| {
                entered.send_blocking(()).unwrap();
                held.recv().unwrap();
            })
            .unwrap();
        ready.recv(&cx).await.unwrap();
        let result = asupersync::time::timeout(
            Cx::current().unwrap().now(),
            Duration::from_secs(1),
            connection.execute("INSERT INTO items VALUES (1, 1)", ()),
        )
        .await;
        release.send(()).unwrap();
        blocker.join(&cx).await.unwrap();
        assert_eq!(
            result
                .expect("writer acknowledgement must bypass occupied blocking pool")
                .unwrap(),
            1
        );
    });
}

#[asupersync::test]
async fn cancelled_statement_batch_retains_work_and_writer_interleaving() {
    let (_directory, connection, gate, mut accepted, release) = fixture().await;
    gate.remaining.store(2, Ordering::Release);
    let caller_connection = Arc::clone(&connection);
    let mut caller = Box::pin(async move {
        caller_connection
            .execute_statements(vec![
                WriteStatement::new("INSERT INTO items(value) VALUES (1)", ()).unwrap(),
                WriteStatement::new("INSERT INTO items(value) VALUES (2)", ()).unwrap(),
                WriteStatement::new("INSERT INTO items(value) VALUES (3)", ()).unwrap(),
            ])
            .await
    });
    assert!(
        poll_fn(|context| Poll::Ready(caller.as_mut().poll(context)))
            .await
            .is_pending()
    );
    accepted.recv(&Cx::current().unwrap()).await.unwrap();
    drop(caller);
    release.send(()).unwrap();
    accepted.recv(&Cx::current().unwrap()).await.unwrap();
    let mut competitor = Box::pin(connection.execute("INSERT INTO items(value) VALUES (9)", ()));
    assert!(
        poll_fn(|cx| Poll::Ready(competitor.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    release.send(()).unwrap();
    competitor.await.unwrap();
    let values = asupersync::time::timeout(
        Cx::current().unwrap().now(),
        Duration::from_secs(1),
        async {
            loop {
                let mut rows = connection
                    .query("SELECT value FROM items ORDER BY id", ())
                    .await
                    .unwrap();
                let mut values = Vec::new();
                while let Some(row) = rows.next().await.unwrap() {
                    values.push(row.get::<i64>(0).unwrap());
                }
                if values.len() == 4 {
                    break values;
                }
                asupersync::runtime::yield_now().await;
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(values, vec![1, 2, 9, 3]);
}

#[asupersync::test]
async fn cancelled_transaction_call_keeps_serialization_until_acknowledgement() {
    let (_directory, connection, gate, mut accepted, release) = fixture().await;
    let transaction = Arc::new(connection.transaction().await.unwrap());
    gate.remaining.store(1, Ordering::Release);
    let first_transaction = Arc::clone(&transaction);
    let mut first = Box::pin(async move {
        first_transaction
            .execute("INSERT INTO items VALUES (1, 1)", ())
            .await
    });
    assert!(
        poll_fn(|context| Poll::Ready(first.as_mut().poll(context)))
            .await
            .is_pending()
    );
    accepted.recv(&Cx::current().unwrap()).await.unwrap();
    drop(first);
    let cx = Cx::current().unwrap();
    let mut pending = Vec::new();
    for id in [2, 3] {
        let transaction = Arc::clone(&transaction);
        pending.push(
            cx.spawn(move |_cx| async move {
                transaction
                    .execute("INSERT INTO items VALUES (?1, ?1)", [id])
                    .await
            })
            .unwrap(),
        );
    }
    // Poll both callers while the first command still owns the actor. Their
    // retained operations queue on the transaction gate, never on a full slot.
    asupersync::runtime::yield_now().await;
    release.send(()).unwrap();
    for mut task in pending {
        assert_eq!(task.join(&cx).await.unwrap().unwrap(), 1);
    }
    let transaction = Arc::into_inner(transaction).expect("all transaction callers completed");
    transaction.commit().await.unwrap();
    let mut rows = connection
        .query("SELECT COUNT(*) FROM items", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        3
    );
}

#[asupersync::test]
async fn cancelled_caller_waiting_on_the_transaction_gate_keeps_its_operation() {
    let (_directory, connection, gate, mut accepted, release) = fixture().await;
    let transaction = Arc::new(connection.transaction().await.unwrap());
    gate.remaining.store(1, Ordering::Release);
    let holder = Arc::clone(&transaction);
    let mut first =
        Box::pin(async move { holder.execute("INSERT INTO items VALUES (1, 1)", ()).await });
    assert!(
        poll_fn(|context| Poll::Ready(first.as_mut().poll(context)))
            .await
            .is_pending()
    );
    let cx = Cx::current().unwrap();
    accepted.recv(&cx).await.unwrap();
    let waiter = Arc::clone(&transaction);
    let mut queued = cx
        .spawn(
            move |_cx| async move { waiter.execute("INSERT INTO items VALUES (2, 2)", ()).await },
        )
        .unwrap();
    // The spawned caller reaches the gate and queues behind the first command.
    asupersync::runtime::yield_now().await;
    queued.abort();
    asupersync::runtime::yield_now().await;
    release.send(()).unwrap();
    assert_eq!(first.await.unwrap(), 1);
    let _ = queued.join(&cx).await;
    let transaction = Arc::into_inner(transaction).expect("all transaction callers completed");
    transaction.commit().await.unwrap();
    let mut rows = connection
        .query("SELECT id FROM items ORDER BY id", ())
        .await
        .unwrap();
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        ids.push(row.get::<i64>(0).unwrap());
    }
    assert_eq!(
        ids,
        vec![1, 2],
        "an operation accepted before its caller was cancelled must still run"
    );
}
