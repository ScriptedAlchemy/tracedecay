//! Stateless bridge from the writer's request savepoint to native execution
//! and ledger persistence.

use rusqlite::{Savepoint, Transaction};
use tracedecay_store::{
    CorruptionClassV1, IdempotencyIdentityV1, RuntimeSubmitRequestV1, StorageRuntimeErrorV1,
    StoreCommitReceiptV1, StoreRuntimeBindingV1,
};

use crate::{
    ledger::{self, LedgerDisposition, LedgerError},
    operation::{self, StorageOperationError, StorageOperationExecutor},
    writer::WriterPersistence,
};

pub(crate) struct RuntimeWriterPersistence<E> {
    executor: E,
}

impl<E> RuntimeWriterPersistence<E> {
    pub(crate) const fn new(executor: E) -> Self {
        Self { executor }
    }
}

impl<E> WriterPersistence for RuntimeWriterPersistence<E>
where
    E: StorageOperationExecutor + Send + 'static,
{
    fn lookup_idempotency(
        &mut self,
        transaction: &Transaction<'_>,
        binding: &StoreRuntimeBindingV1,
        idempotency: &IdempotencyIdentityV1,
    ) -> Result<Option<StoreCommitReceiptV1>, StorageRuntimeErrorV1> {
        ledger::initialize_schema(transaction).map_err(map_ledger_error)?;
        ledger::lookup_receipt(transaction, binding, idempotency).map_err(map_ledger_error)
    }

    fn apply_and_record(
        &mut self,
        savepoint: &mut Savepoint<'_>,
        binding: &StoreRuntimeBindingV1,
        request: &RuntimeSubmitRequestV1,
    ) -> Result<StoreCommitReceiptV1, StorageRuntimeErrorV1> {
        if binding != request.binding() {
            return Err(infrastructure("apply-and-record binding mismatch"));
        }
        operation::execute(savepoint, request, &mut self.executor).map_err(map_operation_error)?;
        match ledger::record_runtime_commit(
            savepoint,
            &request.envelope().metadata,
            request.transaction_scope(),
            &request.envelope().payload,
        )
        .map_err(map_ledger_error)?
        {
            LedgerDisposition::Committed(receipt) => Ok(receipt),
            LedgerDisposition::Replay(receipt) | LedgerDisposition::Conflict(receipt) => {
                drop(receipt);
                Err(infrastructure(
                    "idempotency disposition changed after the writer lookup",
                ))
            }
            LedgerDisposition::New => Err(infrastructure(
                "runtime ledger did not record the applied operation",
            )),
        }
    }
}

fn map_ledger_error(error: LedgerError) -> StorageRuntimeErrorV1 {
    match error {
        LedgerError::Corrupt { .. } => StorageRuntimeErrorV1::Corrupt {
            class: CorruptionClassV1::Authoritative,
        },
        error => infrastructure(format!("runtime ledger: {error}")),
    }
}

fn map_operation_error(error: StorageOperationError) -> StorageRuntimeErrorV1 {
    infrastructure(format!("closed native operation: {error}"))
}

fn infrastructure(operation: impl Into<String>) -> StorageRuntimeErrorV1 {
    StorageRuntimeErrorV1::Infrastructure {
        operation: operation.into(),
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tracedecay_store::RepositoryWritePayloadV1;

    use super::*;
    use crate::test_support::{metadata, request};

    #[derive(Default)]
    struct MarkerExecutor;

    impl StorageOperationExecutor for MarkerExecutor {
        fn execute(
            &mut self,
            savepoint: &Savepoint<'_>,
            _payload: &RepositoryWritePayloadV1,
        ) -> rusqlite::Result<()> {
            savepoint.execute_batch(
                "CREATE TABLE IF NOT EXISTS operation_marker (value INTEGER NOT NULL)",
            )?;
            savepoint.execute("INSERT INTO operation_marker(value) VALUES (1)", [])?;
            Ok(())
        }
    }

    #[test]
    fn apply_and_record_returns_the_receipt_from_the_same_savepoint() {
        let mut connection = Connection::open_in_memory().unwrap();
        let request = request(metadata("operation.atomic", "key.atomic", 'a'));
        let binding = request.binding().clone();
        let effect_id = match &request.envelope().payload {
            RepositoryWritePayloadV1::EnqueueOutbox(entry) => entry.identity.effect_id.clone(),
            _ => unreachable!(),
        };
        let mut first = RuntimeWriterPersistence::new(MarkerExecutor);
        let mut transaction = connection.transaction().unwrap();
        assert!(
            first
                .lookup_idempotency(
                    &transaction,
                    &binding,
                    &request.envelope().metadata.idempotency,
                )
                .unwrap()
                .is_none()
        );
        let mut savepoint = transaction.savepoint().unwrap();
        let receipt = first
            .apply_and_record(&mut savepoint, &binding, &request)
            .unwrap();
        assert_eq!(
            receipt.commit_sequence,
            tracedecay_store::CommitSequenceV1(1)
        );
        assert_eq!(
            ledger::lookup_receipt(
                &savepoint,
                &binding,
                &request.envelope().metadata.idempotency,
            )
            .unwrap(),
            Some(receipt.clone())
        );
        assert_eq!(
            ledger::outbox_entry(&savepoint, &binding, &effect_id)
                .unwrap()
                .unwrap()
                .identity
                .effect_id,
            effect_id
        );
        savepoint.rollback().unwrap();
        drop(savepoint);
        transaction.commit().unwrap();

        let marker_exists: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'operation_marker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker_exists, 0);
    }
}
