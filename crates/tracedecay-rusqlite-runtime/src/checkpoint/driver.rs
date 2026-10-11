use std::{error::Error, fmt};

use tracedecay_sql::{CheckpointMode as EngineCheckpointMode, Connection, ConnectionControl};

use super::types::{CheckpointMode, CheckpointReport, WalSample};

const WAL_HEADER_BYTES: u64 = 32;
const WAL_FRAME_HEADER_BYTES: u64 = 24;

#[derive(Debug)]
pub(crate) enum RusqliteCheckpointError {
    Sqlite(tracedecay_sql::Error),
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

impl From<tracedecay_sql::Error> for RusqliteCheckpointError {
    fn from(error: tracedecay_sql::Error) -> Self {
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

impl RusqliteCheckpointDriver {
    /// Sampling runs after every batch, inside `rusqlite.writer.checkpoint`,
    /// so it is measured apart from `rusqlite.wal_checkpoint`: a hot writer
    /// span with cold checkpoints means the per-commit NOOP probe itself is
    /// the cost, not WAL copy-back.
    pub(crate) fn sample_wal(&mut self) -> Result<WalSample, RusqliteCheckpointError> {
        let _span = tracing::trace_span!("rusqlite.wal_sample").entered();
        let frames = nonnegative(
            self.connection
                .checkpoint(EngineCheckpointMode::Noop)?
                .log_frames,
            1,
        )?;
        let page_size = nonnegative(
            self.connection
                .pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0))?,
            0,
        )?;
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

    pub(crate) fn checkpoint(
        &mut self,
        mode: CheckpointMode,
    ) -> Result<CheckpointReport, RusqliteCheckpointError> {
        let _span = tracing::trace_span!("rusqlite.wal_checkpoint").entered();
        let outcome = self.connection.checkpoint(match mode {
            CheckpointMode::Passive => EngineCheckpointMode::Passive,
            CheckpointMode::Restart => EngineCheckpointMode::Restart,
            CheckpointMode::Truncate => EngineCheckpointMode::Truncate,
        })?;
        failpoint!("mid_checkpoint");
        Ok(CheckpointReport {
            busy: outcome.busy,
            log_frames: nonnegative(outcome.log_frames, 1)?,
            checkpointed_frames: nonnegative(outcome.checkpointed_frames, 2)?,
        })
    }
}

/// `column` is the value's column in SQLite's checkpoint result row.
fn nonnegative(value: i64, column: usize) -> Result<u64, RusqliteCheckpointError> {
    u64::try_from(value).map_err(|_| {
        RusqliteCheckpointError::Sqlite(tracedecay_sql::Error::IntegralValueOutOfRange(
            column, value,
        ))
    })
}
