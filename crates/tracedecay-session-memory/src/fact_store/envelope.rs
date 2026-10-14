//! Canonical project-memory transaction envelopes and durable operation receipts.

use std::future::Future;
use std::pin::Pin;

use asupersync::channel::oneshot;
use asupersync::{Budget, Cx};

use serde_json::Value;
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::{FactEventId, FactId, FactOwnerV1, ProvenanceId, UtcMicros};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_runtime_core::db::{Database, DatabaseMemoryTransaction as Transaction};
use tracedecay_store::{FactStoreError, FactStoreResult, FactWriteControl};

use super::DatabaseFactStore;
use super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, PROJECT_MEMORY_WRITE_OPERATION, QUERY_OPERATION,
    from_json, row_optional_string, row_string, storage_error, storage_message, to_json,
};

/// The runtime retains both the settlement task and its join while the caller
/// holds only a receipt. Dropping that receipt cannot interrupt an admitted
/// commit or its graph publication.
struct FactSettlementReceipt<T> {
    cx: Cx,
    receipt: oneshot::Receiver<FactStoreResult<T>>,
}

pub(in crate::fact_store) fn settle_fact_write<T, F, Fut>(
    operation: &'static str,
    work: F,
) -> Pin<Box<dyn Future<Output = FactStoreResult<T>> + Send>>
where
    T: Send + 'static,
    F: FnOnce(Cx) -> Fut + Send + 'static,
    Fut: Future<Output = FactStoreResult<T>> + Send + 'static,
{
    // `RuntimeHandle` stays in this synchronous admission. Holding it across
    // the caller's await makes every `Send` proof enter `BrowserWorkerPump`'s
    // self-reference (rust-lang/rust#159228).
    let admitted = admit_fact_settlement(operation, work);
    Box::pin(async move {
        let FactSettlementReceipt { cx, mut receipt } = admitted?;
        receipt
            .recv(&cx)
            .await
            .map_err(|error| storage_error(operation, error))?
    })
}

fn admit_fact_settlement<T, F, Fut>(
    operation: &'static str,
    work: F,
) -> FactStoreResult<FactSettlementReceipt<T>>
where
    T: Send + 'static,
    F: FnOnce(Cx) -> Fut + Send + 'static,
    Fut: Future<Output = FactStoreResult<T>> + Send + 'static,
{
    let runtime = tracedecay_runtime_core::native_runtime::handle()
        .ok_or_else(|| storage_message(operation, "native settlement runtime is unavailable"))?;
    let settlement_cx = runtime
        .try_request_cx_with_budget(Budget::INFINITE)
        .map_err(|error| storage_error(operation, error))?;
    let receipt_cx = settlement_cx.clone();
    let (completed, receipt) = oneshot::channel();
    runtime
        .try_spawn_with_cx(move |reaper_cx| async move {
            let outcome = match settlement_cx.spawn(work) {
                Ok(mut task) => task
                    .join(&reaper_cx)
                    .await
                    .map_err(|error| storage_error(operation, error))
                    .and_then(|outcome| outcome),
                Err(error) => Err(storage_error(operation, error)),
            };
            if completed.send(&reaper_cx, outcome).is_err() {
                tracing::trace!(
                    operation,
                    "fact settlement completed after its caller stopped waiting"
                );
            }
        })
        .map_err(|error| storage_error(operation, error))?;
    Ok(FactSettlementReceipt {
        cx: receipt_cx,
        receipt,
    })
}

pub(super) async fn finish_read_snapshot<T>(
    snapshot: Transaction<'_>,
    result: FactStoreResult<T>,
) -> FactStoreResult<T> {
    match result {
        Ok(value) => {
            snapshot
                .commit()
                .await
                .map_err(|error| storage_error(QUERY_OPERATION, error))?;
            Ok(value)
        }
        Err(error) => match snapshot.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(storage_error(
                QUERY_OPERATION,
                std::io::Error::other(format!(
                    "{error}; read snapshot rollback also failed: {rollback}"
                )),
            )),
        },
    }
}

#[derive(Clone)]
pub(super) struct ProjectMemoryOperationReceiptV1 {
    pub(super) fact_id: Option<FactId>,
    pub(super) event_id: Option<FactEventId>,
    pub(super) receipt: Value,
}

pub(super) fn project_memory_digest(material: Value) -> FactStoreResult<String> {
    let encoded = to_json(&material, "serialize project-memory request digest")?;
    Ok(sha256_hex(encoded.as_bytes()))
}

