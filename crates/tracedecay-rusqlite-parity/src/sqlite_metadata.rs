use std::{collections::BTreeSet, fs, io::Read, path::Path};

use tracedecay_sqlite_parity_protocol::{
    EffectiveJournalMode, ErrorCode, ErrorPayload, IntegrityCheck, IntegrityReport,
    JournalModeMetadata, JournalModeNormalization, Metadata, Output, SchemaMetadata, SchemaObject,
    SchemaObjectKind, SourceHeaderJournalMode, SourceJournalMode,
};

use crate::{closed_sql, snapshot::ReadOnlyDriver, snapshot::sqlite_query_error};

const MAX_SCHEMA_OBJECTS: usize = 10_000;
const SQLITE_HEADER_LEN: usize = 20;
const SQLITE_HEADER_SIGNATURE: &[u8; 16] = b"SQLite format 3\0";

impl ReadOnlyDriver {
    pub(crate) fn metadata(&self) -> Result<Metadata, ErrorPayload> {
        let sqlite_version = self
            .connection
            .query_row(closed_sql::SQLITE_VERSION, [], |row| row.get(0))
            .map_err(sqlite_query_error)?;
        let mut statement = self
            .connection
            .prepare(closed_sql::COMPILE_OPTIONS)
            .map_err(sqlite_query_error)?;
        let options = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_query_error)?
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(sqlite_query_error)?;

        Ok(Metadata {
            canonical_path: self.canonical_path.clone(),
            query_only: true,
            immutable: true,
            sqlite_version,
            compile_options: options.into_iter().collect(),
        })
    }

    pub(crate) fn foreign_keys(&self) -> Result<Output, ErrorPayload> {
        self.connection
            .query_row(closed_sql::FOREIGN_KEYS, [], |row| row.get::<_, i64>(0))
            .map(|enabled| Output::ForeignKeys {
                enabled: enabled != 0,
            })
            .map_err(sqlite_query_error)
    }

    pub(crate) fn page_size(&self) -> Result<Output, ErrorPayload> {
        let observed = self
            .connection
            .query_row(closed_sql::PAGE_SIZE, [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_query_error)?;
        let bytes = u32::try_from(observed).map_err(|_| {
            ErrorPayload::new(
                ErrorCode::InvalidSqliteValue,
                format!("SQLite returned invalid page size {observed}"),
            )
        })?;
        if bytes == 0 {
            return Err(ErrorPayload::new(
                ErrorCode::InvalidSqliteValue,
                "SQLite returned zero page size",
            ));
        }
        Ok(Output::PageSize { bytes })
    }

    pub(crate) fn journal_mode(&self) -> Result<JournalModeMetadata, ErrorPayload> {
        let observed = self
            .connection
            .query_row(closed_sql::JOURNAL_MODE, [], |row| row.get::<_, String>(0))
            .map_err(sqlite_query_error)?;
        let immutable_effective_mode = match observed.to_ascii_lowercase().as_str() {
            "delete" => EffectiveJournalMode::Delete,
            _ => {
                return Err(ErrorPayload::new(
                    ErrorCode::InvalidSqliteValue,
                    format!(
                        "immutable SQLite connection returned unsupported journal mode {observed:?}; expected DELETE because sidecars are unavailable"
                    ),
                )
                .with_path(&self.canonical_path));
            }
        };
        let normalization = match self.source_header_journal_mode.mode {
            SourceJournalMode::Rollback => JournalModeNormalization::RollbackSourceImmutableDelete,
            SourceJournalMode::Wal => JournalModeNormalization::WalSourceImmutableDelete,
        };
        Ok(JournalModeMetadata {
            source_header: self.source_header_journal_mode.clone(),
            mode: immutable_effective_mode,
            immutable_effective_mode,
            normalization,
        })
    }

    pub(crate) fn schema(&self) -> Result<SchemaMetadata, ErrorPayload> {
        let schema_version = self
            .connection
            .query_row(closed_sql::SCHEMA_VERSION, [], |row| row.get(0))
            .map_err(sqlite_query_error)?;
        let user_version = self
            .connection
            .query_row(closed_sql::USER_VERSION, [], |row| row.get(0))
            .map_err(sqlite_query_error)?;
        let mut statement = self
            .connection
            .prepare(closed_sql::SCHEMA_OBJECTS)
            .map_err(sqlite_query_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(sqlite_query_error)?;
        let mut objects = Vec::new();
        for row in rows {
            let (kind, name, table_name, sql) = row.map_err(sqlite_query_error)?;
            let kind = match kind.as_str() {
                "table" => SchemaObjectKind::Table,
                "index" => SchemaObjectKind::Index,
                "trigger" => SchemaObjectKind::Trigger,
                "view" => SchemaObjectKind::View,
                _ => {
                    return Err(ErrorPayload::new(
                        ErrorCode::InvalidSqliteValue,
                        format!("SQLite returned unexpected schema object type {kind:?}"),
                    ));
                }
            };
            objects.push(SchemaObject {
                kind,
                name,
                table_name,
                sql,
            });
        }
        if objects.len() > MAX_SCHEMA_OBJECTS {
            return Err(ErrorPayload::new(
                ErrorCode::ResultLimitExceeded,
                format!("schema contains more than {MAX_SCHEMA_OBJECTS} objects"),
            ));
        }
        Ok(SchemaMetadata {
            schema_version,
            user_version,
            objects,
        })
    }

    pub(crate) fn integrity(&self, check: IntegrityCheck) -> Result<IntegrityReport, ErrorPayload> {
        let sql = match check {
            IntegrityCheck::Quick => closed_sql::QUICK_CHECK,
            IntegrityCheck::Full => closed_sql::INTEGRITY_CHECK,
        };
        let mut statement = self.connection.prepare(sql).map_err(sqlite_query_error)?;
        let findings = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_query_error)?;
        Ok(IntegrityReport { check, findings })
    }
}

