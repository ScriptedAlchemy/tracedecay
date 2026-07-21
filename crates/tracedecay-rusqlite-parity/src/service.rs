//! Process-isolated, read-only SQLite parity service implementation.
//!
//! Only dedicated helper binary targets call this crate. Normal TraceDecay
//! executables continue to link libsql without linking bundled rusqlite. The
//! protocol has no generic SQL operation and accepts only sealed copies.

use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags, Row, params, params_from_iter, types::Value};
use sha2::{Digest, Sha256};
use tracedecay_sqlite_parity_protocol::{
    CanonicalRowHasher, Command, CopiedDatabase, EffectiveJournalMode, ErrorCode, ErrorPayload,
    FtsMatch, FtsParity, GraphFtsTable, GraphTable, IntegrityCheck, IntegrityReport,
    JournalModeMetadata, JournalModeNormalization, MAX_REQUEST_BYTES, Metadata, Output,
    PROTOCOL_VERSION, ROW_DIGEST_ALGORITHM, Request, Response, ResponseOutcome, RowParity,
    SchemaMetadata, SchemaObject, SchemaObjectKind, SessionStoreColumn, SessionStoreCount,
    SessionStoreCursor, SessionStoreFamily, SessionStoreForeignKey, SessionStorePage,
    SessionStoreRow, SessionStoreSchema, SessionStoreTable, SnapshotFileIdentity,
    SourceHeaderJournalMode, SourceJournalMode, VerifiedCopiedSnapshot, decode_request_value,
    validate_command, validate_copied_snapshot_provenance,
};

const MAX_SCHEMA_OBJECTS: usize = 10_000;
const SQLITE_HEADER_LEN: usize = 20;
const SQLITE_HEADER_SIGNATURE: &[u8; 16] = b"SQLite format 3\0";

const READ_ONLY_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_ONLY
    .union(OpenFlags::SQLITE_OPEN_URI)
    .union(OpenFlags::SQLITE_OPEN_NO_MUTEX)
    .union(OpenFlags::SQLITE_OPEN_NOFOLLOW);

const SET_QUERY_ONLY_SQL: &str = "PRAGMA query_only = ON";
const QUERY_ONLY_SQL: &str = "PRAGMA query_only";
const SQLITE_VERSION_SQL: &str = "SELECT sqlite_version()";
const COMPILE_OPTIONS_SQL: &str = "PRAGMA compile_options";
const SCHEMA_VERSION_SQL: &str = "PRAGMA schema_version";
const USER_VERSION_SQL: &str = "PRAGMA user_version";
const FOREIGN_KEYS_SQL: &str = "PRAGMA foreign_keys";
const PAGE_SIZE_SQL: &str = "PRAGMA page_size";
const JOURNAL_MODE_SQL: &str = "PRAGMA journal_mode";
const QUICK_CHECK_SQL: &str = "PRAGMA quick_check(1000)";
const INTEGRITY_CHECK_SQL: &str = "PRAGMA integrity_check(1000)";
const SCHEMA_OBJECTS_SQL: &str = "
    SELECT type, name, tbl_name, sql
    FROM sqlite_schema
    WHERE type IN ('table', 'index', 'trigger', 'view')
    ORDER BY type, name
    LIMIT 10001";
const TABLE_EXISTS_SQL: &str = "
    SELECT EXISTS(
        SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1
    )";
const NODES_COUNT_SQL: &str = "SELECT COUNT(*) FROM nodes";
const EDGES_COUNT_SQL: &str = "SELECT COUNT(*) FROM edges";
const FILES_COUNT_SQL: &str = "SELECT COUNT(*) FROM files";
const UNRESOLVED_REFS_COUNT_SQL: &str = "SELECT COUNT(*) FROM unresolved_refs";
const VECTORS_COUNT_SQL: &str = "SELECT COUNT(*) FROM vectors";
const METADATA_COUNT_SQL: &str = "SELECT COUNT(*) FROM metadata";
const NODES_FTS_COUNT_SQL: &str = "SELECT COUNT(*) FROM nodes_fts";
const NODES_FTS_MATCH_SQL: &str = "
    SELECT rowid, rank, snippet(nodes_fts, 0, '<mark>', '</mark>', '…', 24)
    FROM nodes_fts
    WHERE nodes_fts MATCH ?1
    ORDER BY rank, rowid
    LIMIT ?2";

/// Private graph SQL mapping. The shared protocol contains semantic targets,
/// never identifiers or executable SQL.
#[derive(Clone, Copy)]
struct GraphTableSpec {
    identifier: &'static str,
    count_sql: &'static str,
}

fn graph_table_spec(table: GraphTable) -> GraphTableSpec {
    match table {
        GraphTable::Nodes => GraphTableSpec {
            identifier: "nodes",
            count_sql: NODES_COUNT_SQL,
        },
        GraphTable::Edges => GraphTableSpec {
            identifier: "edges",
            count_sql: EDGES_COUNT_SQL,
        },
        GraphTable::Files => GraphTableSpec {
            identifier: "files",
            count_sql: FILES_COUNT_SQL,
        },
        GraphTable::UnresolvedRefs => GraphTableSpec {
            identifier: "unresolved_refs",
            count_sql: UNRESOLVED_REFS_COUNT_SQL,
        },
        GraphTable::Vectors => GraphTableSpec {
            identifier: "vectors",
            count_sql: VECTORS_COUNT_SQL,
        },
        GraphTable::Metadata => GraphTableSpec {
            identifier: "metadata",
            count_sql: METADATA_COUNT_SQL,
        },
        GraphTable::NodesFts => GraphTableSpec {
            identifier: "nodes_fts",
            count_sql: NODES_FTS_COUNT_SQL,
        },
    }
}

/// Private session SQL mapping. The protocol remains a driver-free semantic
/// vocabulary while this process owns every physical identifier and statement.
#[derive(Clone, Copy)]
struct SessionTableSpec {
    identifier: &'static str,
    count_sql: &'static str,
    table_info_sql: &'static str,
    foreign_key_sql: &'static str,
}

fn session_table_spec(table: SessionStoreTable) -> SessionTableSpec {
    match table {
        SessionStoreTable::Observations => SessionTableSpec {
            identifier: "observations",
            count_sql: "SELECT COUNT(*) FROM observations",
            table_info_sql: "PRAGMA table_info(observations)",
            foreign_key_sql: "PRAGMA foreign_key_list(observations)",
        },
        SessionStoreTable::Sessions => SessionTableSpec {
            identifier: "sessions",
            count_sql: "SELECT COUNT(*) FROM sessions",
            table_info_sql: "PRAGMA table_info(sessions)",
            foreign_key_sql: "PRAGMA foreign_key_list(sessions)",
        },
        SessionStoreTable::SessionMessages => SessionTableSpec {
            identifier: "session_messages",
            count_sql: "SELECT COUNT(*) FROM session_messages",
            table_info_sql: "PRAGMA table_info(session_messages)",
            foreign_key_sql: "PRAGMA foreign_key_list(session_messages)",
        },
        SessionStoreTable::SessionSchemaMigrations => SessionTableSpec {
            identifier: "session_schema_migrations",
            count_sql: "SELECT COUNT(*) FROM session_schema_migrations",
            table_info_sql: "PRAGMA table_info(session_schema_migrations)",
            foreign_key_sql: "PRAGMA foreign_key_list(session_schema_migrations)",
        },
        SessionStoreTable::LcmRawMessages => SessionTableSpec {
            identifier: "lcm_raw_messages",
            count_sql: "SELECT COUNT(*) FROM lcm_raw_messages",
            table_info_sql: "PRAGMA table_info(lcm_raw_messages)",
            foreign_key_sql: "PRAGMA foreign_key_list(lcm_raw_messages)",
        },
        SessionStoreTable::SessionTemporalSchemaMigrations => SessionTableSpec {
            identifier: "session_temporal_schema_migrations",
            count_sql: "SELECT COUNT(*) FROM session_temporal_schema_migrations",
            table_info_sql: "PRAGMA table_info(session_temporal_schema_migrations)",
            foreign_key_sql: "PRAGMA foreign_key_list(session_temporal_schema_migrations)",
        },
        SessionStoreTable::SessionTemporalGenerations => SessionTableSpec {
            identifier: "session_temporal_generations",
            count_sql: "SELECT COUNT(*) FROM session_temporal_generations",
            table_info_sql: "PRAGMA table_info(session_temporal_generations)",
            foreign_key_sql: "PRAGMA foreign_key_list(session_temporal_generations)",
        },
        SessionStoreTable::SessionTemporalObservationEffects => SessionTableSpec {
            identifier: "session_temporal_observation_effects",
            count_sql: "SELECT COUNT(*) FROM session_temporal_observation_effects",
            table_info_sql: "PRAGMA table_info(session_temporal_observation_effects)",
            foreign_key_sql: "PRAGMA foreign_key_list(session_temporal_observation_effects)",
        },
    }
}

