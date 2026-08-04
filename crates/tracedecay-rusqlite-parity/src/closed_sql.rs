//! Exhaustive physical SQL mapping for the protocol's semantic command vocabulary.

use rusqlite::types::Value;
use tracedecay_sqlite_parity_protocol::{SessionStoreCursor, SessionStoreTable};

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

#[derive(Clone, Copy)]
pub(crate) struct TableSpec {
    pub(crate) identifier: &'static str,
    pub(crate) count_sql: &'static str,
    pub(crate) table_info_sql: Option<&'static str>,
    pub(crate) foreign_key_sql: Option<&'static str>,
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

pub(crate) fn session_table_spec(table: SessionStoreTable) -> TableSpec {
    match table {
        SessionStoreTable::Observations => session_table(
            "observations",
            "SELECT COUNT(*) FROM observations",
            "PRAGMA table_info(observations)",
            "PRAGMA foreign_key_list(observations)",
        ),
        SessionStoreTable::SourceCursors => session_table(
            "source_cursors",
            "SELECT COUNT(*) FROM source_cursors",
            "PRAGMA table_info(source_cursors)",
            "PRAGMA foreign_key_list(source_cursors)",
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
        SessionStoreTable::SessionLogicalCopyEdges => session_table(
            "session_logical_copy_edges",
            "SELECT COUNT(*) FROM session_logical_copy_edges",
            "PRAGMA table_info(session_logical_copy_edges)",
            "PRAGMA foreign_key_list(session_logical_copy_edges)",
        ),
        SessionStoreTable::SessionAssertions => session_table(
            "session_assertions",
            "SELECT COUNT(*) FROM session_assertions",
            "PRAGMA table_info(session_assertions)",
            "PRAGMA foreign_key_list(session_assertions)",
        ),
        SessionStoreTable::SessionSummaryNodes => session_table(
            "session_summary_nodes",
            "SELECT COUNT(*) FROM session_summary_nodes",
            "PRAGMA table_info(session_summary_nodes)",
            "PRAGMA foreign_key_list(session_summary_nodes)",
        ),
        SessionStoreTable::SessionSummarySources => session_table(
            "session_summary_sources",
            "SELECT COUNT(*) FROM session_summary_sources",
            "PRAGMA table_info(session_summary_sources)",
            "PRAGMA foreign_key_list(session_summary_sources)",
        ),
        SessionStoreTable::SessionSummarySuccessors => session_table(
            "session_summary_successors",
            "SELECT COUNT(*) FROM session_summary_successors",
            "PRAGMA table_info(session_summary_successors)",
            "PRAGMA foreign_key_list(session_summary_successors)",
        ),
        SessionStoreTable::MemoryV2Facts => session_table(
            "memory_v2_facts",
            "SELECT COUNT(*) FROM memory_v2_facts",
            "PRAGMA table_info(memory_v2_facts)",
            "PRAGMA foreign_key_list(memory_v2_facts)",
        ),
        SessionStoreTable::MemoryV2CurrentFacts => session_table(
            "memory_v2_current_facts",
            "SELECT COUNT(*) FROM memory_v2_current_facts",
            "PRAGMA table_info(memory_v2_current_facts)",
            "PRAGMA foreign_key_list(memory_v2_current_facts)",
        ),
        SessionStoreTable::MemoryV2Assertions => session_table(
            "memory_v2_assertions",
            "SELECT COUNT(*) FROM memory_v2_assertions",
            "PRAGMA table_info(memory_v2_assertions)",
            "PRAGMA foreign_key_list(memory_v2_assertions)",
        ),
        SessionStoreTable::MemoryV2LineageEvents => session_table(
            "memory_v2_lineage_events",
            "SELECT COUNT(*) FROM memory_v2_lineage_events",
            "PRAGMA table_info(memory_v2_lineage_events)",
            "PRAGMA foreign_key_list(memory_v2_lineage_events)",
        ),
        SessionStoreTable::RetrievalAnchors => session_table(
            "retrieval_anchors",
            "SELECT COUNT(*) FROM retrieval_anchors",
            "PRAGMA table_info(retrieval_anchors)",
            "PRAGMA foreign_key_list(retrieval_anchors)",
        ),
        SessionStoreTable::GenerationDiagnostics => session_table(
            "generation_diagnostics",
            "SELECT COUNT(*) FROM generation_diagnostics",
            "PRAGMA table_info(generation_diagnostics)",
            "PRAGMA foreign_key_list(generation_diagnostics)",
        ),
        SessionStoreTable::DiagnosticGenerationPublications => session_table(
            "diagnostic_generation_publications",
            "SELECT COUNT(*) FROM diagnostic_generation_publications",
            "PRAGMA table_info(diagnostic_generation_publications)",
            "PRAGMA foreign_key_list(diagnostic_generation_publications)",
        ),
        SessionStoreTable::ConfigurationRevisions => session_table(
            "configuration_revisions",
            "SELECT COUNT(*) FROM configuration_revisions",
            "PRAGMA table_info(configuration_revisions)",
            "PRAGMA foreign_key_list(configuration_revisions)",
        ),
        SessionStoreTable::ConfigurationEntries => session_table(
            "configuration_entries",
            "SELECT COUNT(*) FROM configuration_entries",
            "PRAGMA table_info(configuration_entries)",
            "PRAGMA foreign_key_list(configuration_entries)",
        ),
        SessionStoreTable::ConfigurationMutationReceipts => session_table(
            "configuration_mutation_receipts",
            "SELECT COUNT(*) FROM configuration_mutation_receipts",
            "PRAGMA table_info(configuration_mutation_receipts)",
            "PRAGMA foreign_key_list(configuration_mutation_receipts)",
        ),
        SessionStoreTable::ConfigurationAuditEvents => session_table(
            "configuration_audit_events",
            "SELECT COUNT(*) FROM configuration_audit_events",
            "PRAGMA table_info(configuration_audit_events)",
            "PRAGMA foreign_key_list(configuration_audit_events)",
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
        (SessionStoreTable::SourceCursors, cursor) => {
            let (source_json, scope_json) = match cursor {
                Some(SessionStoreCursor::SourceCursors {
                    source_json,
                    scope_json,
                }) => (
                    Value::Text(source_json.clone()),
                    Value::Text(scope_json.clone()),
                ),
                _ => (Value::Null, Value::Null),
            };
            (
                "SELECT source_json, scope_json, cursor_json
                 FROM source_cursors
                 WHERE ?1 IS NULL
                    OR source_json > ?1
                    OR (source_json = ?1 AND scope_json > ?2)
                 ORDER BY source_json, scope_json
                 LIMIT ?3",
                vec![source_json, scope_json, Value::Integer(limit)],
            )
        }
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
        (SessionStoreTable::SessionLogicalCopyEdges, cursor) => {
            let (session_id, generation, occurrence_id, copied_from_occurrence_id) = match cursor {
                Some(SessionStoreCursor::SessionLogicalCopyEdges {
                    session_id,
                    generation,
                    occurrence_id,
                    copied_from_occurrence_id,
                }) => (
                    Value::Text(session_id.clone()),
                    Value::Integer(*generation),
                    Value::Text(occurrence_id.clone()),
                    Value::Text(copied_from_occurrence_id.clone()),
                ),
                _ => (Value::Null, Value::Null, Value::Null, Value::Null),
            };
            (
                "SELECT session_id, generation, occurrence_id, copied_from_occurrence_id,
                        proof_json, knowledge_at, valid_time_json, created_at
                 FROM session_logical_copy_edges
                 WHERE ?1 IS NULL
                    OR session_id > ?1
                    OR (session_id = ?1 AND generation > ?2)
                    OR (session_id = ?1 AND generation = ?2 AND occurrence_id > ?3)
                    OR (session_id = ?1 AND generation = ?2 AND occurrence_id = ?3
                        AND copied_from_occurrence_id > ?4)
                 ORDER BY session_id, generation, occurrence_id, copied_from_occurrence_id
                 LIMIT ?5",
                vec![
                    session_id,
                    generation,
                    occurrence_id,
                    copied_from_occurrence_id,
                    Value::Integer(limit),
                ],
            )
        }
        (SessionStoreTable::SessionAssertions, cursor) => {
            let (session_id, generation, assertion_id) = match cursor {
                Some(SessionStoreCursor::SessionAssertions {
                    session_id,
                    generation,
                    assertion_id,
                }) => (
                    Value::Text(session_id.clone()),
                    Value::Integer(*generation),
                    Value::Text(assertion_id.clone()),
                ),
                _ => (Value::Null, Value::Null, Value::Null),
            };
            (
                "SELECT session_id, generation, assertion_id, assertion_kind, subject_anchor_id,
                        object_anchor_id, knowledge_at, valid_time_json, evidence_json
                 FROM session_assertions
                 WHERE ?1 IS NULL
                    OR session_id > ?1
                    OR (session_id = ?1 AND generation > ?2)
                    OR (session_id = ?1 AND generation = ?2 AND assertion_id > ?3)
                 ORDER BY session_id, generation, assertion_id
                 LIMIT ?4",
                vec![session_id, generation, assertion_id, Value::Integer(limit)],
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
        (SessionStoreTable::SessionSummarySources, cursor) => {
            let (summary_id, source_ordinal) = match cursor {
                Some(SessionStoreCursor::SessionSummarySources {
                    summary_id,
                    source_ordinal,
                }) => (
                    Value::Text(summary_id.clone()),
                    Value::Integer(*source_ordinal),
                ),
                _ => (Value::Null, Value::Null),
            };
            (
                "SELECT summary_id, source_ordinal, source_kind, source_anchor_id,
                        source_summary_id
                 FROM session_summary_sources
                 WHERE ?1 IS NULL
                    OR summary_id > ?1
                    OR (summary_id = ?1 AND source_ordinal > ?2)
                 ORDER BY summary_id, source_ordinal
                 LIMIT ?3",
                vec![summary_id, source_ordinal, Value::Integer(limit)],
            )
        }
        (SessionStoreTable::SessionSummarySuccessors, cursor) => {
            let (predecessor_summary_id, successor_summary_id) = match cursor {
                Some(SessionStoreCursor::SessionSummarySuccessors {
                    predecessor_summary_id,
                    successor_summary_id,
                }) => (
                    Value::Text(predecessor_summary_id.clone()),
                    Value::Text(successor_summary_id.clone()),
                ),
                _ => (Value::Null, Value::Null),
            };
            (
                "SELECT predecessor_summary_id, successor_summary_id, created_at
                 FROM session_summary_successors
                 WHERE ?1 IS NULL
                    OR predecessor_summary_id > ?1
                    OR (predecessor_summary_id = ?1 AND successor_summary_id > ?2)
                 ORDER BY predecessor_summary_id, successor_summary_id
                 LIMIT ?3",
                vec![
                    predecessor_summary_id,
                    successor_summary_id,
                    Value::Integer(limit),
                ],
            )
        }
        (SessionStoreTable::MemoryV2Facts, cursor) => {
            let (fact_id, owner_kind, project_id) = match cursor {
                Some(SessionStoreCursor::MemoryV2Facts {
                    fact_id,
                    owner_kind,
                    project_id,
                }) => (
                    Value::Text(fact_id.clone()),
                    Value::Text(owner_kind.clone()),
                    Value::Text(project_id.clone()),
                ),
                _ => (Value::Null, Value::Null, Value::Null),
            };
            (
                "SELECT fact_id, owner_kind, project_id, owner_json, identity_json, created_at
                 FROM memory_v2_facts
                 WHERE ?1 IS NULL
                    OR fact_id > ?1
                    OR (fact_id = ?1 AND owner_kind > ?2)
                    OR (fact_id = ?1 AND owner_kind = ?2 AND project_id > ?3)
                 ORDER BY fact_id, owner_kind, project_id
                 LIMIT ?4",
                vec![fact_id, owner_kind, project_id, Value::Integer(limit)],
            )
        }
        (SessionStoreTable::MemoryV2CurrentFacts, cursor) => {
            let (fact_id, owner_kind, project_id) = match cursor {
                Some(SessionStoreCursor::MemoryV2CurrentFacts {
                    fact_id,
                    owner_kind,
                    project_id,
                }) => (
                    Value::Text(fact_id.clone()),
                    Value::Text(owner_kind.clone()),
                    Value::Text(project_id.clone()),
                ),
                _ => (Value::Null, Value::Null, Value::Null),
            };
            (
                "SELECT fact_id, owner_kind, project_id, payload_access, trust_score,
                        active_assertion_id, last_event_id, updated_at, retrieval_count,
                        access_count, helpful_count, unhelpful_count, last_retrieved_at,
                        last_recalled_at, last_feedback_at, projection_state,
                        vector_watermark_json
                 FROM memory_v2_current_facts
                 WHERE ?1 IS NULL
                    OR fact_id > ?1
                    OR (fact_id = ?1 AND owner_kind > ?2)
                    OR (fact_id = ?1 AND owner_kind = ?2 AND project_id > ?3)
                 ORDER BY fact_id, owner_kind, project_id
                 LIMIT ?4",
                vec![fact_id, owner_kind, project_id, Value::Integer(limit)],
            )
        }
        (SessionStoreTable::MemoryV2Assertions, cursor) => {
            let (assertion_id, fact_id, owner_kind, project_id) = match cursor {
                Some(SessionStoreCursor::MemoryV2Assertions {
                    assertion_id,
                    fact_id,
                    owner_kind,
                    project_id,
                }) => (
                    Value::Text(assertion_id.clone()),
                    Value::Text(fact_id.clone()),
                    Value::Text(owner_kind.clone()),
                    Value::Text(project_id.clone()),
                ),
                _ => (Value::Null, Value::Null, Value::Null, Value::Null),
            };
            (
                "SELECT assertion_id, fact_id, owner_kind, project_id, owner_json,
                        assertion_header_json, kind_json, payload_reference_json, receipt_json,
                        asserted_at, actor_id
                 FROM memory_v2_assertions
                 WHERE ?1 IS NULL
                    OR assertion_id > ?1
                    OR (assertion_id = ?1 AND fact_id > ?2)
                    OR (assertion_id = ?1 AND fact_id = ?2 AND owner_kind > ?3)
                    OR (assertion_id = ?1 AND fact_id = ?2 AND owner_kind = ?3
                        AND project_id > ?4)
                 ORDER BY assertion_id, fact_id, owner_kind, project_id
                 LIMIT ?5",
                vec![
                    assertion_id,
                    fact_id,
                    owner_kind,
                    project_id,
                    Value::Integer(limit),
                ],
            )
        }
        (SessionStoreTable::MemoryV2LineageEvents, cursor) => (
            "SELECT event_sequence, event_id, fact_id, owner_kind, project_id, event_json,
                    occurred_at, recorded_at
             FROM memory_v2_lineage_events
             WHERE event_sequence > ?1
             ORDER BY event_sequence
             LIMIT ?2",
            vec![
                Value::Integer(match cursor {
                    Some(SessionStoreCursor::MemoryV2LineageEvents { event_sequence }) => {
                        *event_sequence
                    }
                    _ => 0,
                }),
                Value::Integer(limit),
            ],
        ),
        (SessionStoreTable::RetrievalAnchors, cursor) => (
            "SELECT anchor_id, anchor_json, owner_json, projection_generation
             FROM retrieval_anchors
             WHERE ?1 IS NULL OR anchor_id > ?1
             ORDER BY anchor_id
             LIMIT ?2",
            vec![
                match cursor {
                    Some(SessionStoreCursor::RetrievalAnchors { anchor_id }) => {
                        Value::Text(anchor_id.clone())
                    }
                    _ => Value::Null,
                },
                Value::Integer(limit),
            ],
        ),
        (SessionStoreTable::GenerationDiagnostics, cursor) => (
            "SELECT diagnostic_anchor, generation_id, repository, worktree, reference,
                    source_revision, file_occurrence_id, content_digest, symbol_occurrence_id,
                    span_start, span_end, code, severity, message, message_digest,
                    producer_kind, producer, analyzer_revision, configuration_revision,
                    sanitization_receipt, evidence_class, collected_at, record_state,
                    state_generation, persisted_at
             FROM generation_diagnostics
             WHERE ?1 IS NULL OR diagnostic_anchor > ?1
             ORDER BY diagnostic_anchor
             LIMIT ?2",
            vec![
                match cursor {
                    Some(SessionStoreCursor::GenerationDiagnostics { diagnostic_anchor }) => {
                        Value::Text(diagnostic_anchor.clone())
                    }
                    _ => Value::Null,
                },
                Value::Integer(limit),
            ],
        ),
        (SessionStoreTable::DiagnosticGenerationPublications, cursor) => (
            "SELECT generation_id, record_state, state_generation, published_at
             FROM diagnostic_generation_publications
             WHERE ?1 IS NULL OR generation_id > ?1
             ORDER BY generation_id
             LIMIT ?2",
            vec![
                match cursor {
                    Some(SessionStoreCursor::DiagnosticGenerationPublications {
                        generation_id,
                    }) => Value::Text(generation_id.clone()),
                    _ => Value::Null,
                },
                Value::Integer(limit),
            ],
        ),
        (SessionStoreTable::ConfigurationRevisions, cursor) => (
            "SELECT revision_id, parent_revision_id, snapshot_id, effective_behavior_digest,
                    resolution_provenance_digest, actor_id, operation_kind, created_at
             FROM configuration_revisions
             WHERE ?1 IS NULL OR revision_id > ?1
             ORDER BY revision_id
             LIMIT ?2",
            vec![
                match cursor {
                    Some(SessionStoreCursor::ConfigurationRevisions { revision_id }) => {
                        Value::Text(revision_id.clone())
                    }
                    _ => Value::Null,
                },
                Value::Integer(limit),
            ],
        ),
        (SessionStoreTable::ConfigurationEntries, cursor) => {
            let (revision_id, key, layer_kind, layer_id) = match cursor {
                Some(SessionStoreCursor::ConfigurationEntries {
                    revision_id,
                    key,
                    layer_kind,
                    layer_id,
                }) => (
                    Value::Text(revision_id.clone()),
                    Value::Text(key.clone()),
                    Value::Text(layer_kind.clone()),
                    Value::Text(layer_id.clone()),
                ),
                _ => (Value::Null, Value::Null, Value::Null, Value::Null),
            };
            (
                "SELECT revision_id, key, layer_kind, layer_id, schema_revision, typed_value
                 FROM configuration_entries
                 WHERE ?1 IS NULL
                    OR revision_id > ?1
                    OR (revision_id = ?1 AND key > ?2)
                    OR (revision_id = ?1 AND key = ?2 AND layer_kind > ?3)
                    OR (revision_id = ?1 AND key = ?2 AND layer_kind = ?3 AND layer_id > ?4)
                 ORDER BY revision_id, key, layer_kind, layer_id
                 LIMIT ?5",
                vec![
                    revision_id,
                    key,
                    layer_kind,
                    layer_id,
                    Value::Integer(limit),
                ],
            )
        }
        (SessionStoreTable::ConfigurationMutationReceipts, cursor) => (
            "SELECT receipt_id, plan_id, actor_id, idempotency_key, base_revision_id,
                    result_revision_id, operation_digest, authorization_policy_digest,
                    activation_status, receipt_digest, created_at
             FROM configuration_mutation_receipts
             WHERE ?1 IS NULL OR receipt_id > ?1
             ORDER BY receipt_id
             LIMIT ?2",
            vec![
                match cursor {
                    Some(SessionStoreCursor::ConfigurationMutationReceipts { receipt_id }) => {
                        Value::Text(receipt_id.clone())
                    }
                    _ => Value::Null,
                },
                Value::Integer(limit),
            ],
        ),
        (SessionStoreTable::ConfigurationAuditEvents, cursor) => (
            "SELECT event_id, actor_id, idempotency_key, operation_kind, base_revision_id,
                    result_revision_id, sealed_target_reference,
                    event_scoped_target_commitment, receipt_digest, correlation_id,
                    safe_reason_code, occurred_at
             FROM configuration_audit_events
             WHERE ?1 IS NULL OR event_id > ?1
             ORDER BY event_id
             LIMIT ?2",
            vec![
                match cursor {
                    Some(SessionStoreCursor::ConfigurationAuditEvents { event_id }) => {
                        Value::Text(event_id.clone())
                    }
                    _ => Value::Null,
                },
                Value::Integer(limit),
            ],
        ),
    }
}