pub(crate) fn read_source_header_journal_mode(
    path: &Path,
) -> Result<SourceHeaderJournalMode, ErrorPayload> {
    let length = fs::metadata(path)
        .map_err(|error| {
            ErrorPayload::new(
                ErrorCode::InvalidSqliteHeader,
                format!("could not inspect copied snapshot header length: {error}"),
            )
            .with_path(path)
        })?
        .len();
    if length < SQLITE_HEADER_LEN as u64 {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSqliteHeader,
            format!(
                "copied snapshot is {length} bytes; a SQLite header requires at least {SQLITE_HEADER_LEN} bytes"
            ),
        )
        .with_path(path));
    }

    let mut header = [0_u8; SQLITE_HEADER_LEN];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| {
            ErrorPayload::new(
                ErrorCode::InvalidSqliteHeader,
                format!("could not read copied snapshot SQLite header: {error}"),
            )
            .with_path(path)
        })?;
    if &header[..SQLITE_HEADER_SIGNATURE.len()] != SQLITE_HEADER_SIGNATURE {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSqliteHeader,
            "copied snapshot does not have the SQLite format 3 header signature",
        )
        .with_path(path));
    }

    let read_version = header[18];
    let write_version = header[19];
    let mode = match (read_version, write_version) {
        (1, 1) => SourceJournalMode::Rollback,
        (2, 2) => SourceJournalMode::Wal,
        (read, write) => {
            return Err(ErrorPayload::new(
                ErrorCode::InvalidSqliteHeader,
                format!(
                    "copied snapshot has inconsistent or unknown SQLite header journal versions: read={read}, write={write}; expected 1/1 (rollback) or 2/2 (WAL)"
                ),
            )
            .with_path(path));
        }
    };
    Ok(SourceHeaderJournalMode {
        read_version,
        write_version,
        mode,
    })
}