pub(super) async fn project_memory_lookup_operation_receipt_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    operation_id: &ProvenanceId,
    expected_kind: &'static str,
    request_digest: &str,
) -> FactStoreResult<Option<ProjectMemoryOperationReceiptV1>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT operation_kind, request_digest, fact_id, event_id, receipt_json
             FROM memory_v2_operation_receipts
             WHERE owner_kind = ?1
               AND project_id = ?2
               AND operation_id = ?3
             ORDER BY
                recorded_at ASC,
                operation_id ASC
             LIMIT 1",
            params![key.kind, key.project_id.as_str(), operation_id.as_str(),],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    let operation_kind = row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?;
    let stored_digest = row_string(&row, 1, PROJECT_MEMORY_WRITE_OPERATION)?;
    if operation_kind != expected_kind || stored_digest != request_digest {
        return Err(FactStoreError::OperationConflict);
    }
    let fact_id = row_optional_string(&row, 2, PROJECT_MEMORY_WRITE_OPERATION)?
        .map(FactId::new)
        .transpose()
        .map_err(FactStoreError::from)?;
    let event_id = row_optional_string(&row, 3, PROJECT_MEMORY_WRITE_OPERATION)?
        .map(FactEventId::new)
        .transpose()
        .map_err(FactStoreError::from)?;
    if event_id.is_some() && fact_id.is_none() {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "operation receipt has an event without a fact",
        ));
    }
    let receipt = from_json::<Value>(
        &row_string(&row, 4, PROJECT_MEMORY_WRITE_OPERATION)?,
        PROJECT_MEMORY_WRITE_OPERATION,
    )?;
    Ok(Some(ProjectMemoryOperationReceiptV1 {
        fact_id,
        event_id,
        receipt,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn project_memory_record_operation_receipt_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    operation_id: &ProvenanceId,
    operation_kind: &'static str,
    request_digest: &str,
    fact_id: Option<&FactId>,
    event_id: Option<&FactEventId>,
    receipt: &Value,
    recorded_at: UtcMicros,
) -> FactStoreResult<()> {
    if event_id.is_some() && fact_id.is_none() {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "operation receipt cannot reference an event without a fact",
        ));
    }
    let key = OwnerKey::new(owner)?;
    transaction
        .execute(
            "INSERT INTO memory_v2_operation_receipts(
                owner_kind, project_id, operation_id, operation_kind, request_digest,
                fact_id, event_id, receipt_json, recorded_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                key.kind,
                key.project_id.as_str(),
                operation_id.as_str(),
                operation_kind,
                request_digest,
                fact_id.map(FactId::as_str),
                event_id.map(FactEventId::as_str),
                to_json(receipt, "serialize operation receipt")?,
                recorded_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    Ok(())
}

pub(super) fn project_memory_receipt_u64(
    receipt: &Value,
    field: &'static str,
) -> FactStoreResult<u64> {
    receipt.get(field).and_then(Value::as_u64).ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            format!("operation receipt {field} is malformed"),
        )
    })
}

impl DatabaseFactStore<'_> {
    pub(super) async fn project_memory_read<T>(
        &self,
        work: impl for<'tx> FnOnce(
            &'tx Transaction<'_>,
        )
            -> Pin<Box<dyn Future<Output = FactStoreResult<T>> + Send + 'tx>>,
    ) -> FactStoreResult<T> {
        let snapshot = self
            .db
            .begin_memory_read_transaction(PROJECT_MEMORY_READ_OPERATION)
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
        let result = work(&snapshot).await;
        match result {
            Ok(value) => {
                snapshot
                    .commit()
                    .await
                    .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
                Ok(value)
            }
            Err(error) => match snapshot.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(storage_error(
                    PROJECT_MEMORY_READ_OPERATION,
                    std::io::Error::other(format!(
                        "{error}; read snapshot rollback also failed: {rollback}"
                    )),
                )),
            },
        }
    }

    pub(super) async fn project_memory_write<T: Send + 'static>(
        &self,
        write_control: &FactWriteControl,
        graph_source_changed: impl FnOnce(&T) -> bool + Send + 'static,
        work: impl for<'tx> FnOnce(
            &'tx Transaction<'_>,
        )
            -> Pin<Box<dyn Future<Output = FactStoreResult<T>> + Send + 'tx>>
        + Send
        + 'static,
    ) -> FactStoreResult<T> {
        self.project_memory_write_with_barrier_content(
            write_control,
            None,
            graph_source_changed,
            work,
        )
        .await
    }

    /// Like [`Self::project_memory_write`], but names the fact content the
    /// test-transport commit barrier may match against `expect_content`.
    pub(super) async fn project_memory_write_with_barrier_content<T: Send + 'static>(
        &self,
        write_control: &FactWriteControl,
        barrier_content: Option<String>,
        graph_source_changed: impl FnOnce(&T) -> bool + Send + 'static,
        work: impl for<'tx> FnOnce(
            &'tx Transaction<'_>,
        )
            -> Pin<Box<dyn Future<Output = FactStoreResult<T>> + Send + 'tx>>
        + Send
        + 'static,
    ) -> FactStoreResult<T> {
        let db = (*self.db).clone();
        let write_control = write_control.clone();
        settle_fact_write(PROJECT_MEMORY_WRITE_OPERATION, move |cx| async move {
            let result =
                execute_project_memory_write(&cx, db.clone(), write_control, barrier_content, work)
                    .await;
            if result.as_ref().is_ok_and(graph_source_changed) {
                super::graph::publish_project_memory_graph_after_write(&cx, db).await;
            }
            result
        })
        .await
    }
}

