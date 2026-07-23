//! Exhaustive physical SQL mapping for the protocol's semantic command vocabulary.

use rusqlite::types::Value;
use tracedecay_sqlite_parity_protocol::{GraphTable, SessionStoreCursor, SessionStoreTable};

pub(crate) const SET_QUERY_ONLY: &str = "PRAGMA query_only = ON";
pub(crate) const QUERY_ONLY: &str = "PRAGMA query_only";
pub(crate) const SET_FOREIGN_KEYS: &str = "PRAGMA foreign_keys = ON";
pub(crate) const SQLITE_VERSION: &str = "SELECT sqlite_version()";
pub(crate) const COMPILE_OPTIONS: &str = "PRAGMA compile_options";
pub(crate) const SCHEMA_VERSION: &str = "PRAGMA schema_version";
pub(crate) const USER_VERSION: &str = "PRAGMA user_version";
pub(crate) const FOREIGN_KEYS: &str = "PRAGMA foreign_keys";
pub(crate) const PAGE_SIZE: &str = "PRAGMA page_size";
pub(crate) const JOURNAL_MODE: &str = "PRAGMA journal_mode";
pub(crate) const QUICK_CHECK: &str = "PRAGMA quick_check(1000)";
pub(crate) const INTEGRITY_CHECK: &str = "PRAGMA integrity_check(1000)";
pub(crate) const SCHEMA_OBJECTS: &str = "
    SELECT type, name, tbl_name, sql
    FROM sqlite_schema
    WHERE type IN ('table', 'index', 'trigger', 'view')
    ORDER BY type, name
    LIMIT 10001";
pub(crate) const TABLE_EXISTS: &str = "
    SELECT EXISTS(
        SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1
    )";
pub(crate) const NODES_FTS_MATCH: &str = "
    SELECT rowid, rank, snippet(nodes_fts, 0, '<mark>', '</mark>', '…', 24)
    FROM nodes_fts
    WHERE nodes_fts MATCH ?1
    ORDER BY rank, rowid
    LIMIT ?2";

#[derive(Clone, Copy)]
pub(crate) struct TableSpec {
    pub(crate) identifier: &'static str,
    pub(crate) count_sql: &'static str,
    pub(crate) table_info_sql: Option<&'static str>,
    pub(crate) foreign_key_sql: Option<&'static str>,
}

const fn graph_table(identifier: &'static str, count_sql: &'static str) -> TableSpec {
    TableSpec {
        identifier,
        count_sql,
        table_info_sql: None,
        foreign_key_sql: None,
    }
}

const fn session_table(
    identifier: &'static str,
    count_sql: &'static str,
    table_info_sql: &'static str,
    foreign_key_sql: &'static str,
) -> TableSpec {
    TableSpec {
        identifier,
        count_sql,
        table_info_sql: Some(table_info_sql),
        foreign_key_sql: Some(foreign_key_sql),
    }
}

pub(crate) fn graph_table_spec(table: GraphTable) -> TableSpec {
    match table {
        GraphTable::Nodes => graph_table("nodes", "SELECT COUNT(*) FROM nodes"),
        GraphTable::Edges => graph_table("edges", "SELECT COUNT(*) FROM edges"),
        GraphTable::Files => graph_table("files", "SELECT COUNT(*) FROM files"),
        GraphTable::UnresolvedRefs => {
            graph_table("unresolved_refs", "SELECT COUNT(*) FROM unresolved_refs")
        }
        GraphTable::Vectors => graph_table("vectors", "SELECT COUNT(*) FROM vectors"),
        GraphTable::Metadata => graph_table("metadata", "SELECT COUNT(*) FROM metadata"),
        GraphTable::NodesFts => graph_table("nodes_fts", "SELECT COUNT(*) FROM nodes_fts"),
    }
}

