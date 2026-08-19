use std::{error::Error, fmt};

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension};

use super::types::{CheckpointMode, CheckpointReport, WalSample};

const WAL_HEADER_BYTES: u64 = 32;
const WAL_FRAME_HEADER_BYTES: u64 = 24;

/// Narrow physical-driver seam used by the writer-owned policy.
///
/// Implementations may configure and checkpoint only their already-open writer
/// connection. There is deliberately no path, open, close, delete, scheduling,
/// transaction, or arbitrary-SQL capability in this interface.
pub(crate) trait CheckpointDriver {
    type Error;

    fn disable_auto_checkpoint(&mut self) -> Result<(), Self::Error>;
    fn sample_wal(&mut self) -> Result<WalSample, Self::Error>;
    fn checkpoint(&mut self, mode: CheckpointMode) -> Result<CheckpointReport, Self::Error>;
}

#[derive(Debug)]
pub(crate) enum RusqliteCheckpointError {
    Sqlite(rusqlite::Error),
}

impl fmt::Display for RusqliteCheckpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for RusqliteCheckpointError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
        }
    }
}

impl From<rusqlite::Error> for RusqliteCheckpointError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

pub(crate) struct RusqliteCheckpointDriver {
    connection: Connection,
}

impl RusqliteCheckpointDriver {
    pub(crate) fn new(connection: Connection) -> Self {
        Self { connection }
    }

    pub(super) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

impl CheckpointDriver for RusqliteCheckpointDriver {
    type Error = RusqliteCheckpointError;

    fn disable_auto_checkpoint(&mut self) -> Result<(), Self::Error> {
        self.connection
            .pragma_update(None, "wal_autocheckpoint", 0_i64)
            .map_err(Into::into)
    }

    fn sample_wal(&mut self) -> Result<WalSample, Self::Error> {
        let (_, frames, _) = self.checkpoint_row("PRAGMA wal_checkpoint(NOOP)")?;
        let page_size = self
            .connection
            .pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0))
            .map_err(RusqliteCheckpointError::Sqlite)
            .and_then(|value| {
                nonnegative_integer(value, 0).map_err(RusqliteCheckpointError::Sqlite)
            })?;
        let frame_bytes = page_size.saturating_add(WAL_FRAME_HEADER_BYTES);
        let bytes = if frames > 0 {
            frames
                .saturating_mul(frame_bytes)
                .saturating_add(WAL_HEADER_BYTES)
        } else {
            0
        };
        Ok(WalSample { frames, bytes })
    }

    fn checkpoint(&mut self, mode: CheckpointMode) -> Result<CheckpointReport, Self::Error> {
        let sql = match mode {
            CheckpointMode::Passive => "PRAGMA wal_checkpoint(PASSIVE)",
            CheckpointMode::Restart => "PRAGMA wal_checkpoint(RESTART)",
            CheckpointMode::Truncate => "PRAGMA wal_checkpoint(TRUNCATE)",
        };
        let row = self.checkpoint_row(sql)?;
        Ok(CheckpointReport {
            busy: row.0 != 0,
            log_frames: row.1,
            checkpointed_frames: row.2,
        })
    }
}

impl RusqliteCheckpointDriver {
    fn checkpoint_row(&self, sql: &str) -> Result<(i64, u64, u64), RusqliteCheckpointError> {
        let row = self
            .connection
            .query_row(sql, [], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .optional()
            .map_err(RusqliteCheckpointError::Sqlite)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)
            .map_err(RusqliteCheckpointError::Sqlite)?;
        Ok((
            row.0,
            nonnegative_integer(row.1, 1).map_err(RusqliteCheckpointError::Sqlite)?,
            nonnegative_integer(row.2, 2).map_err(RusqliteCheckpointError::Sqlite)?,
        ))
    }
}

fn nonnegative_integer(value: i64, column: usize) -> Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Integer, Box::new(error))
    })
}