async fn execute_project_memory_write<T: Send + 'static>(
    _cx: &Cx,
    db: Database,
    write_control: FactWriteControl,
    _barrier_content: Option<String>,
    work: impl for<'tx> FnOnce(
        &'tx Transaction<'_>,
    ) -> Pin<Box<dyn Future<Output = FactStoreResult<T>> + Send + 'tx>>
    + Send
    + 'static,
) -> FactStoreResult<T> {
    if write_control.interrupted() {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "fact write was interrupted before transaction admission",
        ));
    }
    let transaction = db
        .begin_memory_write_transaction(PROJECT_MEMORY_WRITE_OPERATION)
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let result = work(&transaction).await;
    match result {
        Ok(value) => {
            if !write_control.try_begin_commit() {
                return match transaction.rollback().await {
                    Ok(()) => Err(storage_message(
                        PROJECT_MEMORY_WRITE_OPERATION,
                        "fact write was interrupted before durable commit",
                    )),
                    Err(rollback) => Err(storage_error(
                        PROJECT_MEMORY_WRITE_OPERATION,
                        std::io::Error::other(format!(
                            "fact write was interrupted before durable commit; transaction rollback also failed: {rollback}"
                        )),
                    )),
                };
            }
            transaction
                .commit()
                .await
                .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
            // The mutation is durable from here; the operation is not settled
            // yet. Acceptance harnesses park exactly here to make a budget that
            // expires after the commit point reproducible.
            #[cfg(feature = "test-transport")]
            crate::fact_store::commit_barrier::wait_after_durable_fact_commit(
                _cx,
                _barrier_content.as_deref(),
            )
            .await;
            Ok(value)
        }
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(storage_error(
                PROJECT_MEMORY_WRITE_OPERATION,
                std::io::Error::other(format!(
                    "{error}; transaction rollback also failed: {rollback}"
                )),
            )),
        },
    }
}

