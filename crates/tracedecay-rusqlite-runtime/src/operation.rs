//! Closed native execution seam for validated repository write payloads.

mod validation;

use std::{error::Error, fmt};

use rusqlite::Savepoint;
use tracedecay_domain::ObservationSourceCursorV1;
use tracedecay_store::{
    CursorAdvanceLedgerDisagreementV1, RepositoryWritePayloadV1, RuntimeSubmitRequestV1,
    StorageRuntimeContractErrorV1,
};

/// Executes one store-owned payload through the writer's request savepoint.
///
/// The closed payload enum is the dispatch authority. Implementors do not echo
/// receipt material, byte estimates, or result digests back to the runtime; the
/// validated request and ledger already own those values.
pub trait StorageOperationExecutor {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()>;

    fn execute_closed(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> Result<(), StorageOperationError> {
        self.execute(savepoint, payload)
            .map_err(StorageOperationError::Native)
    }
}

/// Writer executor for isolated runtimes whose writes are performed through
/// the bounded exact-SQL channel.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExactSqlOnlyStorageOperationV1;

impl StorageOperationExecutor for ExactSqlOnlyStorageOperationV1 {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        Err(rusqlite::Error::InvalidQuery)
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
    ObservationCursorAdvanceCollision,
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
            Self::ObservationCursorAdvanceCollision => formatter
                .write_str("observation cursor advance receipt collided with different contents"),
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
            | Self::ObservationCursorAdvanceCollision
            | Self::CursorAdvanceLedgerDisagreement { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for StorageOperationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Native(error)
    }
}

impl StorageOperationExecutor for Box<dyn StorageOperationExecutor + Send> {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        (**self).execute(savepoint, payload)
    }

    fn execute_closed(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> Result<(), StorageOperationError> {
        (**self).execute_closed(savepoint, payload)
    }
}

pub(crate) fn execute<E: StorageOperationExecutor>(
    savepoint: &Savepoint<'_>,
    request: &RuntimeSubmitRequestV1,
    executor: &mut E,
) -> Result<(), StorageOperationError> {
    validation::validate(request).map_err(StorageOperationError::Contract)?;
    executor.execute_closed(savepoint, &request.envelope().payload)
}