struct ReadOnlyDriver {
    canonical_path: PathBuf,
    source_header_journal_mode: SourceHeaderJournalMode,
    connection: Connection,
}

impl ReadOnlyDriver {
    fn open(snapshot: &VerifiedCopiedSnapshot) -> Result<Self, ErrorPayload> {
        let canonical_path = validate_verified_snapshot(snapshot)?;
        let source_header_journal_mode = read_source_header_journal_mode(&canonical_path)?;
        let mut uri = url::Url::from_file_path(&canonical_path).map_err(|()| {
            ErrorPayload::new(
                ErrorCode::InvalidPath,
                "copied snapshot path could not be represented as a file URI",
            )
            .with_path(&canonical_path)
        })?;
        uri.query_pairs_mut()
            .append_pair("mode", "ro")
            .append_pair("immutable", "1");

        let connection =
            Connection::open_with_flags(uri.as_str(), READ_ONLY_FLAGS).map_err(|error| {
                let mut payload = sqlite_error(
                    ErrorCode::OpenFailed,
                    "could not open copied snapshot read-only/no-create",
                    error,
                );
                payload.path = Some(canonical_path.clone());
                payload
            })?;
        // Revalidate after open so a replacement between request validation and
        // SQLite's immutable open is detected before executing a command.
        validate_verified_snapshot(snapshot)?;
        connection
            .execute_batch(SET_QUERY_ONLY_SQL)
            .map_err(|error| {
                sqlite_error(
                    ErrorCode::ReadOnlyInvariant,
                    "could not enable SQLite query_only",
                    error,
                )
                .with_path(&canonical_path)
            })?;
        let observed = connection
            .query_row(QUERY_ONLY_SQL, [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_query_error)?;
        if observed != 1 {
            return Err(ErrorPayload::new(
                ErrorCode::ReadOnlyInvariant,
                format!("SQLite query_only was not retained (observed {observed})"),
            )
            .with_path(&canonical_path));
        }

        Ok(Self {
            canonical_path,
            source_header_journal_mode,
            connection,
        })
    }

    fn execute(&self, command: Command) -> Result<Output, ErrorPayload> {
        validate_command(&command)?;
        match command {
            Command::Metadata => self.metadata().map(Output::Metadata),
            Command::Schema => self.schema().map(Output::Schema),
            Command::ForeignKeys => {
                self.scalar_i64(FOREIGN_KEYS_SQL)
                    .map(|enabled| Output::ForeignKeys {
                        enabled: enabled != 0,
                    })
            }
            Command::PageSize => {
                let observed = self.scalar_i64(PAGE_SIZE_SQL)?;
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
            Command::JournalMode => self.journal_mode().map(Output::JournalMode),
            Command::Integrity { check } => self.integrity(check).map(Output::Integrity),
            Command::RowParity { table } => self.row_parity(table).map(Output::RowParity),
            Command::FtsParity {
                table,
                query,
                limit,
            } => self.fts_parity(table, &query, limit).map(Output::FtsParity),
            Command::SessionStoreCount { family, table } => self
                .session_store_count(family, table)
                .map(Output::SessionStoreCount),
            Command::SessionStoreSchema { family, table } => self
                .session_store_schema(family, table)
                .map(Output::SessionStoreSchema),
            Command::SessionStorePage {
                family,
                table,
                cursor,
                limit,
            } => self
                .session_store_page(family, table, cursor, limit)
                .map(Output::SessionStorePage),
        }
    }

    fn metadata(&self) -> Result<Metadata, ErrorPayload> {
        let sqlite_version = self
            .connection
            .query_row(SQLITE_VERSION_SQL, [], |row| row.get(0))
            .map_err(sqlite_query_error)?;
        let mut statement = self
            .connection
            .prepare(COMPILE_OPTIONS_SQL)
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

    fn journal_mode(&self) -> Result<JournalModeMetadata, ErrorPayload> {
        let observed = self
            .connection
            .query_row(JOURNAL_MODE_SQL, [], |row| row.get::<_, String>(0))
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

    fn schema(&self) -> Result<SchemaMetadata, ErrorPayload> {
        let schema_version = self.scalar_i64(SCHEMA_VERSION_SQL)?;
        let user_version = self.scalar_i64(USER_VERSION_SQL)?;
        let mut statement = self
            .connection
            .prepare(SCHEMA_OBJECTS_SQL)
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

    fn integrity(&self, check: IntegrityCheck) -> Result<IntegrityReport, ErrorPayload> {
        let sql = match check {
            IntegrityCheck::Quick => QUICK_CHECK_SQL,
            IntegrityCheck::Full => INTEGRITY_CHECK_SQL,
        };
        let mut statement = self.connection.prepare(sql).map_err(sqlite_query_error)?;
        let findings = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_query_error)?;
        Ok(IntegrityReport { check, findings })
    }

    fn row_parity(&self, table: GraphTable) -> Result<RowParity, ErrorPayload> {
        let spec = graph_table_spec(table);
        let exists = self
            .connection
            .query_row(TABLE_EXISTS_SQL, params![spec.identifier], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(sqlite_query_error)?;
        let row_count = if exists == 0 {
            None
        } else {
            let observed = self.scalar_i64(spec.count_sql)?;
            Some(u64::try_from(observed).map_err(|_| {
                ErrorPayload::new(
                    ErrorCode::InvalidSqliteValue,
                    format!(
                        "SQLite returned negative row count {observed} for {}",
                        spec.identifier
                    ),
                )
            })?)
        };
        Ok(RowParity { table, row_count })
    }

    fn fts_parity(
        &self,
        table: GraphFtsTable,
        query: &str,
        limit: u16,
    ) -> Result<FtsParity, ErrorPayload> {
        let sql = match table {
            GraphFtsTable::Nodes => NODES_FTS_MATCH_SQL,
        };
        let mut statement = self.connection.prepare(sql).map_err(sqlite_query_error)?;
        let matches = statement
            .query_map(params![query, i64::from(limit)], |row| {
                Ok(FtsMatch {
                    rowid: row.get(0)?,
                    rank: row.get(1)?,
                    snippet: row.get(2)?,
                })
            })
            .map_err(sqlite_query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_query_error)?;
        Ok(FtsParity { table, matches })
    }

    fn session_store_count(
        &self,
        family: SessionStoreFamily,
        table: SessionStoreTable,
    ) -> Result<SessionStoreCount, ErrorPayload> {
        let spec = session_table_spec(table);
        let exists = self.table_exists(spec.identifier)?;
        let row_count = if exists {
            let observed = self.scalar_i64(spec.count_sql)?;
            Some(nonnegative_u64(observed, spec.identifier, "row count")?)
        } else {
            None
        };
        Ok(SessionStoreCount {
            family,
            table,
            row_count,
        })
    }

    fn session_store_schema(
        &self,
        family: SessionStoreFamily,
        table: SessionStoreTable,
    ) -> Result<SessionStoreSchema, ErrorPayload> {
        let spec = session_table_spec(table);
        let exists = self.table_exists(spec.identifier)?;
        if !exists {
            return Ok(SessionStoreSchema {
                family,
                table,
                exists: false,
                columns: Vec::new(),
                foreign_keys: Vec::new(),
            });
        }

        let mut column_statement = self
            .connection
            .prepare(spec.table_info_sql)
            .map_err(sqlite_query_error)?;
        let columns = column_statement
            .query_map([], |row| {
                Ok(SessionStoreColumn {
                    ordinal: u32::try_from(row.get::<_, i64>(0)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                    name: row.get(1)?,
                    declared_type: row.get(2)?,
                    not_null: row.get::<_, i64>(3)? != 0,
                    default_value: row.get(4)?,
                    primary_key_ordinal: u32::try_from(row.get::<_, i64>(5)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            5,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                })
            })
            .map_err(sqlite_query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_query_error)?;

        let mut foreign_key_statement = self
            .connection
            .prepare(spec.foreign_key_sql)
            .map_err(sqlite_query_error)?;
        let mut foreign_keys = foreign_key_statement
            .query_map([], |row| {
                Ok(SessionStoreForeignKey {
                    id: u32::try_from(row.get::<_, i64>(0)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                    sequence: u32::try_from(row.get::<_, i64>(1)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                    referenced_table: row.get(2)?,
                    from_column: row.get(3)?,
                    to_column: row.get(4)?,
                    on_update: row.get(5)?,
                    on_delete: row.get(6)?,
                    match_kind: row.get(7)?,
                })
            })
            .map_err(sqlite_query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_query_error)?;
        foreign_keys.sort();

        Ok(SessionStoreSchema {
            family,
            table,
            exists: true,
            columns,
            foreign_keys,
        })
    }

    fn session_store_page(
        &self,
        family: SessionStoreFamily,
        table: SessionStoreTable,
        cursor: Option<SessionStoreCursor>,
        limit: u16,
    ) -> Result<SessionStorePage, ErrorPayload> {
        let spec = session_table_spec(table);
        if !self.table_exists(spec.identifier)? {
            return Ok(SessionStorePage {
                family,
                table,
                order_columns: table
                    .order_columns()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                digest_algorithm: ROW_DIGEST_ALGORITHM.to_owned(),
                rows: Vec::new(),
                next_cursor: None,
            });
        }

        let fetch_limit = i64::from(limit) + 1;
        let (sql, parameters) = session_page_query(table, cursor.as_ref(), fetch_limit);
        let mut statement = self.connection.prepare(sql).map_err(sqlite_query_error)?;
        let mut rows = statement
            .query_map(params_from_iter(parameters), |row| {
                decode_session_store_row(table, row)
            })
            .map_err(sqlite_query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_query_error)?;
        let has_more = rows.len() > usize::from(limit);
        rows.truncate(usize::from(limit));
        let next_cursor = if has_more {
            rows.last().map(cursor_for_row).transpose()?
        } else {
            None
        };

        Ok(SessionStorePage {
            family,
            table,
            order_columns: table
                .order_columns()
                .iter()
                .map(ToString::to_string)
                .collect(),
            digest_algorithm: ROW_DIGEST_ALGORITHM.to_owned(),
            rows,
            next_cursor,
        })
    }

    fn table_exists(&self, table: &'static str) -> Result<bool, ErrorPayload> {
        self.connection
            .query_row(TABLE_EXISTS_SQL, params![table], |row| row.get::<_, i64>(0))
            .map(|exists| exists != 0)
            .map_err(sqlite_query_error)
    }

    fn scalar_i64(&self, sql: &'static str) -> Result<i64, ErrorPayload> {
        self.connection
            .query_row(sql, [], |row| row.get(0))
            .map_err(sqlite_query_error)
    }
}

fn session_page_query(
    table: SessionStoreTable,
    cursor: Option<&SessionStoreCursor>,
    limit: i64,
) -> (&'static str, Vec<Value>) {
    match (table, cursor) {
        (SessionStoreTable::Observations, cursor) => (
            "SELECT sequence, observation_id, payload_digest, receipt_id, observation_json,
                    committed_cursor_json
             FROM observations
             WHERE sequence > ?1
             ORDER BY sequence
             LIMIT ?2",
            vec![
                Value::Integer(match cursor {
                    Some(SessionStoreCursor::Observations { sequence }) => *sequence,
                    _ => 0,
                }),
                Value::Integer(limit),
            ],
        ),
        (SessionStoreTable::Sessions, cursor) => {
            let (provider, session_id) = match cursor {
                Some(SessionStoreCursor::Sessions {
                    provider,
                    session_id,
                }) => (
                    Value::Text(provider.clone()),
                    Value::Text(session_id.clone()),
                ),
                _ => (Value::Null, Value::Null),
            };
            (
                "SELECT provider, session_id, project_key, project_path, title, started_at,
                        ended_at, transcript_path, metadata_json, parent_session_id, is_subagent,
                        agent_id, parent_tool_use_id
                 FROM sessions
                 WHERE ?1 IS NULL OR provider > ?1 OR (provider = ?1 AND session_id > ?2)
                 ORDER BY provider, session_id
                 LIMIT ?3",
                vec![provider, session_id, Value::Integer(limit)],
            )
        }
        (SessionStoreTable::SessionMessages, cursor) => {
            let (provider, session_id, ordinal, message_id) = match cursor {
                Some(SessionStoreCursor::SessionMessages {
                    provider,
                    session_id,
                    ordinal,
                    message_id,
                }) => (
                    Value::Text(provider.clone()),
                    Value::Text(session_id.clone()),
                    Value::Integer(*ordinal),
                    Value::Text(message_id.clone()),
                ),
                _ => (Value::Null, Value::Null, Value::Null, Value::Null),
            };
            (
                "SELECT provider, session_id, ordinal, message_id, role, timestamp, text, kind,
                        model, tool_names, source_path, source_offset, metadata_json
                 FROM session_messages
                 WHERE ?1 IS NULL
                    OR provider > ?1
                    OR (provider = ?1 AND session_id > ?2)
                    OR (provider = ?1 AND session_id = ?2 AND ordinal > ?3)
                    OR (provider = ?1 AND session_id = ?2 AND ordinal = ?3 AND message_id > ?4)
                 ORDER BY provider, session_id, ordinal, message_id
                 LIMIT ?5",
                vec![
                    provider,
                    session_id,
                    ordinal,
                    message_id,
                    Value::Integer(limit),
                ],
            )
        }
        (SessionStoreTable::SessionSchemaMigrations, cursor) => (
            "SELECT name, version, applied_at
             FROM session_schema_migrations
             WHERE ?1 IS NULL OR name > ?1
             ORDER BY name
             LIMIT ?2",
            vec![
                match cursor {
                    Some(SessionStoreCursor::SessionSchemaMigrations { name }) => {
                        Value::Text(name.clone())
                    }
                    _ => Value::Null,
                },
                Value::Integer(limit),
            ],
        ),
        (SessionStoreTable::LcmRawMessages, cursor) => (
            "SELECT store_id, provider, session_id, ordinal, message_id, role, timestamp, content,
                    content_hash, storage_kind, payload_ref, snippet_text, index_text,
                    legacy_source, legacy_truncated, metadata_json
             FROM lcm_raw_messages
             WHERE store_id > ?1
             ORDER BY store_id
             LIMIT ?2",
            vec![
                Value::Integer(match cursor {
                    Some(SessionStoreCursor::LcmRawMessages { store_id }) => *store_id,
                    _ => 0,
                }),
                Value::Integer(limit),
            ],
        ),
        (SessionStoreTable::SessionTemporalSchemaMigrations, cursor) => (
            "SELECT name, version, applied_at
             FROM session_temporal_schema_migrations
             WHERE ?1 IS NULL OR name > ?1
             ORDER BY name
             LIMIT ?2",
            vec![
                match cursor {
                    Some(SessionStoreCursor::SessionTemporalSchemaMigrations { name }) => {
                        Value::Text(name.clone())
                    }
                    _ => Value::Null,
                },
                Value::Integer(limit),
            ],
        ),
        (SessionStoreTable::SessionTemporalGenerations, cursor) => {
            let (session_id, generation) = match cursor {
                Some(SessionStoreCursor::SessionTemporalGenerations {
                    session_id,
                    generation,
                }) => (Value::Text(session_id.clone()), Value::Integer(*generation)),
                _ => (Value::Null, Value::Null),
            };
            (
                "SELECT session_id, generation, state, frozen_watermarks_json, created_at,
                        ready_at, activated_at, completed_at
                 FROM session_temporal_generations
                 WHERE ?1 IS NULL OR session_id > ?1
                    OR (session_id = ?1 AND generation > ?2)
                 ORDER BY session_id, generation
                 LIMIT ?3",
                vec![session_id, generation, Value::Integer(limit)],
            )
        }
        (SessionStoreTable::SessionTemporalObservationEffects, cursor) => (
            "SELECT observation_id, observation_sequence, session_id, receipt_id, effect_digest,
                    output_count, recorded_at
             FROM session_temporal_observation_effects
             WHERE observation_sequence > ?1
             ORDER BY observation_sequence
             LIMIT ?2",
            vec![
                Value::Integer(match cursor {
                    Some(SessionStoreCursor::SessionTemporalObservationEffects {
                        observation_sequence,
                    }) => *observation_sequence,
                    _ => 0,
                }),
                Value::Integer(limit),
            ],
        ),
    }
}

fn decode_session_store_row(
    table: SessionStoreTable,
    row: &Row<'_>,
) -> rusqlite::Result<SessionStoreRow> {
    let row_digest = digest_row(row)?;
    match table {
        SessionStoreTable::Observations => Ok(SessionStoreRow::Observations {
            sequence: row.get(0)?,
            observation_id: row.get(1)?,
            payload_digest: row.get(2)?,
            row_digest,
        }),
        SessionStoreTable::Sessions => Ok(SessionStoreRow::Sessions {
            provider: row.get(0)?,
            session_id: row.get(1)?,
            row_digest,
        }),
        SessionStoreTable::SessionMessages => Ok(SessionStoreRow::SessionMessages {
            provider: row.get(0)?,
            session_id: row.get(1)?,
            ordinal: row.get(2)?,
            message_id: row.get(3)?,
            row_digest,
        }),
        SessionStoreTable::SessionSchemaMigrations => {
            Ok(SessionStoreRow::SessionSchemaMigrations {
                name: row.get(0)?,
                version: row.get(1)?,
                row_digest,
            })
        }
        SessionStoreTable::LcmRawMessages => Ok(SessionStoreRow::LcmRawMessages {
            store_id: row.get(0)?,
            provider: row.get(1)?,
            session_id: row.get(2)?,
            ordinal: row.get(3)?,
            message_id: row.get(4)?,
            content_hash: row.get(8)?,
            row_digest,
        }),
        SessionStoreTable::SessionTemporalSchemaMigrations => {
            Ok(SessionStoreRow::SessionTemporalSchemaMigrations {
                name: row.get(0)?,
                version: row.get(1)?,
                row_digest,
            })
        }
        SessionStoreTable::SessionTemporalGenerations => {
            Ok(SessionStoreRow::SessionTemporalGenerations {
                session_id: row.get(0)?,
                generation: row.get(1)?,
                state: row.get(2)?,
                row_digest,
            })
        }
        SessionStoreTable::SessionTemporalObservationEffects => {
            Ok(SessionStoreRow::SessionTemporalObservationEffects {
                observation_id: row.get(0)?,
                observation_sequence: row.get(1)?,
                session_id: row.get(2)?,
                effect_digest: row.get(4)?,
                row_digest,
            })
        }
    }
}

fn cursor_for_row(row: &SessionStoreRow) -> Result<SessionStoreCursor, ErrorPayload> {
    Ok(match row {
        SessionStoreRow::Observations { sequence, .. } => SessionStoreCursor::Observations {
            sequence: *sequence,
        },
        SessionStoreRow::Sessions {
            provider,
            session_id,
            ..
        } => SessionStoreCursor::Sessions {
            provider: provider.clone(),
            session_id: session_id.clone(),
        },
        SessionStoreRow::SessionMessages {
            provider,
            session_id,
            ordinal,
            message_id,
            ..
        } => SessionStoreCursor::SessionMessages {
            provider: provider.clone(),
            session_id: session_id.clone(),
            ordinal: *ordinal,
            message_id: message_id.clone(),
        },
        SessionStoreRow::SessionSchemaMigrations { name, .. } => {
            SessionStoreCursor::SessionSchemaMigrations { name: name.clone() }
        }
        SessionStoreRow::LcmRawMessages { store_id, .. } => SessionStoreCursor::LcmRawMessages {
            store_id: *store_id,
        },
        SessionStoreRow::SessionTemporalSchemaMigrations { name, .. } => {
            SessionStoreCursor::SessionTemporalSchemaMigrations { name: name.clone() }
        }
        SessionStoreRow::SessionTemporalGenerations {
            session_id,
            generation,
            ..
        } => SessionStoreCursor::SessionTemporalGenerations {
            session_id: session_id.clone(),
            generation: *generation,
        },
        SessionStoreRow::SessionTemporalObservationEffects {
            observation_sequence,
            ..
        } => SessionStoreCursor::SessionTemporalObservationEffects {
            observation_sequence: *observation_sequence,
        },
    })
}

fn digest_row(row: &Row<'_>) -> rusqlite::Result<String> {
    let mut hash = CanonicalRowHasher::new();
    for index in 0..row.as_ref().column_count() {
        match row.get_ref(index)? {
            rusqlite::types::ValueRef::Null => hash.update_null(),
            rusqlite::types::ValueRef::Integer(value) => hash.update_integer(value),
            rusqlite::types::ValueRef::Real(value) => hash.update_real(value),
            rusqlite::types::ValueRef::Text(value) => hash.update_text(value),
            rusqlite::types::ValueRef::Blob(value) => hash.update_blob(value),
        }
    }
    Ok(hash.finish())
}

fn nonnegative_u64(value: i64, table: &str, label: &str) -> Result<u64, ErrorPayload> {
    u64::try_from(value).map_err(|_| {
        ErrorPayload::new(
            ErrorCode::InvalidSqliteValue,
            format!("SQLite returned negative {label} {value} for {table}"),
        )
    })
}

fn sqlite_query_error(source: rusqlite::Error) -> ErrorPayload {
    sqlite_error(
        ErrorCode::SqliteFailure,
        "read-only SQLite probe failed",
        source,
    )
}

fn sqlite_error(
    code: ErrorCode,
    message: impl Into<String>,
    source: rusqlite::Error,
) -> ErrorPayload {
    let sqlite_code = match &source {
        rusqlite::Error::SqliteFailure(error, _) => Some(format!("{:?}", error.code)),
        _ => None,
    };
    ErrorPayload {
        code,
        message: format!("{}: {source}", message.into()),
        path: None,
        sqlite_code,
    }
}

fn read_source_header_journal_mode(path: &Path) -> Result<SourceHeaderJournalMode, ErrorPayload> {
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

fn validate_copied_path(path: &Path) -> Result<PathBuf, ErrorPayload> {
    if !path.is_absolute() {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidPath,
            "copied snapshot path must be absolute",
        )
        .with_path(path));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidPath,
            format!("copied snapshot path is not an existing regular file: {error}"),
        )
        .with_path(path)
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidPath,
            "copied snapshot path must be a regular file and not a symlink",
        )
        .with_path(path));
    }
    let canonical = fs::canonicalize(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidPath,
            format!("could not canonicalize copied snapshot path: {error}"),
        )
        .with_path(path)
    })?;
    reject_protected_profile_path(&canonical, &protected_profile_roots())?;
    Ok(canonical)
}

fn verify_copied_snapshot(
    database: &CopiedDatabase,
) -> Result<VerifiedCopiedSnapshot, ErrorPayload> {
    let provenance = &database.provenance;
    validate_copied_snapshot_provenance(provenance)?;
    let staging_root = validate_staging_root(&provenance.staging_root)?;
    let canonical_path = validate_copied_path(&database.path)?;
    if !canonical_path.starts_with(&staging_root) {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot is outside its declared private staging root",
        )
        .with_path(&canonical_path));
    }
    if canonical_path != provenance.canonical_path {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot canonical path does not match its sealed provenance",
        )
        .with_path(&canonical_path));
    }
    let (byte_len, content_digest, file_identity) = sealed_file_metadata(&canonical_path)?;
    if byte_len != provenance.byte_len {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!(
                "copied snapshot byte length changed from {} to {byte_len}",
                provenance.byte_len
            ),
        )
        .with_path(&canonical_path));
    }
    if file_identity != provenance.file_identity {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot file identity does not match its sealed provenance",
        )
        .with_path(&canonical_path));
    }
    if content_digest != provenance.content_digest {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot content digest does not match its sealed provenance",
        )
        .with_path(&canonical_path));
    }
    Ok(VerifiedCopiedSnapshot {
        authority_identity: provenance.authority_identity.clone(),
        canonical_path,
        byte_len,
        content_digest,
        file_identity,
    })
}

