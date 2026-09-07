use std::fmt;

use tracedecay_store::{StorageRuntimeContractErrorV1, StoreAuthorityEpochV1};

#[derive(Debug)]
pub(crate) enum LedgerError {
    Sqlite(rusqlite::Error),
    InvalidRequest(StorageRuntimeContractErrorV1),
    Encoding {
        value: &'static str,
    },
    Corrupt {
        table: &'static str,
        field: &'static str,
    },
    UnsupportedInteger {
        field: &'static str,
    },
    StaleAuthority {
        persisted: StoreAuthorityEpochV1,
        requested: StoreAuthorityEpochV1,
    },
    SequenceExhausted,
    ConcurrentCheckpointUpdate,
    ConcurrentIdempotencyWrite,
    OutboxEffectConflict,
    ReplayBindingMismatch {
        field: &'static str,
    },
    OutboxRequiresFullDurability,
    OutboxSourceWatermarkMismatch,
}

impl fmt::Display for LedgerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => {
                write!(formatter, "runtime ledger SQLite operation failed: {error}")
            }
            Self::InvalidRequest(error) => {
                write!(formatter, "invalid runtime ledger request: {error}")
            }
            Self::Encoding { value } => write!(formatter, "could not canonically encode {value}"),
            Self::Corrupt { table, field } => {
                write!(
                    formatter,
                    "runtime ledger row is corrupt in {table}.{field}"
                )
            }
            Self::UnsupportedInteger { field } => {
                write!(
                    formatter,
                    "runtime ledger cannot represent {field} in SQLite"
                )
            }
            Self::StaleAuthority {
                persisted,
                requested,
            } => write!(
                formatter,
                "runtime ledger rejects stale writer authority epoch {requested:?}; persisted epoch is {persisted:?}"
            ),
            Self::SequenceExhausted => {
                formatter.write_str("runtime ledger commit sequence exhausted")
            }
            Self::ConcurrentCheckpointUpdate => {
                formatter.write_str("runtime ledger checkpoint changed during commit")
            }
            Self::ConcurrentIdempotencyWrite => {
                formatter.write_str("runtime ledger idempotency record changed during commit")
            }
            Self::OutboxEffectConflict => {
                formatter.write_str("runtime ledger outbox effect identity already exists")
            }
            Self::ReplayBindingMismatch { field } => {
                write!(
                    formatter,
                    "runtime ledger replay binding mismatched at {field}"
                )
            }
            Self::OutboxRequiresFullDurability => {
                formatter.write_str("runtime ledger outbox records require full durability")
            }
            Self::OutboxSourceWatermarkMismatch => formatter
                .write_str("runtime ledger outbox source watermark does not bind to its receipt"),
        }
    }
}

impl std::error::Error for LedgerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::InvalidRequest(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for LedgerError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
