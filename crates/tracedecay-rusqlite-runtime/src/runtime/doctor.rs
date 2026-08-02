use std::{error::Error, fmt};

use rusqlite::{Connection, OptionalExtension, types::Type};
use tracedecay_store::StoreRuntimeBindingV1;

use crate::{
    WriterState,
    reader::{ReaderPoolSnapshot, ReaderPoolState},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntegrityResult {
    Healthy,
    Corrupt { messages: Vec<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalHealth {
    pub enabled: bool,
    pub busy: bool,
    pub log_frames: u64,
    pub checkpointed_frames: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoctorHealthSnapshot {
    pub binding: StoreRuntimeBindingV1,
    pub quick_check: IntegrityResult,
    pub integrity_check: Option<IntegrityResult>,
    pub wal: WalHealth,
    pub writer_state: WriterState,
    pub reader_state: ReaderPoolState,
    pub reader_workers: u16,
    pub available_health_readers: u16,
    pub leased_readers: u16,
}

#[derive(Debug)]
pub struct DoctorHealthError {
    stage: &'static str,
    source: rusqlite::Error,
}

impl fmt::Display for DoctorHealthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "SQLite Doctor health probe failed at {}: {}",
            self.stage, self.source
        )
    }
}

impl Error for DoctorHealthError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Owns the reserved Doctor connection; callers supply only runtime state.
pub struct SqliteDoctorHealthLane {
    binding: StoreRuntimeBindingV1,
    connection: Connection,
}

impl SqliteDoctorHealthLane {
    pub fn from_health_connection(binding: StoreRuntimeBindingV1, connection: Connection) -> Self {
        Self {
            binding,
            connection,
        }
    }

    pub fn inspect(
        &self,
        writer_state: WriterState,
        readers: ReaderPoolSnapshot,
        include_full_integrity: bool,
    ) -> Result<DoctorHealthSnapshot, DoctorHealthError> {
        let quick_check = integrity_rows(&self.connection, "PRAGMA quick_check", "quick_check")?;
        let integrity_check = include_full_integrity
            .then(|| {
                integrity_rows(
                    &self.connection,
                    "PRAGMA integrity_check",
                    "integrity_check",
                )
            })
            .transpose()?;
        let wal = wal_health(&self.connection)?;
        Ok(DoctorHealthSnapshot {
            binding: self.binding.clone(),
            quick_check,
            integrity_check,
            wal,
            writer_state,
            reader_state: readers.state,
            reader_workers: readers
                .general_workers
                .saturating_add(readers.health_workers),
            available_health_readers: readers.available_health,
            leased_readers: readers.leased_general.saturating_add(readers.leased_health),
        })
    }

    pub fn close(self) -> Result<(), DoctorHealthError> {
        self.connection
            .close()
            .map_err(|(_, source)| DoctorHealthError {
                stage: "close",
                source,
            })
    }
}

fn integrity_rows(
    connection: &Connection,
    pragma: &'static str,
    stage: &'static str,
) -> Result<IntegrityResult, DoctorHealthError> {
    let mut statement = connection
        .prepare(pragma)
        .map_err(|source| DoctorHealthError { stage, source })?;
    let messages = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| DoctorHealthError { stage, source })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|source| DoctorHealthError { stage, source })?;
    if messages.len() == 1 && messages[0].eq_ignore_ascii_case("ok") {
        Ok(IntegrityResult::Healthy)
    } else {
        Ok(IntegrityResult::Corrupt { messages })
    }
}

fn wal_health(connection: &Connection) -> Result<WalHealth, DoctorHealthError> {
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|source| DoctorHealthError {
            stage: "journal mode",
            source,
        })?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Ok(WalHealth {
            enabled: false,
            busy: false,
            log_frames: 0,
            checkpointed_frames: 0,
        });
    }
    let row = connection
        .query_row("PRAGMA wal_checkpoint(NOOP)", [], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .optional()
        .map_err(|source| DoctorHealthError {
            stage: "wal state",
            source,
        })?
        .ok_or(rusqlite::Error::QueryReturnedNoRows)
        .map_err(|source| DoctorHealthError {
            stage: "wal state",
            source,
        })?;
    Ok(WalHealth {
        enabled: true,
        busy: row.0 != 0,
        log_frames: nonnegative(row.1, 1)?,
        checkpointed_frames: nonnegative(row.2, 2)?,
    })
}

fn nonnegative(value: i64, column: usize) -> Result<u64, DoctorHealthError> {
    u64::try_from(value).map_err(|error| DoctorHealthError {
        stage: "wal state",
        source: rusqlite::Error::FromSqlConversionFailure(column, Type::Integer, Box::new(error)),
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn doctor_reports_integrity_wal_and_runtime_lanes() {
        let directory = TempDir::new().unwrap();
        let connection = Connection::open(directory.path().join("doctor.sqlite3")).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE facts(value INTEGER);")
            .unwrap();
        let binding = serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.doctor",
                "profile_id": "profile.doctor",
                "scope": { "kind": "project", "project_id": "project.doctor" }
            },
            "incarnation": 1,
            "authority_epoch": 2
        }))
        .unwrap();
        let snapshot = SqliteDoctorHealthLane::from_health_connection(binding, connection)
            .inspect(
                WriterState::Ready,
                ReaderPoolSnapshot {
                    state: ReaderPoolState::Ready,
                    general_workers: 2,
                    available_general: 1,
                    health_workers: 1,
                    available_health: 1,
                    leased_general: 1,
                    leased_health: 0,
                    limbo_general: 0,
                    limbo_health: 0,
                },
                true,
            )
            .unwrap();

        assert_eq!(snapshot.quick_check, IntegrityResult::Healthy);
        assert_eq!(snapshot.integrity_check, Some(IntegrityResult::Healthy));
        assert!(snapshot.wal.enabled);
        assert_eq!(snapshot.reader_workers, 3);
        assert_eq!(snapshot.available_health_readers, 1);
    }
}
