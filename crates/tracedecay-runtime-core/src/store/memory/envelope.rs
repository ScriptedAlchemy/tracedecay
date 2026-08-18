//! Canonical project-memory transaction envelopes and durable operation receipts.

use std::future::Future;
use std::pin::Pin;

use crate::db::engine::params;
use crate::db::{Database, DatabaseMemoryTransaction as Transaction};
use serde_json::Value;
use sha2::{Digest, Sha256};

use tracedecay_domain::{FactEventId, FactId, FactOwnerV1, ProvenanceId, UtcMicros};
use tracedecay_store::{FactStoreError, FactStoreResult, FactWriteControl};

use super::DatabaseFactStore;
use super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, PROJECT_MEMORY_WRITE_OPERATION, QUERY_OPERATION,
    from_json, row_optional_string, row_string, storage_error, storage_message, to_json,
};

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
    Ok(hex::encode(Sha256::digest(encoded.as_bytes())))
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
        let db = (*self.db).clone();
        let write_control = write_control.clone();
        // Dropping the caller's future detaches this owned task. The control
        // can still deny the write before its commit-start transition; after
        // that transition the bounded transaction commit runs to completion.
        tokio::spawn(async move {
            let result = execute_project_memory_write(db.clone(), write_control, work).await;
            if result.as_ref().is_ok_and(graph_source_changed) {
                super::graph::publish_project_memory_graph_after_write(db).await;
            }
            result
        })
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    }
}

async fn execute_project_memory_write<T: Send + 'static>(
    db: Database,
    write_control: FactWriteControl,
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
            crate::store::memory::commit_barrier::wait_after_durable_fact_commit().await;
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

    use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};

    use super::*;

    async fn fixture(label: &str) -> (TempDir, Database) {
        let temp = tempfile::tempdir().expect("write-control fixture root");
        let path = temp.path().join(format!("{label}.db"));
        let authority = DatabaseAuthority::acquire_test(&path, "write-control fixture")
            .expect("database authority");
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("write-control database");
        (temp, db)
    }

    #[tokio::test]
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

    #[tokio::test]
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
