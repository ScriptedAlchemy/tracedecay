//! Closed native execution seam for validated repository write payloads.

mod validation;

use std::{error::Error, fmt};

use rusqlite::Savepoint;
use tracedecay_domain::ObservationSourceCursorV1;
use tracedecay_store::{
    CursorAdvanceLedgerDisagreementV1, RepositoryWritePayloadV1, RuntimeSubmitRequestV1,
    StorageRuntimeContractErrorV1,
};

/// Effects produced by one request savepoint. They travel with that
/// request's receipt and are reported only after the outer commit succeeds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StorageOperationOutput {
    #[default]
    None,
    ExternalSourceProjection {
        projected: u32,
    },
}

impl StorageOperationOutput {
    pub(crate) fn validate_for(
        self,
        payload: &RepositoryWritePayloadV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        match (self, payload) {
            (
                Self::ExternalSourceProjection { projected },
                RepositoryWritePayloadV1::ExternalSourceProjectionBatch(batch),
            ) if projected <= batch.max() => Ok(()),
            (Self::ExternalSourceProjection { .. }, _)
            | (Self::None, RepositoryWritePayloadV1::ExternalSourceProjectionBatch(_)) => {
                Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                    field: "native operation output",
                })
            }
            (Self::None, _) => Ok(()),
        }
    }
}

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
    ) -> Result<StorageOperationOutput, StorageOperationError> {
        self.execute(savepoint, payload)
            .map(|()| StorageOperationOutput::None)
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
    ) -> Result<StorageOperationOutput, StorageOperationError> {
        (**self).execute_closed(savepoint, payload)
    }
}

pub(crate) fn execute<E: StorageOperationExecutor>(
    savepoint: &Savepoint<'_>,
    request: &RuntimeSubmitRequestV1,
    executor: &mut E,
) -> Result<StorageOperationOutput, StorageOperationError> {
    validation::validate(request).map_err(StorageOperationError::Contract)?;
    let output = executor.execute_closed(savepoint, &request.envelope().payload)?;
    output
        .validate_for(&request.envelope().payload)
        .map_err(StorageOperationError::Contract)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_sql_only_executor_rejects_product_domain_writes() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        let request = crate::test_support::request(crate::test_support::metadata(
            "operation.exact-sql-only",
            "idempotency.exact-sql-only",
            'e',
        ));

        let result =
            ExactSqlOnlyStorageOperationV1.execute(&savepoint, &request.envelope().payload);

        assert!(matches!(result, Err(rusqlite::Error::InvalidQuery)));
    }

    #[test]
    fn projection_output_must_match_its_request() {
        let request = crate::test_support::projection_batch_request(6);
        let batch = &request.envelope().payload;
        for projected in [0, 6] {
            StorageOperationOutput::ExternalSourceProjection { projected }
                .validate_for(batch)
                .unwrap();
        }
        assert!(StorageOperationOutput::None.validate_for(batch).is_err());
        assert!(
            StorageOperationOutput::ExternalSourceProjection { projected: 7 }
                .validate_for(batch)
                .is_err()
        );
        let other = crate::test_support::request(crate::test_support::metadata(
            "operation.other",
            "key.other",
            'o',
        ));
        assert!(
            StorageOperationOutput::ExternalSourceProjection { projected: 0 }
                .validate_for(&other.envelope().payload)
                .is_err()
        );
    }
}