#[cfg(test)]
mod write_control_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tempfile::TempDir;

    use tracedecay_runtime_core::db::{DatabaseAuthority, TestDatabaseRuntimeMode};

    use super::*;

    async fn fixture(label: &str) -> (TempDir, Database) {
        let temp = tempfile::tempdir().expect("write-control fixture root");
        let path = temp.path().join(format!("{label}.db"));
        let authority = DatabaseAuthority::acquire_test(&path, "write-control fixture")
            .expect("database authority");
        tracedecay_global_db::register_registered_schema_installer();
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("write-control database");
        (temp, db)
    }

    #[test]
    fn settlement_from_a_foreign_executor_runs_on_a_native_worker() {
        let foreign = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("foreign executor");
        let on_native_worker = foreign
            .block_on(settle_fact_write(
                PROJECT_MEMORY_WRITE_OPERATION,
                |_cx| async { Ok(Cx::current().is_some()) },
            ))
            .expect("settlement through the process runtime");
        assert!(on_native_worker);
    }

    #[asupersync::test]
    async fn dropped_write_waiter_preserves_owned_durable_settlement(cx: &Cx) {
        let (_temp, db) = fixture("dropped-settlement-waiter").await;
        let (entered_tx, mut entered_rx) = oneshot::channel();
        let (release_tx, mut release_rx) = oneshot::channel();
        let (committed_tx, mut committed_rx) = oneshot::channel();
        let control = FactWriteControl::new(Arc::new(|| false), Arc::new(|| true));
        let store = DatabaseFactStore::new(&db);
        {
            let write = store.project_memory_write(
                &control,
                move |()| {
                    committed_tx
                        .send_blocking(())
                        .expect("publish committed write");
                    false
                },
                move |transaction| {
                    Box::pin(async move {
                        let work_cx = Cx::current().expect("owned native write context");
                        entered_tx
                            .send(&work_cx, ())
                            .expect("publish admitted write");
                        release_rx
                            .recv(&work_cx)
                            .await
                            .expect("release admitted write");
                        transaction
                            .execute_batch(
                                "CREATE TABLE settlement_probe(value TEXT NOT NULL);
                         INSERT INTO settlement_probe(value) VALUES('durable');",
                            )
                            .await
                            .map_err(|error| {
                                storage_error(PROJECT_MEMORY_WRITE_OPERATION, error)
                            })?;
                        Ok(())
                    })
                },
            );
            let mut write = std::pin::pin!(write);
            let mut entered = std::pin::pin!(entered_rx.recv(cx));
            asupersync::time::timeout(
                cx.now(),
                std::time::Duration::from_secs(5),
                std::future::poll_fn(|task| {
                    assert!(
                        write.as_mut().poll(task).is_pending(),
                        "write settled before release"
                    );
                    entered.as_mut().poll(task)
                }),
            )
            .await
            .expect("write admission timed out")
            .expect("write admission");
        }
        release_tx
            .send(cx, ())
            .expect("release accepted work after waiter drop");
        asupersync::time::timeout(
            cx.now(),
            std::time::Duration::from_secs(5),
            committed_rx.recv(cx),
        )
        .await
        .expect("owned settlement timed out")
        .expect("commit observed");
        let snapshot = db
            .begin_memory_read_transaction("verify owned settlement")
            .await
            .expect("read snapshot");
        let mut rows = snapshot
            .query("SELECT value FROM settlement_probe", ())
            .await
            .expect("read durable value");
        let row = rows.next().await.expect("read row").expect("committed row");
        assert_eq!(row.get::<String>(0).expect("value"), "durable");
        snapshot.commit().await.expect("finish snapshot");
    }

    #[asupersync::test]
    async fn interruption_before_admission_never_runs_write_work() {
        let (_temp, db) = fixture("pre-admission").await;
        let work_ran = Arc::new(AtomicBool::new(false));
        let work_ran_in_task = Arc::clone(&work_ran);
        let control = FactWriteControl::new(Arc::new(|| true), Arc::new(|| true));

        let result: FactStoreResult<()> = DatabaseFactStore::new(&db)
            .project_memory_write(
                &control,
                |()| true,
                move |_| {
                    Box::pin(async move {
                        work_ran_in_task.store(true, Ordering::Release);
                        Ok(())
                    })
                },
            )
            .await;

        assert!(matches!(result, Err(FactStoreError::Storage { .. })));
        assert!(!work_ran.load(Ordering::Acquire));
    }

    #[asupersync::test]
    async fn denied_commit_gate_rolls_back_transaction() {
        let (_temp, db) = fixture("gate-rollback").await;
        let gate_polled = Arc::new(AtomicBool::new(false));
        let gate_polled_in_task = Arc::clone(&gate_polled);
        let control = FactWriteControl::new(
            Arc::new(|| false),
            Arc::new(move || {
                gate_polled_in_task.store(true, Ordering::Release);
                false
            }),
        );

        let result: FactStoreResult<()> = DatabaseFactStore::new(&db)
            .project_memory_write(
                &control,
                |()| true,
                |transaction| {
                    Box::pin(async move {
                        transaction
                            .execute_batch(
                                "CREATE TABLE write_control_probe(value TEXT NOT NULL);
                                 INSERT INTO write_control_probe(value) VALUES('uncommitted');",
                            )
                            .await
                            .map_err(|error| {
                                storage_error(PROJECT_MEMORY_WRITE_OPERATION, error)
                            })?;
                        Ok(())
                    })
                },
            )
            .await;

        assert!(matches!(result, Err(FactStoreError::Storage { .. })));
        assert!(gate_polled.load(Ordering::Acquire));
        let snapshot = db
            .begin_memory_read_transaction("verify write-control rollback")
            .await
            .expect("read snapshot");
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'write_control_probe'",
                (),
            )
            .await
            .expect("query table authority");
        let row = rows
            .next()
            .await
            .expect("read table row")
            .expect("table row");
        assert_eq!(row.get::<i64>(0).expect("table count"), 0);
        drop(rows);
        snapshot.commit().await.expect("finish read snapshot");
    }
}