fn validate_verified_snapshot(snapshot: &VerifiedCopiedSnapshot) -> Result<PathBuf, ErrorPayload> {
    let canonical_path = validate_copied_path(&snapshot.canonical_path)?;
    if canonical_path != snapshot.canonical_path {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "verified snapshot path is not canonical",
        )
        .with_path(&canonical_path));
    }
    let (byte_len, content_digest, file_identity) = sealed_file_metadata(&canonical_path)?;
    if byte_len != snapshot.byte_len
        || content_digest != snapshot.content_digest
        || file_identity != snapshot.file_identity
    {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot changed after provenance verification",
        )
        .with_path(&canonical_path));
    }
    Ok(canonical_path)
}

fn validate_staging_root(path: &Path) -> Result<PathBuf, ErrorPayload> {
    if !path.is_absolute() {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "snapshot staging root must be absolute",
        )
        .with_path(path));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!("snapshot staging root is not an existing directory: {error}"),
        )
        .with_path(path)
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "snapshot staging root must be a directory and not a symlink",
        )
        .with_path(path));
    }
    fs::canonicalize(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!("could not canonicalize snapshot staging root: {error}"),
        )
        .with_path(path)
    })
}

fn sealed_file_metadata(path: &Path) -> Result<(u64, String, SnapshotFileIdentity), ErrorPayload> {
    let mut file = fs::File::open(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!("could not open copied snapshot for provenance verification: {error}"),
        )
        .with_path(path)
    })?;
    let before = file.metadata().map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!("could not inspect copied snapshot provenance: {error}"),
        )
        .with_path(path)
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            ErrorPayload::new(
                ErrorCode::InvalidSnapshotProvenance,
                format!("could not hash copied snapshot provenance: {error}"),
            )
            .with_path(path)
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let after = fs::metadata(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!("could not revalidate copied snapshot provenance: {error}"),
        )
        .with_path(path)
    })?;
    let identity = SnapshotFileIdentity::from_metadata(&before);
    if before.len() != after.len() || identity != SnapshotFileIdentity::from_metadata(&after) {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot changed while its provenance was verified",
        )
        .with_path(path));
    }
    Ok((
        before.len(),
        format!("sha256:{}", hex::encode(hasher.finalize())),
        identity,
    ))
}

