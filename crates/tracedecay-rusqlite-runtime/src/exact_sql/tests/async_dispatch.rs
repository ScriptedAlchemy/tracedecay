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
