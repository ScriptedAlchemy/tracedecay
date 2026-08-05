use rusqlite::{Row, params_from_iter};
use tracedecay_sqlite_parity_protocol::{
    CanonicalRowHasher, ErrorCode, ErrorPayload, ROW_DIGEST_ALGORITHM, SessionStoreColumn,
    SessionStoreCount, SessionStoreCursor, SessionStoreFamily, SessionStoreForeignKey,
    SessionStorePage, SessionStoreRow, SessionStoreSchema, SessionStoreTable,
};

use crate::{closed_sql, snapshot::ReadOnlyDriver, snapshot::sqlite_query_error};

impl ReadOnlyDriver {
    pub(crate) fn session_store_count(
        &self,
        family: SessionStoreFamily,
        table: SessionStoreTable,
    ) -> Result<SessionStoreCount, ErrorPayload> {
        let spec = closed_sql::session_table_spec(table);
        let row_count = if self.table_exists(spec)? {
            Some(nonnegative_u64(
                self.count_rows(spec)?,
                spec.identifier,
                "row count",
            )?)
        } else {
            None
        };
        Ok(SessionStoreCount {
            family,
            table,
            row_count,
        })
    }

    pub(crate) fn session_store_schema(
        &self,
        family: SessionStoreFamily,
        table: SessionStoreTable,
    ) -> Result<SessionStoreSchema, ErrorPayload> {
        let spec = closed_sql::session_table_spec(table);
        if !self.table_exists(spec)? {
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
            .prepare(
                spec.table_info_sql
                    .expect("session table has table-info SQL"),
            )
            .map_err(sqlite_query_error)?;
        let columns = column_statement
            .query_map([], |row| {
                Ok(SessionStoreColumn {
                    ordinal: decode_u32(row, 0)?,
                    name: row.get(1)?,
                    declared_type: row.get(2)?,
                    not_null: row.get::<_, i64>(3)? != 0,
                    default_value: row.get(4)?,
                    primary_key_ordinal: decode_u32(row, 5)?,
                })
            })
            .map_err(sqlite_query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_query_error)?;

        let mut foreign_key_statement = self
            .connection
            .prepare(
                spec.foreign_key_sql
                    .expect("session table has foreign-key SQL"),
            )
            .map_err(sqlite_query_error)?;
        let mut foreign_keys = foreign_key_statement
            .query_map([], |row| {
                Ok(SessionStoreForeignKey {
                    id: decode_u32(row, 0)?,
                    sequence: decode_u32(row, 1)?,
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

    pub(crate) fn session_store_page(
        &self,
        family: SessionStoreFamily,
        table: SessionStoreTable,
        cursor: Option<SessionStoreCursor>,
        limit: u16,
    ) -> Result<SessionStorePage, ErrorPayload> {
        let spec = closed_sql::session_table_spec(table);
        if !self.table_exists(spec)? {
            return Ok(empty_page(family, table));
        }

        let (sql, parameters) =
            closed_sql::session_page_query(table, cursor.as_ref(), i64::from(limit) + 1);
        let mut statement = self.connection.prepare(sql).map_err(sqlite_query_error)?;
        let mut rows = statement
            .query_map(params_from_iter(parameters), |row| decode_row(table, row))
            .map_err(sqlite_query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_query_error)?;
        let has_more = rows.len() > usize::from(limit);
        rows.truncate(usize::from(limit));
        let next_cursor = if has_more {
            rows.last().map(cursor_for_row)
        } else {
            None
        };

        Ok(SessionStorePage {
            family,
            table,
            order_columns: order_columns(table),
            digest_algorithm: ROW_DIGEST_ALGORITHM.to_owned(),
            rows,
            next_cursor,
        })
    }
}

fn empty_page(family: SessionStoreFamily, table: SessionStoreTable) -> SessionStorePage {
    SessionStorePage {
        family,
        table,
        order_columns: order_columns(table),
        digest_algorithm: ROW_DIGEST_ALGORITHM.to_owned(),
        rows: Vec::new(),
        next_cursor: None,
    }
}

fn order_columns(table: SessionStoreTable) -> Vec<String> {
    table
        .order_columns()
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn decode_u32(row: &Row<'_>, index: usize) -> rusqlite::Result<u32> {
    u32::try_from(row.get::<_, i64>(index)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn decode_row(table: SessionStoreTable, row: &Row<'_>) -> rusqlite::Result<SessionStoreRow> {
    let row_digest = digest_row(row)?;
    match table {
        SessionStoreTable::Observations => Ok(SessionStoreRow::Observations {
            sequence: row.get(0)?,
            observation_id: row.get(1)?,
            payload_digest: row.get(2)?,
            row_digest,
        }),
        SessionStoreTable::SourceCursors => Ok(SessionStoreRow::SourceCursors {
            source_json: row.get(0)?,
            scope_json: row.get(1)?,
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
        SessionStoreTable::SessionTemporalProjectionReceipts => {
            Ok(SessionStoreRow::SessionTemporalProjectionReceipts {
                session_id: row.get(0)?,
                generation: row.get(1)?,
                batch_ordinal: row.get(2)?,
                batch_digest: row.get(3)?,
                row_digest,
            })
        }
        SessionStoreTable::SessionOccurrences => Ok(SessionStoreRow::SessionOccurrences {
            session_id: row.get(0)?,
            generation: row.get(1)?,
            occurrence_id: row.get(2)?,
            role: row.get(12)?,
            row_digest,
        }),
        SessionStoreTable::SessionAssertions => Ok(SessionStoreRow::SessionAssertions {
            session_id: row.get(0)?,
            generation: row.get(1)?,
            assertion_id: row.get(2)?,
            assertion_kind: row.get(3)?,
            row_digest,
        }),
        SessionStoreTable::SessionSummaryNodes => Ok(SessionStoreRow::SessionSummaryNodes {
            summary_id: row.get(0)?,
            session_id: row.get(1)?,
            summary_anchor_id: row.get(2)?,
            row_digest,
        }),
        SessionStoreTable::MemoryV2Facts => Ok(SessionStoreRow::MemoryV2Facts {
            fact_id: row.get(0)?,
            owner_kind: row.get(1)?,
            project_id: row.get(2)?,
            identity_json: row.get(4)?,
            row_digest,
        }),
        SessionStoreTable::MemoryV2CurrentFacts => Ok(SessionStoreRow::MemoryV2CurrentFacts {
            fact_id: row.get(0)?,
            owner_kind: row.get(1)?,
            project_id: row.get(2)?,
            payload_access: row.get(3)?,
            projection_state: row.get(15)?,
            row_digest,
        }),
        SessionStoreTable::MemoryV2Assertions => Ok(SessionStoreRow::MemoryV2Assertions {
            assertion_id: row.get(0)?,
            fact_id: row.get(1)?,
            owner_kind: row.get(2)?,
            project_id: row.get(3)?,
            row_digest,
        }),
        SessionStoreTable::MemoryV2LineageEvents => Ok(SessionStoreRow::MemoryV2LineageEvents {
            event_sequence: row.get(0)?,
            event_id: row.get(1)?,
            fact_id: row.get(2)?,
            row_digest,
        }),
        SessionStoreTable::RetrievalAnchors => Ok(SessionStoreRow::RetrievalAnchors {
            anchor_id: row.get(0)?,
            projection_generation: row.get(3)?,
            row_digest,
        }),
        SessionStoreTable::GenerationDiagnostics => Ok(SessionStoreRow::GenerationDiagnostics {
            diagnostic_anchor: row.get(0)?,
            generation_id: row.get(1)?,
            severity: row.get(12)?,
            record_state: row.get(22)?,
            row_digest,
        }),
        SessionStoreTable::DiagnosticGenerationPublications => {
            Ok(SessionStoreRow::DiagnosticGenerationPublications {
                generation_id: row.get(0)?,
                record_state: row.get(1)?,
                row_digest,
            })
        }
        SessionStoreTable::ConfigurationRevisions => Ok(SessionStoreRow::ConfigurationRevisions {
            revision_id: row.get(0)?,
            snapshot_id: row.get(2)?,
            operation_kind: row.get(6)?,
            row_digest,
        }),
        SessionStoreTable::ConfigurationEntries => Ok(SessionStoreRow::ConfigurationEntries {
            revision_id: row.get(0)?,
            key: row.get(1)?,
            layer_kind: row.get(2)?,
            layer_id: row.get(3)?,
            row_digest,
        }),
        SessionStoreTable::ConfigurationMutationReceipts => {
            Ok(SessionStoreRow::ConfigurationMutationReceipts {
                receipt_id: row.get(0)?,
                result_revision_id: row.get(5)?,
                activation_status: row.get(8)?,
                row_digest,
            })
        }
        SessionStoreTable::ConfigurationAuditEvents => {
            Ok(SessionStoreRow::ConfigurationAuditEvents {
                event_id: row.get(0)?,
                operation_kind: row.get(3)?,
                base_revision_id: row.get(4)?,
                row_digest,
            })
        }
    }
}

fn cursor_for_row(row: &SessionStoreRow) -> SessionStoreCursor {
    match row {
        SessionStoreRow::Observations { sequence, .. } => SessionStoreCursor::Observations {
            sequence: *sequence,
        },
        SessionStoreRow::SourceCursors {
            source_json,
            scope_json,
            ..
        } => SessionStoreCursor::SourceCursors {
            source_json: source_json.clone(),
            scope_json: scope_json.clone(),
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
        SessionStoreRow::SessionTemporalProjectionReceipts {
            session_id,
            generation,
            batch_ordinal,
            ..
        } => SessionStoreCursor::SessionTemporalProjectionReceipts {
            session_id: session_id.clone(),
            generation: *generation,
            batch_ordinal: *batch_ordinal,
        },
        SessionStoreRow::SessionOccurrences {
            session_id,
            generation,
            occurrence_id,
            ..
        } => SessionStoreCursor::SessionOccurrences {
            session_id: session_id.clone(),
            generation: *generation,
            occurrence_id: occurrence_id.clone(),
        },
        SessionStoreRow::SessionAssertions {
            session_id,
            generation,
            assertion_id,
            ..
        } => SessionStoreCursor::SessionAssertions {
            session_id: session_id.clone(),
            generation: *generation,
            assertion_id: assertion_id.clone(),
        },
        SessionStoreRow::SessionSummaryNodes { summary_id, .. } => {
            SessionStoreCursor::SessionSummaryNodes {
                summary_id: summary_id.clone(),
            }
        }
        SessionStoreRow::MemoryV2Facts {
            fact_id,
            owner_kind,
            project_id,
            ..
        } => SessionStoreCursor::MemoryV2Facts {
            fact_id: fact_id.clone(),
            owner_kind: owner_kind.clone(),
            project_id: project_id.clone(),
        },
        SessionStoreRow::MemoryV2CurrentFacts {
            fact_id,
            owner_kind,
            project_id,
            ..
        } => SessionStoreCursor::MemoryV2CurrentFacts {
            fact_id: fact_id.clone(),
            owner_kind: owner_kind.clone(),
            project_id: project_id.clone(),
        },
        SessionStoreRow::MemoryV2Assertions {
            assertion_id,
            fact_id,
            owner_kind,
            project_id,
            ..
        } => SessionStoreCursor::MemoryV2Assertions {
            assertion_id: assertion_id.clone(),
            fact_id: fact_id.clone(),
            owner_kind: owner_kind.clone(),
            project_id: project_id.clone(),
        },
        SessionStoreRow::MemoryV2LineageEvents { event_sequence, .. } => {
            SessionStoreCursor::MemoryV2LineageEvents {
                event_sequence: *event_sequence,
            }
        }
        SessionStoreRow::RetrievalAnchors { anchor_id, .. } => {
            SessionStoreCursor::RetrievalAnchors {
                anchor_id: anchor_id.clone(),
            }
        }
        SessionStoreRow::GenerationDiagnostics {
            diagnostic_anchor, ..
        } => SessionStoreCursor::GenerationDiagnostics {
            diagnostic_anchor: diagnostic_anchor.clone(),
        },
        SessionStoreRow::DiagnosticGenerationPublications { generation_id, .. } => {
            SessionStoreCursor::DiagnosticGenerationPublications {
                generation_id: generation_id.clone(),
            }
        }
        SessionStoreRow::ConfigurationRevisions { revision_id, .. } => {
            SessionStoreCursor::ConfigurationRevisions {
                revision_id: revision_id.clone(),
            }
        }
        SessionStoreRow::ConfigurationEntries {
            revision_id,
            key,
            layer_kind,
            layer_id,
            ..
        } => SessionStoreCursor::ConfigurationEntries {
            revision_id: revision_id.clone(),
            key: key.clone(),
            layer_kind: layer_kind.clone(),
            layer_id: layer_id.clone(),
        },
        SessionStoreRow::ConfigurationMutationReceipts { receipt_id, .. } => {
            SessionStoreCursor::ConfigurationMutationReceipts {
                receipt_id: receipt_id.clone(),
            }
        }
        SessionStoreRow::ConfigurationAuditEvents { event_id, .. } => {
            SessionStoreCursor::ConfigurationAuditEvents {
                event_id: event_id.clone(),
            }
        }
    }
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