fn protected_profile_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = env::var_os("TRACEDECAY_DATA_DIR").filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(root));
    }
    for home_var in ["HOME", "USERPROFILE"] {
        if let Some(home) = env::var_os(home_var).filter(|value| !value.is_empty()) {
            roots.push(PathBuf::from(home).join(".tracedecay"));
        }
    }
    roots
}

fn reject_protected_profile_path(
    canonical: &Path,
    protected_roots: &[PathBuf],
) -> Result<(), ErrorPayload> {
    let has_profile_component = canonical.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".tracedecay")
    });
    let under_protected_root = protected_roots.iter().any(|root| {
        let absolute = if root.is_absolute() {
            root.clone()
        } else {
            env::current_dir()
                .map(|current| current.join(root))
                .unwrap_or_else(|_| root.clone())
        };
        let normalized = fs::canonicalize(&absolute).unwrap_or(absolute);
        canonical.starts_with(normalized)
    });
    if has_profile_component || under_protected_root {
        return Err(ErrorPayload::new(
            ErrorCode::RefusedLiveProfile,
            "path is inside a live/default TraceDecay profile; inspect an explicit copy elsewhere",
        )
        .with_path(canonical));
    }
    Ok(())
}

pub fn handle_request_bytes(bytes: &[u8]) -> Response {
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return error_response(
            None,
            ErrorPayload::new(
                ErrorCode::RequestTooLarge,
                format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
            ),
        );
    }
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(error) => {
            return error_response(
                None,
                ErrorPayload::new(ErrorCode::InvalidRequest, format!("invalid JSON: {error}")),
            );
        }
    };
    let request_id = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let request: Request = match decode_request_value(value) {
        Ok(request) => request,
        Err(error) => return error_response(request_id, error),
    };

    let verified_snapshot = match verify_copied_snapshot(&request.database) {
        Ok(snapshot) => snapshot,
        Err(error) => return error_response(Some(request.request_id), error),
    };
    let outcome = ReadOnlyDriver::open(&verified_snapshot)
        .and_then(|driver| {
            let output = driver.execute(request.command)?;
            validate_verified_snapshot(&verified_snapshot)?;
            Ok(output)
        })
        .map_or_else(
            |error| ResponseOutcome::Error { error },
            |output| ResponseOutcome::Ok { output },
        );
    Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some(request.request_id),
        verified_snapshot: Some(verified_snapshot),
        outcome,
    }
}

