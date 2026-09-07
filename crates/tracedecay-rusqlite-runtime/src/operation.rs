//! Closed native execution seam for validated repository write payloads.

mod validation;

use std::{error::Error, fmt};

use rusqlite::Savepoint;
use tracedecay_domain::ObservationSourceCursorV1;
use tracedecay_store::{
    CursorAdvanceLedgerDisagreementV1, OutboxEffectStateV1, RepositoryWritePayloadV1,
    RuntimeSubmitRequestV1, StorageRuntimeContractErrorV1, TransactionalInboxReceiptV1,
    TransactionalOutboxEntryV1,
};

/// Executes one store-owned payload through the writer's request savepoint.
///
/// The closed payload enum is the dispatch authority. Implementors do not echo
/// receipt material, outbox data, byte estimates, or result digests back to the
/// runtime; the validated request and ledger already own those values.
pub trait StorageOperationExecutor {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()>;

    fn enqueue_outbox(
        &mut self,
        savepoint: &Savepoint<'_>,
        entry: &TransactionalOutboxEntryV1,
    ) -> rusqlite::Result<()> {
        if entry.state == OutboxEffectStateV1::Pending {
            self.execute(
                savepoint,
                &RepositoryWritePayloadV1::EnqueueOutbox(Box::new(entry.clone())),
            )
        } else {
            Ok(())
        }
    }

    fn apply_inbox(
        &mut self,
        savepoint: &Savepoint<'_>,
        entry: &TransactionalOutboxEntryV1,
    ) -> rusqlite::Result<()> {
        self.execute(
            savepoint,
            &RepositoryWritePayloadV1::ApplyInbox(Box::new(entry.clone())),
        )
    }

    fn acknowledge_outbox(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _receipt: &TransactionalInboxReceiptV1,
    ) -> rusqlite::Result<()> {
        Ok(())
    }

    fn execute_closed(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> Result<(), StorageOperationError> {
        self.execute(savepoint, payload)
            .map_err(StorageOperationError::Native)
    }
}

#[derive(Debug)]
pub enum StorageOperationError {
    Contract(StorageRuntimeContractErrorV1),
    Native(rusqlite::Error),
    ObservationSourceCursorConflict {
        expected: Box<Option<ObservationSourceCursorV1>>,
        actual: Box<Option<ObservationSourceCursorV1>>,
    },
    CursorAdvanceLedgerDisagreement {
        disagreement: Box<CursorAdvanceLedgerDisagreementV1>,
    },
}

impl fmt::Display for StorageOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(error) => write!(formatter, "invalid native storage operation: {error}"),
            Self::Native(error) => write!(formatter, "native SQLite operation failed: {error}"),
            Self::ObservationSourceCursorConflict { expected, actual } => write!(
                formatter,
                "observation source cursor conflict: expected {expected:?}, found {actual:?}"
            ),
            Self::CursorAdvanceLedgerDisagreement { .. } => formatter
                .write_str("observation cursor-advance ledger disagrees with immutable coverage"),
        }
    }
}

impl Error for StorageOperationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Contract(error) => Some(error),
            Self::Native(error) => Some(error),
            Self::ObservationSourceCursorConflict { .. }
            | Self::CursorAdvanceLedgerDisagreement { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for StorageOperationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Native(error)
    }
}

pub(crate) fn execute<E: StorageOperationExecutor>(
    savepoint: &Savepoint<'_>,
    request: &RuntimeSubmitRequestV1,
    executor: &mut E,
) -> Result<(), StorageOperationError> {
    validation::validate(request).map_err(StorageOperationError::Contract)?;
    match &request.envelope().payload {
        RepositoryWritePayloadV1::EnqueueOutbox(entry) => executor
            .enqueue_outbox(savepoint, entry)
            .map_err(StorageOperationError::Native),
        RepositoryWritePayloadV1::ApplyInbox(entry) => executor
            .apply_inbox(savepoint, entry)
            .map_err(StorageOperationError::Native),
        RepositoryWritePayloadV1::AcknowledgeOutbox(receipt) => executor
            .acknowledge_outbox(savepoint, receipt)
            .map_err(StorageOperationError::Native),
        payload => executor.execute_closed(savepoint, payload),
    }
}