pub(crate) fn session_table_spec(table: SessionStoreTable) -> TableSpec {
    match table {
        SessionStoreTable::Observations => session_table(
            "observations",
            "SELECT COUNT(*) FROM observations",
            "PRAGMA table_info(observations)",
            "PRAGMA foreign_key_list(observations)",
        ),
        SessionStoreTable::Sessions => session_table(
            "sessions",
            "SELECT COUNT(*) FROM sessions",
            "PRAGMA table_info(sessions)",
            "PRAGMA foreign_key_list(sessions)",
        ),
        SessionStoreTable::SessionMessages => session_table(
            "session_messages",
            "SELECT COUNT(*) FROM session_messages",
            "PRAGMA table_info(session_messages)",
            "PRAGMA foreign_key_list(session_messages)",
        ),
        SessionStoreTable::SessionSchemaMigrations => session_table(
            "session_schema_migrations",
            "SELECT COUNT(*) FROM session_schema_migrations",
            "PRAGMA table_info(session_schema_migrations)",
            "PRAGMA foreign_key_list(session_schema_migrations)",
        ),
        SessionStoreTable::LcmRawMessages => session_table(
            "lcm_raw_messages",
            "SELECT COUNT(*) FROM lcm_raw_messages",
            "PRAGMA table_info(lcm_raw_messages)",
            "PRAGMA foreign_key_list(lcm_raw_messages)",
        ),
        SessionStoreTable::SessionTemporalSchemaMigrations => session_table(
            "session_temporal_schema_migrations",
            "SELECT COUNT(*) FROM session_temporal_schema_migrations",
            "PRAGMA table_info(session_temporal_schema_migrations)",
            "PRAGMA foreign_key_list(session_temporal_schema_migrations)",
        ),
        SessionStoreTable::SessionTemporalGenerations => session_table(
            "session_temporal_generations",
            "SELECT COUNT(*) FROM session_temporal_generations",
            "PRAGMA table_info(session_temporal_generations)",
            "PRAGMA foreign_key_list(session_temporal_generations)",
        ),
        SessionStoreTable::SessionTemporalObservationEffects => session_table(
            "session_temporal_observation_effects",
            "SELECT COUNT(*) FROM session_temporal_observation_effects",
            "PRAGMA table_info(session_temporal_observation_effects)",
            "PRAGMA foreign_key_list(session_temporal_observation_effects)",
        ),
        SessionStoreTable::SessionTemporalProjectionReceipts => session_table(
            "session_temporal_projection_receipts",
            "SELECT COUNT(*) FROM session_temporal_projection_receipts",
            "PRAGMA table_info(session_temporal_projection_receipts)",
            "PRAGMA foreign_key_list(session_temporal_projection_receipts)",
        ),
        SessionStoreTable::SessionOccurrences => session_table(
            "session_occurrences",
            "SELECT COUNT(*) FROM session_occurrences",
            "PRAGMA table_info(session_occurrences)",
            "PRAGMA foreign_key_list(session_occurrences)",
        ),
        SessionStoreTable::SessionSummaryNodes => session_table(
            "session_summary_nodes",
            "SELECT COUNT(*) FROM session_summary_nodes",
            "PRAGMA table_info(session_summary_nodes)",
            "PRAGMA foreign_key_list(session_summary_nodes)",
        ),
    }
}

pub(crate) fn session_page_query(
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
        (SessionStoreTable::SessionTemporalProjectionReceipts, cursor) => {
            let (session_id, generation, batch_ordinal) = match cursor {
                Some(SessionStoreCursor::SessionTemporalProjectionReceipts {
                    session_id,
                    generation,
                    batch_ordinal,
                }) => (
                    Value::Text(session_id.clone()),
                    Value::Integer(*generation),
                    Value::Integer(*batch_ordinal),
                ),
                _ => (Value::Null, Value::Null, Value::Null),
            };
            (
                "SELECT session_id, generation, batch_ordinal, batch_digest,
                        frozen_watermarks_json, source_through, projection_through,
                        occurrence_count, occurrence_digest, dimension_count, dimension_digest,
                        copy_count, copy_digest, assertion_count, assertion_digest,
                        supersession_count, supersession_digest, current_count, current_digest,
                        fts_count, fts_digest, committed_at
                 FROM session_temporal_projection_receipts
                 WHERE ?1 IS NULL
                    OR session_id > ?1
                    OR (session_id = ?1 AND generation > ?2)
                    OR (session_id = ?1 AND generation = ?2 AND batch_ordinal > ?3)
                 ORDER BY session_id, generation, batch_ordinal
                 LIMIT ?4",
                vec![session_id, generation, batch_ordinal, Value::Integer(limit)],
            )
        }
        (SessionStoreTable::SessionOccurrences, cursor) => {
            let (session_id, generation, occurrence_id) = match cursor {
                Some(SessionStoreCursor::SessionOccurrences {
                    session_id,
                    generation,
                    occurrence_id,
                }) => (
                    Value::Text(session_id.clone()),
                    Value::Integer(*generation),
                    Value::Text(occurrence_id.clone()),
                ),
                _ => (Value::Null, Value::Null, Value::Null),
            };
            (
                "SELECT session_id, generation, occurrence_id, source_observation_id,
                        projection_output_ordinal, retrieval_anchor_id, thread_id,
                        thread_grouping_json, turn_id, turn_grouping_json, message_id,
                        agent_id, role, knowledge_at, valid_time_json, evidence_json,
                        snippet_text, index_text
                 FROM session_occurrences
                 WHERE ?1 IS NULL
                    OR session_id > ?1
                    OR (session_id = ?1 AND generation > ?2)
                    OR (session_id = ?1 AND generation = ?2 AND occurrence_id > ?3)
                 ORDER BY session_id, generation, occurrence_id
                 LIMIT ?4",
                vec![session_id, generation, occurrence_id, Value::Integer(limit)],
            )
        }
        (SessionStoreTable::SessionSummaryNodes, cursor) => (
            "SELECT summary_id, session_id, summary_anchor_id, summary_text, index_text,
                    source_horizon_json, publication_json, created_at
             FROM session_summary_nodes
             WHERE ?1 IS NULL OR summary_id > ?1
             ORDER BY summary_id
             LIMIT ?2",
            vec![
                match cursor {
                    Some(SessionStoreCursor::SessionSummaryNodes { summary_id }) => {
                        Value::Text(summary_id.clone())
                    }
                    _ => Value::Null,
                },
                Value::Integer(limit),
            ],
        ),
    }
}