fn error_response(request_id: Option<String>, error: ErrorPayload) -> Response {
    Response {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        verified_snapshot: None,
        outcome: ResponseOutcome::Error { error },
    }
}

/// Reads one bounded JSON request and writes one versioned JSON response.
pub fn serve(reader: impl Read, mut writer: impl Write) -> io::Result<()> {
    let mut bytes = Vec::new();
    reader.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut bytes)?;
    let response = handle_request_bytes(&bytes);
    serde_json::to_writer(&mut writer, &response)?;
    writer.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::TempDir;
    use tracedecay_sqlite_parity_protocol::DatabaseKind;

    struct Fixture {
        _directory: TempDir,
        path: PathBuf,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("copied-東京.db");
        let connection = Connection::open(&path).expect("create fixture");
        connection
            .execute_batch(
                "
                PRAGMA page_size = 4096;
                PRAGMA journal_mode = DELETE;
                PRAGMA user_version = 7;
                CREATE TABLE nodes (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    qualified_name TEXT NOT NULL,
                    docstring TEXT,
                    signature TEXT
                );
                CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE TABLE sanitization_receipts (
                    receipt_id TEXT PRIMARY KEY,
                    sanitizer_version TEXT NOT NULL,
                    payload_digest TEXT NOT NULL,
                    receipt_json TEXT NOT NULL
                );
                CREATE TABLE observations (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                    observation_id TEXT NOT NULL UNIQUE,
                    payload_digest TEXT NOT NULL,
                    receipt_id TEXT NOT NULL,
                    observation_json TEXT NOT NULL,
                    committed_cursor_json TEXT NOT NULL,
                    FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
                );
                CREATE TABLE sessions (
                    provider TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    project_key TEXT NOT NULL,
                    project_path TEXT NOT NULL,
                    title TEXT,
                    started_at INTEGER,
                    ended_at INTEGER,
                    transcript_path TEXT,
                    metadata_json TEXT,
                    parent_session_id TEXT,
                    is_subagent INTEGER NOT NULL DEFAULT 0,
                    agent_id TEXT,
                    parent_tool_use_id TEXT,
                    PRIMARY KEY(provider, session_id)
                );
                CREATE TABLE session_messages (
                    provider TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    timestamp INTEGER,
                    ordinal INTEGER NOT NULL,
                    text TEXT NOT NULL,
                    kind TEXT,
                    model TEXT,
                    tool_names TEXT,
                    source_path TEXT,
                    source_offset INTEGER,
                    metadata_json TEXT,
                    PRIMARY KEY(provider, message_id),
                    FOREIGN KEY(provider, session_id)
                        REFERENCES sessions(provider, session_id) ON DELETE CASCADE
                );
                CREATE TABLE session_schema_migrations (
                    name TEXT PRIMARY KEY,
                    version INTEGER NOT NULL,
                    applied_at INTEGER NOT NULL
                );
                CREATE TABLE lcm_raw_messages (
                    provider TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    store_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    role TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    timestamp INTEGER,
                    content TEXT,
                    content_hash TEXT NOT NULL,
                    storage_kind TEXT NOT NULL,
                    payload_ref TEXT,
                    snippet_text TEXT NOT NULL,
                    index_text TEXT NOT NULL,
                    legacy_source INTEGER NOT NULL DEFAULT 0,
                    legacy_truncated INTEGER NOT NULL DEFAULT 0,
                    metadata_json TEXT,
                    UNIQUE(provider, message_id),
                    FOREIGN KEY(provider, session_id)
                        REFERENCES sessions(provider, session_id) ON DELETE CASCADE
                );
                CREATE TABLE session_temporal_schema_migrations (
                    name TEXT PRIMARY KEY,
                    version INTEGER NOT NULL,
                    applied_at INTEGER NOT NULL
                );
                CREATE TABLE session_temporal_generations (
                    session_id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    frozen_watermarks_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    ready_at INTEGER,
                    activated_at INTEGER,
                    completed_at INTEGER,
                    PRIMARY KEY(session_id, generation)
                );
                CREATE TABLE session_temporal_observation_effects (
                    observation_id TEXT PRIMARY KEY,
                    observation_sequence INTEGER NOT NULL UNIQUE,
                    session_id TEXT NOT NULL,
                    receipt_id TEXT NOT NULL,
                    effect_digest TEXT NOT NULL,
                    output_count INTEGER NOT NULL,
                    recorded_at INTEGER NOT NULL
                );
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, docstring, signature,
                    content='nodes', content_rowid='rowid'
                );",
            )
            .expect("create schema");
        connection
            .execute(
                "INSERT INTO nodes(id, name, qualified_name, docstring, signature)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    "node-unicode",
                    "東京 café 🦀",
                    "crate::東京",
                    "Unicode graph node",
                    "fn 東京()"
                ],
            )
            .expect("insert Unicode node");
        connection
            .execute_batch(
                "INSERT INTO sanitization_receipts VALUES ('receipt', 'v1', 'payloads', '{}');
                 INSERT INTO observations(
                     observation_id, payload_digest, receipt_id, observation_json,
                     committed_cursor_json
                 ) VALUES
                     ('observation-1', 'digest-1', 'receipt', '{}', '{}'),
                     ('observation-2', 'digest-2', 'receipt', '{}', '{}');
                 INSERT INTO sessions(provider, session_id, project_key, project_path)
                 VALUES ('codex', 'session-1', 'project', '/copy');
                 INSERT INTO session_messages(
                     provider, message_id, session_id, role, ordinal, text
                 ) VALUES
                     ('codex', 'message-1', 'session-1', 'user', 0, 'one'),
                     ('codex', 'message-2', 'session-1', 'assistant', 1, 'two');
                 INSERT INTO session_schema_migrations VALUES ('hermes-lcm', 11, 1);
                 INSERT INTO lcm_raw_messages(
                     provider, message_id, session_id, role, ordinal, content, content_hash,
                     storage_kind, snippet_text, index_text
                 ) VALUES
                     ('codex', 'message-1', 'session-1', 'user', 0, 'one', 'hash-1',
                      'inline', 'one', 'one'),
                     ('codex', 'message-2', 'session-1', 'assistant', 1, 'two', 'hash-2',
                      'inline', 'two', 'two');
                 INSERT INTO session_temporal_schema_migrations
                 VALUES ('session-temporal', 3, 1);
                 INSERT INTO session_temporal_generations(
                     session_id, generation, state, frozen_watermarks_json, created_at
                 ) VALUES ('session-1', 1, 'ready', '{}', 1);
                 INSERT INTO session_temporal_observation_effects(
                     observation_id, observation_sequence, session_id, receipt_id,
                     effect_digest, output_count, recorded_at
                 ) VALUES ('observation-1', 1, 'session-1', 'receipt', 'effect-1', 1, 1);",
            )
            .expect("insert session-store fixture rows");
        connection
            .execute("INSERT INTO nodes_fts(nodes_fts) VALUES ('rebuild')", [])
            .expect("build FTS index");
        drop(connection);
        Fixture {
            _directory: directory,
            path,
        }
    }

    fn copied_database(path: &Path) -> CopiedDatabase {
        let canonical_path = fs::canonicalize(path).expect("canonicalize copied fixture");
        let (byte_len, content_digest, file_identity) =
            sealed_file_metadata(&canonical_path).expect("seal copied fixture");
        CopiedDatabase {
            path: canonical_path.clone(),
            kind: DatabaseKind::CopiedSnapshot,
            provenance: tracedecay_sqlite_parity_protocol::CopiedSnapshotProvenance {
                authority_identity: "test:copied-snapshot".to_owned(),
                staging_root: canonical_path
                    .parent()
                    .expect("copied fixture parent")
                    .to_path_buf(),
                canonical_path,
                byte_len,
                content_digest,
                file_identity,
            },
        }
    }

    fn missing_copied_database(path: &Path) -> CopiedDatabase {
        CopiedDatabase {
            path: path.to_path_buf(),
            kind: DatabaseKind::CopiedSnapshot,
            provenance: tracedecay_sqlite_parity_protocol::CopiedSnapshotProvenance {
                authority_identity: "test:missing-snapshot".to_owned(),
                staging_root: path.parent().expect("missing fixture parent").to_path_buf(),
                canonical_path: path.to_path_buf(),
                byte_len: 0,
                content_digest: format!("sha256:{}", "0".repeat(64)),
                file_identity: SnapshotFileIdentity::Unsupported,
            },
        }
    }

    fn request_value(
        path: &Path,
        request_id: &str,
        command: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "protocol_version": PROTOCOL_VERSION,
            "request_id": request_id,
            "database": copied_database(path),
            "command": command,
        })
    }

    fn execute(path: &Path, command: Command) -> Output {
        let request = Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: "unit".to_string(),
            database: copied_database(path),
            command,
        };
        let bytes = serde_json::to_vec(&request).expect("serialize request");
        match handle_request_bytes(&bytes).outcome {
            ResponseOutcome::Ok { output } => output,
            ResponseOutcome::Error { error } => panic!("unexpected error: {error:?}"),
        }
    }

    #[test]
    fn missing_snapshot_is_rejected_without_creation() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("missing.db");
        let response = handle_request_bytes(
            &serde_json::to_vec(&Request {
                protocol_version: PROTOCOL_VERSION,
                request_id: "missing".to_string(),
                database: missing_copied_database(&path),
                command: Command::Metadata,
            })
            .expect("serialize request"),
        );
        assert!(matches!(
            response.outcome,
            ResponseOutcome::Error {
                error: ErrorPayload {
                    code: ErrorCode::InvalidPath,
                    ..
                }
            }
        ));
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn final_component_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let fixture = fixture();
        let link = fixture.path.parent().unwrap().join("copied-link.db");
        symlink(&fixture.path, &link).expect("create fixture symlink");
        let error = validate_copied_path(&link).expect_err("symlink must be rejected");
        assert_eq!(error.code, ErrorCode::InvalidPath);
    }

    #[test]
    fn sealed_snapshot_provenance_is_required_and_revalidated() {
        let fixture = fixture();
        let mut database = copied_database(&fixture.path);
        database.provenance.byte_len += 1;
        let response = handle_request_bytes(
            &serde_json::to_vec(&Request {
                protocol_version: PROTOCOL_VERSION,
                request_id: "changed-provenance".to_owned(),
                database,
                command: Command::Metadata,
            })
            .expect("serialize request"),
        );
        assert!(response.verified_snapshot.is_none());
        assert!(matches!(
            response.outcome,
            ResponseOutcome::Error {
                error: ErrorPayload {
                    code: ErrorCode::InvalidSnapshotProvenance,
                    ..
                }
            }
        ));

        let request = Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: "sealed-provenance".to_owned(),
            database: copied_database(&fixture.path),
            command: Command::Metadata,
        };
        let response =
            handle_request_bytes(&serde_json::to_vec(&request).expect("serialize sealed request"));
        assert_eq!(
            response
                .verified_snapshot
                .as_ref()
                .map(|snapshot| &snapshot.canonical_path),
            Some(&fs::canonicalize(&fixture.path).expect("canonicalize copied fixture"))
        );
        assert!(matches!(response.outcome, ResponseOutcome::Ok { .. }));

        let database = copied_database(&fixture.path);
        let mut bytes = fs::read(&fixture.path).expect("read fixture before same-size mutation");
        let last = bytes.last_mut().expect("nonempty SQLite fixture");
        *last ^= 1;
        fs::write(&fixture.path, bytes).expect("mutate fixture without changing its length");
        let response = handle_request_bytes(
            &serde_json::to_vec(&Request {
                protocol_version: PROTOCOL_VERSION,
                request_id: "content-changed".to_owned(),
                database,
                command: Command::Metadata,
            })
            .expect("serialize content-changed request"),
        );
        assert!(matches!(
            response.outcome,
            ResponseOutcome::Error {
                error: ErrorPayload {
                    code: ErrorCode::InvalidSnapshotProvenance,
                    ..
                }
            }
        ));
    }

    #[test]
    fn connection_is_immutable_query_only_and_rejects_writes() {
        let fixture = fixture();
        let before = fs::read(&fixture.path).expect("fixture before probe");
        let verified =
            verify_copied_snapshot(&copied_database(&fixture.path)).expect("verify copied fixture");
        let driver = ReadOnlyDriver::open(&verified).expect("open read-only driver");
        let error = driver
            .connection
            .execute(
                "INSERT INTO metadata(key, value) VALUES ('blocked', 'write')",
                [],
            )
            .expect_err("write must fail");
        let message = error.to_string().to_ascii_lowercase();
        assert!(message.contains("readonly") || message.contains("read-only"));
        drop(driver);
        assert_eq!(
            before,
            fs::read(&fixture.path).expect("fixture after probe")
        );
        assert!(!fixture.path.with_extension("db-wal").exists());
        assert!(!fixture.path.with_extension("db-shm").exists());
    }

    #[test]
    fn typed_commands_report_metadata_schema_checks_rows_and_unicode_fts() {
        let fixture = fixture();
        let Output::Metadata(metadata) = execute(&fixture.path, Command::Metadata) else {
            panic!("metadata output expected");
        };
        assert!(metadata.query_only && metadata.immutable);
        assert!(
            metadata
                .sqlite_version
                .split('.')
                .all(|part| part.parse::<u32>().is_ok())
        );
        assert!(
            metadata
                .compile_options
                .windows(2)
                .all(|pair| pair[0] <= pair[1])
        );
        assert!(
            metadata
                .compile_options
                .iter()
                .any(|option| option == "ENABLE_FTS5")
        );

        let Output::Schema(schema) = execute(&fixture.path, Command::Schema) else {
            panic!("schema output expected");
        };
        assert_eq!(schema.user_version, 7);
        assert!(schema.objects.iter().any(|object| object.name == "nodes"));
        assert!(matches!(
            execute(&fixture.path, Command::ForeignKeys),
            Output::ForeignKeys { .. }
        ));
        assert_eq!(
            execute(&fixture.path, Command::PageSize),
            Output::PageSize { bytes: 4096 }
        );
        assert_eq!(
            execute(&fixture.path, Command::JournalMode),
            Output::JournalMode(JournalModeMetadata {
                source_header: SourceHeaderJournalMode {
                    read_version: 1,
                    write_version: 1,
                    mode: SourceJournalMode::Rollback,
                },
                mode: EffectiveJournalMode::Delete,
                immutable_effective_mode: EffectiveJournalMode::Delete,
                normalization: JournalModeNormalization::RollbackSourceImmutableDelete,
            })
        );
        let Output::Integrity(report) = execute(
            &fixture.path,
            Command::Integrity {
                check: IntegrityCheck::Full,
            },
        ) else {
            panic!("integrity output expected");
        };
        assert_eq!(report.findings, ["ok"]);
        assert_eq!(
            execute(
                &fixture.path,
                Command::RowParity {
                    table: GraphTable::Nodes
                }
            ),
            Output::RowParity(RowParity {
                table: GraphTable::Nodes,
                row_count: Some(1)
            })
        );
        assert_eq!(
            execute(
                &fixture.path,
                Command::RowParity {
                    table: GraphTable::Vectors
                }
            ),
            Output::RowParity(RowParity {
                table: GraphTable::Vectors,
                row_count: None
            })
        );
        let Output::FtsParity(fts) = execute(
            &fixture.path,
            Command::FtsParity {
                table: GraphFtsTable::Nodes,
                query: "東京".to_string(),
                limit: 10,
            },
        ) else {
            panic!("FTS output expected");
        };
        assert_eq!(fts.matches.len(), 1);
        assert!(fts.matches[0].snippet.contains("<mark>東京</mark>"));

        assert_eq!(
            execute(
                &fixture.path,
                Command::SessionStoreCount {
                    family: SessionStoreFamily::Observation,
                    table: SessionStoreTable::Observations,
                }
            ),
            Output::SessionStoreCount(SessionStoreCount {
                family: SessionStoreFamily::Observation,
                table: SessionStoreTable::Observations,
                row_count: Some(2),
            })
        );
        let Output::SessionStoreSchema(message_schema) = execute(
            &fixture.path,
            Command::SessionStoreSchema {
                family: SessionStoreFamily::Transcript,
                table: SessionStoreTable::SessionMessages,
            },
        ) else {
            panic!("session-store schema output expected");
        };
        assert!(message_schema.exists);
        assert_eq!(message_schema.columns[0].name, "provider");
        assert_eq!(message_schema.foreign_keys.len(), 2);
        assert!(
            message_schema
                .foreign_keys
                .iter()
                .all(|key| { key.referenced_table == "sessions" && key.on_delete == "CASCADE" })
        );

        let Output::SessionStorePage(first_page) = execute(
            &fixture.path,
            Command::SessionStorePage {
                family: SessionStoreFamily::Observation,
                table: SessionStoreTable::Observations,
                cursor: None,
                limit: 1,
            },
        ) else {
            panic!("session-store page output expected");
        };
        assert_eq!(first_page.order_columns, ["sequence"]);
        assert_eq!(first_page.digest_algorithm, ROW_DIGEST_ALGORITHM);
        assert_eq!(first_page.rows.len(), 1);
        assert!(matches!(
            &first_page.rows[0],
            SessionStoreRow::Observations {
                sequence: 1,
                observation_id,
                payload_digest,
                row_digest,
            } if observation_id == "observation-1"
                && payload_digest == "digest-1"
                && row_digest.starts_with("sha256:")
        ));
        let mut expected_digest = CanonicalRowHasher::new();
        expected_digest.update_integer(1);
        expected_digest.update_text(b"observation-1");
        expected_digest.update_text(b"digest-1");
        expected_digest.update_text(b"receipt");
        expected_digest.update_text(b"{}");
        expected_digest.update_text(b"{}");
        assert!(matches!(
            &first_page.rows[0],
            SessionStoreRow::Observations { row_digest, .. }
                if row_digest == &expected_digest.finish()
        ));
        let Output::SessionStorePage(second_page) = execute(
            &fixture.path,
            Command::SessionStorePage {
                family: SessionStoreFamily::Observation,
                table: SessionStoreTable::Observations,
                cursor: first_page.next_cursor,
                limit: 1,
            },
        ) else {
            panic!("second session-store page output expected");
        };
        assert!(matches!(
            &second_page.rows[0],
            SessionStoreRow::Observations {
                sequence: 2,
                observation_id,
                ..
            } if observation_id == "observation-2"
        ));
        assert!(second_page.next_cursor.is_none());

        for (table, expected) in [
            (SessionStoreTable::Sessions, "sessions"),
            (SessionStoreTable::SessionMessages, "session_messages"),
            (
                SessionStoreTable::SessionSchemaMigrations,
                "session_schema_migrations",
            ),
            (SessionStoreTable::LcmRawMessages, "lcm_raw_messages"),
            (
                SessionStoreTable::SessionTemporalSchemaMigrations,
                "session_temporal_schema_migrations",
            ),
            (
                SessionStoreTable::SessionTemporalGenerations,
                "session_temporal_generations",
            ),
            (
                SessionStoreTable::SessionTemporalObservationEffects,
                "session_temporal_observation_effects",
            ),
        ] {
            let Output::SessionStorePage(page) = execute(
                &fixture.path,
                Command::SessionStorePage {
                    family: table.family(),
                    table,
                    cursor: None,
                    limit: 10,
                },
            ) else {
                panic!("session-store page expected for {table:?}");
            };
            assert!(!page.rows.is_empty(), "fixture row missing for {table:?}");
            assert_eq!(
                serde_json::to_value(&page.rows[0]).unwrap()["table"],
                expected
            );
        }
    }

    #[test]
    fn protocol_rejects_versions_options_sql_and_profile_paths() {
        let fixture = fixture();
        let mut unsupported = request_value(
            &fixture.path,
            "version",
            serde_json::json!({ "type": "metadata" }),
        );
        unsupported["protocol_version"] = serde_json::json!(2);
        assert!(matches!(
            handle_request_bytes(&serde_json::to_vec(&unsupported).unwrap()).outcome,
            ResponseOutcome::Error {
                error: ErrorPayload {
                    code: ErrorCode::UnsupportedProtocolVersion,
                    ..
                }
            }
        ));
        for invalid_command in [
            serde_json::json!({ "type": "sql", "sql": "DELETE FROM nodes" }),
            serde_json::json!({ "type": "metadata", "writable": true }),
            serde_json::json!({ "type": "row_parity", "table": "nodes; DELETE FROM nodes" }),
        ] {
            let invalid = request_value(&fixture.path, "invalid", invalid_command);
            assert!(matches!(
                handle_request_bytes(&serde_json::to_vec(&invalid).unwrap()).outcome,
                ResponseOutcome::Error {
                    error: ErrorPayload {
                        code: ErrorCode::InvalidRequest,
                        ..
                    }
                }
            ));
        }

        for (command, expected_code) in [
            (
                serde_json::json!({
                    "type": "session_store_count",
                    "family": "lcm",
                    "table": "observations"
                }),
                ErrorCode::InvalidStoreFamily,
            ),
            (
                serde_json::json!({
                    "type": "session_store_page",
                    "family": "observation",
                    "table": "observations",
                    "cursor": null,
                    "limit": 0
                }),
                ErrorCode::InvalidPageLimit,
            ),
            (
                serde_json::json!({
                    "type": "session_store_page",
                    "family": "observation",
                    "table": "observations",
                    "cursor": null,
                    "limit": 101
                }),
                ErrorCode::InvalidPageLimit,
            ),
            (
                serde_json::json!({
                    "type": "session_store_page",
                    "family": "observation",
                    "table": "observations",
                    "cursor": { "table": "lcm_raw_messages", "store_id": 1 },
                    "limit": 10
                }),
                ErrorCode::InvalidPageCursor,
            ),
        ] {
            let invalid = request_value(&fixture.path, "invalid-session-store", command);
            let ResponseOutcome::Error { error } =
                handle_request_bytes(&serde_json::to_vec(&invalid).unwrap()).outcome
            else {
                panic!("invalid session-store request unexpectedly succeeded");
            };
            assert_eq!(error.code, expected_code);
        }

        let directory = tempfile::tempdir().expect("temp profile parent");
        let profile = directory.path().join(".tracedecay");
        fs::create_dir(&profile).expect("create profile directory");
        let live = profile.join("tracedecay.db");
        fs::write(&live, b"not opened").expect("create protected file");
        let error = validate_copied_path(&live).expect_err("profile path must be rejected");
        assert_eq!(error.code, ErrorCode::RefusedLiveProfile);

        let custom_profile = directory.path().join("custom-profile-root");
        fs::create_dir(&custom_profile).expect("create custom profile root");
        let custom_live = custom_profile.join("global.db");
        fs::write(&custom_live, b"not opened").expect("create custom protected file");
        let canonical_live = fs::canonicalize(&custom_live).expect("canonical custom live path");
        let error = reject_protected_profile_path(&canonical_live, &[custom_profile])
            .expect_err("configured profile root must be rejected");
        assert_eq!(error.code, ErrorCode::RefusedLiveProfile);
    }
}
