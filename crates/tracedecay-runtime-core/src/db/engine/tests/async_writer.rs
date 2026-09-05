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
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl ExactSqlWriteAuthority for WriteGate {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if intent == ExactSqlWriteIntent::Execute
            && self
                .remaining
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
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
    tokio::sync::mpsc::UnboundedReceiver<()>,
    mpsc::Sender<()>,
) {
    let directory = tempfile::TempDir::new().unwrap();
    let (entered, accepted) = tokio::sync::mpsc::unbounded_channel();
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (_directory, connection, _gate, _accepted, _release) = fixture().await;
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, held) = mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            held.recv().unwrap();
        });
        ready.await.unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            connection.execute("INSERT INTO items VALUES (1, 1)", ()),
        )
        .await;
        release.send(()).unwrap();
        blocker.await.unwrap();
        assert_eq!(
            result
                .expect("writer acknowledgement must bypass occupied blocking pool")
                .unwrap(),
            1
        );
    });
}

#[tokio::test]
async fn cancelled_statement_batch_retains_work_and_writer_interleaving() {
    let (_directory, connection, gate, mut accepted, release) = fixture().await;
    gate.remaining.store(2, Ordering::Release);
    let caller_connection = Arc::clone(&connection);
    let caller = tokio::spawn(async move {
        caller_connection
            .execute_statements(vec![
                WriteStatement::new("INSERT INTO items(value) VALUES (1)", ()).unwrap(),
                WriteStatement::new("INSERT INTO items(value) VALUES (2)", ()).unwrap(),
                WriteStatement::new("INSERT INTO items(value) VALUES (3)", ()).unwrap(),
            ])
            .await
    });
    accepted.recv().await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    accepted.recv().await.unwrap();
    let mut competitor = Box::pin(connection.execute("INSERT INTO items(value) VALUES (9)", ()));
    assert!(
        poll_fn(|cx| Poll::Ready(competitor.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    release.send(()).unwrap();
    competitor.await.unwrap();
    let values = tokio::time::timeout(Duration::from_secs(1), async {
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
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(values, vec![1, 2, 9, 3]);
}

#[tokio::test]
async fn cancelled_transaction_call_keeps_serialization_until_acknowledgement() {
    let (_directory, connection, gate, mut accepted, release) = fixture().await;
    let transaction = Arc::new(connection.transaction().await.unwrap());
    gate.remaining.store(1, Ordering::Release);
    let first_transaction = Arc::clone(&transaction);
    let first = tokio::spawn(async move {
        first_transaction
            .execute("INSERT INTO items VALUES (1, 1)", ())
            .await
    });
    accepted.recv().await.unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let mut pending = tokio::task::JoinSet::new();
    for id in [2, 3] {
        let transaction = Arc::clone(&transaction);
        pending.spawn(async move {
            transaction
                .execute("INSERT INTO items VALUES (?1, ?1)", [id])
                .await
        });
    }
    // Poll both callers while the first command still owns the actor. Their
    // retained operations queue on the transaction gate, never on a full slot.
    tokio::task::yield_now().await;
    release.send(()).unwrap();
    while let Some(result) = pending.join_next().await {
        assert_eq!(result.unwrap().unwrap(), 1);
    }
    let transaction = match Arc::try_unwrap(transaction) {
        Ok(transaction) => transaction,
        Err(_) => panic!("all transaction callers completed"),
    };
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
