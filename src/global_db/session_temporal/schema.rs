use libsql::{Connection, params};

use crate::global_db::{global_db_operation_error, global_db_operation_message};

const OPERATION: &str = "initialize session temporal schema";
const MIGRATION_NAME: &str = "session-temporal";
pub(super) const SESSION_TEMPORAL_SCHEMA_VERSION: i64 = 3;

const TEMPORAL_FTS_CONTRACTS: &[(&str, &str)] = &[
    (
        "session_occurrences_fts",
        "createvirtualtablesession_occurrences_ftsusingfts5(index_text,snippet_text,content='session_occurrences',content_rowid='rowid')",
    ),
    (
        "session_summary_nodes_fts",
        "createvirtualtablesession_summary_nodes_ftsusingfts5(summary_text,index_text,content='session_summary_nodes',content_rowid='rowid')",
    ),
];

const TEMPORAL_SCHEMA_DDL: &str = r#"
    CREATE TABLE IF NOT EXISTS session_temporal_schema_migrations (
        name TEXT PRIMARY KEY,
        version INTEGER NOT NULL CHECK(version > 0),
        applied_at INTEGER NOT NULL
    );
    -- These duplicate exact primary-key or unique-key prefixes. Execute the
    -- drops on every pre-live reopen so existing schemas converge as well.
    DROP INDEX IF EXISTS idx_session_refresh_progress_operation;
    DROP INDEX IF EXISTS idx_session_temporal_projection_receipts_digest;

    CREATE TABLE IF NOT EXISTS session_summary_nodes (
        summary_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        summary_anchor_id TEXT NOT NULL,
        summary_text TEXT NOT NULL,
        index_text TEXT NOT NULL,
        source_horizon_json TEXT NOT NULL CHECK(json_valid(source_horizon_json)),
        publication_json TEXT CHECK(publication_json IS NULL OR json_valid(publication_json)),
        created_at INTEGER NOT NULL,
        FOREIGN KEY(summary_anchor_id) REFERENCES retrieval_anchors(anchor_id)
    );
    CREATE INDEX IF NOT EXISTS idx_session_summary_nodes_session_created
        ON session_summary_nodes(session_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_session_summary_nodes_root_created_order
        ON session_summary_nodes(created_at, session_id, summary_id);

    CREATE TABLE IF NOT EXISTS session_summary_sources (
        summary_id TEXT NOT NULL,
        source_ordinal INTEGER NOT NULL CHECK(source_ordinal >= 0),
        source_kind TEXT NOT NULL CHECK(source_kind IN ('anchor', 'summary')),
        source_anchor_id TEXT,
        source_summary_id TEXT,
        PRIMARY KEY(summary_id, source_ordinal),
        CHECK(
            (source_kind = 'anchor' AND source_anchor_id IS NOT NULL AND source_summary_id IS NULL)
            OR (source_kind = 'summary' AND source_anchor_id IS NULL AND source_summary_id IS NOT NULL)
        ),
        FOREIGN KEY(summary_id) REFERENCES session_summary_nodes(summary_id) ON DELETE CASCADE,
        FOREIGN KEY(source_anchor_id) REFERENCES retrieval_anchors(anchor_id),
        FOREIGN KEY(source_summary_id) REFERENCES session_summary_nodes(summary_id)
    );
    CREATE INDEX IF NOT EXISTS idx_session_summary_sources_anchor
        ON session_summary_sources(source_anchor_id);
    CREATE INDEX IF NOT EXISTS idx_session_summary_sources_summary
        ON session_summary_sources(source_summary_id, summary_id);

    CREATE TABLE IF NOT EXISTS session_summary_successors (
        predecessor_summary_id TEXT NOT NULL,
        successor_summary_id TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY(predecessor_summary_id, successor_summary_id),
        CHECK(predecessor_summary_id <> successor_summary_id),
        FOREIGN KEY(predecessor_summary_id) REFERENCES session_summary_nodes(summary_id),
        FOREIGN KEY(successor_summary_id) REFERENCES session_summary_nodes(summary_id)
    );
    CREATE INDEX IF NOT EXISTS idx_session_summary_successors_successor
        ON session_summary_successors(successor_summary_id, created_at, predecessor_summary_id);

    CREATE TABLE IF NOT EXISTS session_external_payload_manifests (
        payload_ref TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        payload_digest TEXT NOT NULL,
        manifest_json TEXT NOT NULL CHECK(json_valid(manifest_json)),
        receipt_id TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
    );
    CREATE INDEX IF NOT EXISTS idx_session_external_payload_manifests_session
        ON session_external_payload_manifests(session_id);

    CREATE TABLE IF NOT EXISTS session_refresh_operations (
        session_id TEXT NOT NULL,
        operation_id TEXT NOT NULL,
        request_digest TEXT NOT NULL,
        target_frontier_json TEXT NOT NULL CHECK(json_valid(target_frontier_json)),
        state TEXT NOT NULL CHECK(state IN ('running', 'complete', 'failed', 'cancelled')),
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        terminal_at INTEGER,
        failure_code TEXT,
        PRIMARY KEY(session_id, operation_id),
        CHECK(
            (state = 'running' AND terminal_at IS NULL AND failure_code IS NULL)
            OR (state = 'complete' AND terminal_at IS NOT NULL AND failure_code IS NULL)
            OR (state = 'failed' AND terminal_at IS NOT NULL AND failure_code IS NOT NULL)
            OR (state = 'cancelled' AND terminal_at IS NOT NULL AND failure_code IS NULL)
        )
    );
    CREATE INDEX IF NOT EXISTS idx_session_refresh_operations_join
        ON session_refresh_operations(session_id, request_digest, state);
    CREATE INDEX IF NOT EXISTS idx_session_refresh_operations_state
        ON session_refresh_operations(state, updated_at);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_session_refresh_operations_one_running
        ON session_refresh_operations(session_id) WHERE state = 'running';

    CREATE TABLE IF NOT EXISTS session_refresh_bindings (
        session_id TEXT NOT NULL,
        operation_id TEXT NOT NULL,
        scope_kind TEXT NOT NULL CHECK(scope_kind = 'session_store'),
        source_frontier INTEGER NOT NULL CHECK(source_frontier >= 0),
        target_frontier INTEGER NOT NULL CHECK(target_frontier >= source_frontier),
        projector_version TEXT NOT NULL,
        config_digest TEXT NOT NULL,
        generation INTEGER NOT NULL CHECK(generation > 0),
        frozen_watermarks_json TEXT NOT NULL CHECK(json_valid(frozen_watermarks_json)),
        binding_digest TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY(session_id, operation_id),
        UNIQUE(session_id, generation),
        FOREIGN KEY(session_id, operation_id)
            REFERENCES session_refresh_operations(session_id, operation_id) ON DELETE CASCADE,
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE
    );

    CREATE TABLE IF NOT EXISTS session_refresh_progress (
        session_id TEXT NOT NULL,
        operation_id TEXT NOT NULL,
        progress_ordinal INTEGER NOT NULL CHECK(progress_ordinal >= 0),
        frontier_json TEXT NOT NULL CHECK(json_valid(frontier_json)),
        coverage_json TEXT NOT NULL CHECK(json_valid(coverage_json)),
        committed_batches INTEGER NOT NULL CHECK(committed_batches >= 0),
        committed_records INTEGER NOT NULL CHECK(committed_records >= 0),
        recorded_at INTEGER NOT NULL,
        PRIMARY KEY(session_id, operation_id, progress_ordinal),
        FOREIGN KEY(session_id, operation_id)
            REFERENCES session_refresh_operations(session_id, operation_id) ON DELETE CASCADE
    );

    CREATE TABLE IF NOT EXISTS session_refresh_batch_bindings (
        session_id TEXT NOT NULL,
        operation_id TEXT NOT NULL,
        progress_ordinal INTEGER NOT NULL CHECK(progress_ordinal >= 0),
        generation INTEGER NOT NULL CHECK(generation > 0),
        batch_ordinal INTEGER NOT NULL CHECK(batch_ordinal >= 0),
        PRIMARY KEY(session_id, operation_id, progress_ordinal),
        UNIQUE(session_id, generation, batch_ordinal),
        FOREIGN KEY(session_id, operation_id, progress_ordinal)
            REFERENCES session_refresh_progress(
                session_id, operation_id, progress_ordinal
            ) ON DELETE CASCADE,
        FOREIGN KEY(session_id, generation, batch_ordinal)
            REFERENCES session_temporal_projection_receipts(
                session_id, generation, batch_ordinal
            ) ON DELETE CASCADE
    );

    CREATE TABLE IF NOT EXISTS session_refresh_receipts (
        session_id TEXT NOT NULL,
        operation_id TEXT NOT NULL,
        terminal_state TEXT NOT NULL CHECK(terminal_state IN ('complete', 'failed', 'cancelled')),
        frontier_json TEXT NOT NULL CHECK(json_valid(frontier_json)),
        coverage_json TEXT NOT NULL CHECK(json_valid(coverage_json)),
        failure_code TEXT,
        terminal_at INTEGER NOT NULL,
        PRIMARY KEY(session_id, operation_id),
        CHECK(
            (terminal_state = 'failed' AND failure_code IS NOT NULL)
            OR (terminal_state IN ('complete', 'cancelled') AND failure_code IS NULL)
        ),
        FOREIGN KEY(session_id, operation_id)
            REFERENCES session_refresh_operations(session_id, operation_id) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_session_refresh_receipts_session
        ON session_refresh_receipts(session_id, terminal_at);

    -- The schema deliberately creates no key row. Daemon-owned key rotation
    -- appends an authenticated version; SQLite cannot prove the caller's identity.
    CREATE TABLE IF NOT EXISTS session_query_cursor_keys (
        key_id TEXT PRIMARY KEY,
        key_version INTEGER NOT NULL UNIQUE CHECK(key_version > 0),
        key_material BLOB NOT NULL,
        created_at INTEGER NOT NULL,
        retired_at INTEGER CHECK(retired_at IS NULL OR retired_at >= created_at)
    );
    CREATE INDEX IF NOT EXISTS idx_session_query_cursor_keys_active
        ON session_query_cursor_keys(retired_at, key_version);

    CREATE TABLE IF NOT EXISTS session_temporal_generations (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL CHECK(generation > 0),
        state TEXT NOT NULL CHECK(state IN ('building', 'ready', 'active', 'superseded', 'failed', 'cancelled')),
        frozen_watermarks_json TEXT NOT NULL CHECK(json_valid(frozen_watermarks_json)),
        created_at INTEGER NOT NULL,
        ready_at INTEGER,
        activated_at INTEGER,
        completed_at INTEGER,
        PRIMARY KEY(session_id, generation),
        CHECK(
            (state = 'building' AND ready_at IS NULL AND activated_at IS NULL AND completed_at IS NULL)
            OR (state = 'ready' AND ready_at IS NOT NULL AND activated_at IS NULL AND completed_at IS NULL)
            OR (state = 'active' AND ready_at IS NOT NULL AND activated_at IS NOT NULL AND completed_at IS NULL)
            OR (state = 'superseded' AND ready_at IS NOT NULL AND activated_at IS NOT NULL AND completed_at IS NOT NULL)
            OR (state IN ('failed', 'cancelled') AND completed_at IS NOT NULL)
        )
    );
    CREATE INDEX IF NOT EXISTS idx_session_temporal_generations_session_state
        ON session_temporal_generations(session_id, state);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_session_temporal_generations_one_active
        ON session_temporal_generations(session_id)
        WHERE state = 'active';

    CREATE TABLE IF NOT EXISTS session_temporal_projection_receipts (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        batch_ordinal INTEGER NOT NULL CHECK(batch_ordinal >= 0),
        batch_digest TEXT NOT NULL,
        frozen_watermarks_json TEXT NOT NULL CHECK(json_valid(frozen_watermarks_json)),
        source_through INTEGER NOT NULL CHECK(source_through >= 0),
        projection_through INTEGER NOT NULL CHECK(projection_through >= 0),
        occurrence_count INTEGER NOT NULL CHECK(occurrence_count >= 0),
        occurrence_digest TEXT NOT NULL,
        dimension_count INTEGER NOT NULL CHECK(dimension_count >= 0),
        dimension_digest TEXT NOT NULL,
        copy_count INTEGER NOT NULL CHECK(copy_count >= 0),
        copy_digest TEXT NOT NULL,
        assertion_count INTEGER NOT NULL CHECK(assertion_count >= 0),
        assertion_digest TEXT NOT NULL,
        supersession_count INTEGER NOT NULL CHECK(supersession_count >= 0),
        supersession_digest TEXT NOT NULL,
        current_count INTEGER NOT NULL CHECK(current_count >= 0),
        current_digest TEXT NOT NULL,
        fts_count INTEGER NOT NULL CHECK(fts_count >= 0),
        fts_digest TEXT NOT NULL,
        committed_at INTEGER NOT NULL,
        PRIMARY KEY(session_id, generation, batch_ordinal),
        UNIQUE(session_id, generation, batch_digest),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE
    );

    CREATE TABLE IF NOT EXISTS session_temporal_observation_effects (
        observation_id TEXT PRIMARY KEY,
        observation_sequence INTEGER NOT NULL UNIQUE CHECK(observation_sequence > 0),
        session_id TEXT NOT NULL,
        receipt_id TEXT NOT NULL,
        effect_digest TEXT NOT NULL,
        output_count INTEGER NOT NULL CHECK(output_count >= 0),
        recorded_at INTEGER NOT NULL,
        FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
        FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
    );
    CREATE INDEX IF NOT EXISTS idx_session_temporal_observation_effects_session
        ON session_temporal_observation_effects(session_id, observation_sequence);

    CREATE TABLE IF NOT EXISTS session_turns (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        turn_id TEXT NOT NULL,
        ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
        grouping_provenance TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY(session_id, generation, turn_id),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE
    );

    CREATE TABLE IF NOT EXISTS session_threads (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        thread_id TEXT NOT NULL,
        grouping_provenance TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY(session_id, generation, thread_id),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE
    );

    CREATE TABLE IF NOT EXISTS session_agents (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        agent_id TEXT NOT NULL,
        agent_json TEXT NOT NULL CHECK(json_valid(agent_json)),
        created_at INTEGER NOT NULL,
        PRIMARY KEY(session_id, generation, agent_id),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE
    );

    CREATE TABLE IF NOT EXISTS session_occurrences (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        occurrence_id TEXT NOT NULL,
        source_observation_id TEXT NOT NULL,
        projection_output_ordinal INTEGER NOT NULL CHECK(projection_output_ordinal >= 0),
        retrieval_anchor_id TEXT NOT NULL,
        thread_id TEXT,
        thread_grouping_json TEXT CHECK(thread_grouping_json IS NULL OR json_valid(thread_grouping_json)),
        turn_id TEXT,
        turn_grouping_json TEXT CHECK(turn_grouping_json IS NULL OR json_valid(turn_grouping_json)),
        message_id TEXT,
        agent_id TEXT,
        role TEXT NOT NULL,
        knowledge_at INTEGER NOT NULL,
        valid_time_json TEXT NOT NULL CHECK(
            json_valid(valid_time_json)
            AND json_type(valid_time_json, '$.kind') IS 'text'
            AND (
                (
                    json_extract(valid_time_json, '$.kind') = 'unknown'
                    AND json_type(valid_time_json, '$.valid_at') IS NULL
                )
                OR (
                    json_extract(valid_time_json, '$.kind') = 'known'
                    AND json_type(valid_time_json, '$.valid_at') IS 'integer'
                )
            )
        ),
        evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
        snippet_text TEXT NOT NULL,
        index_text TEXT NOT NULL,
        PRIMARY KEY(session_id, generation, occurrence_id),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE,
        FOREIGN KEY(source_observation_id) REFERENCES observations(observation_id),
        FOREIGN KEY(retrieval_anchor_id) REFERENCES retrieval_anchors(anchor_id),
        FOREIGN KEY(session_id, generation, thread_id)
            REFERENCES session_threads(session_id, generation, thread_id),
        FOREIGN KEY(session_id, generation, turn_id)
            REFERENCES session_turns(session_id, generation, turn_id),
        FOREIGN KEY(session_id, generation, agent_id)
            REFERENCES session_agents(session_id, generation, agent_id)
    );
    CREATE INDEX IF NOT EXISTS idx_session_occurrences_generation_order
        ON session_occurrences(session_id, generation, knowledge_at, occurrence_id);
    CREATE INDEX IF NOT EXISTS idx_session_occurrences_root_generation_order
        ON session_occurrences(knowledge_at, session_id, occurrence_id, generation);
    CREATE INDEX IF NOT EXISTS idx_session_occurrences_session_time
        ON session_occurrences(session_id, knowledge_at);
    CREATE INDEX IF NOT EXISTS idx_session_occurrences_anchor_order
        ON session_occurrences(
            session_id, generation, retrieval_anchor_id, knowledge_at, occurrence_id
        );
    CREATE INDEX IF NOT EXISTS idx_session_occurrences_message
        ON session_occurrences(session_id, generation, message_id, knowledge_at, occurrence_id);
    CREATE INDEX IF NOT EXISTS idx_session_occurrences_thread
        ON session_occurrences(session_id, generation, thread_id, knowledge_at, occurrence_id);
    CREATE INDEX IF NOT EXISTS idx_session_occurrences_turn
        ON session_occurrences(session_id, generation, turn_id, knowledge_at, occurrence_id);
    CREATE INDEX IF NOT EXISTS idx_session_occurrences_agent
        ON session_occurrences(session_id, generation, agent_id, knowledge_at, occurrence_id);

    CREATE TABLE IF NOT EXISTS session_logical_copy_edges (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        occurrence_id TEXT NOT NULL,
        copied_from_occurrence_id TEXT NOT NULL,
        proof_json TEXT NOT NULL CHECK(json_valid(proof_json)),
        knowledge_at INTEGER NOT NULL,
        valid_time_json TEXT NOT NULL CHECK(
            json_valid(valid_time_json)
            AND json_type(valid_time_json, '$.kind') IS 'text'
            AND (
                (
                    json_extract(valid_time_json, '$.kind') = 'unknown'
                    AND json_type(valid_time_json, '$.valid_at') IS NULL
                )
                OR (
                    json_extract(valid_time_json, '$.kind') = 'known'
                    AND json_type(valid_time_json, '$.valid_at') IS 'integer'
                )
            )
        ),
        created_at INTEGER NOT NULL,
        PRIMARY KEY(session_id, generation, occurrence_id, copied_from_occurrence_id),
        CHECK(occurrence_id <> copied_from_occurrence_id),
        FOREIGN KEY(session_id, generation, occurrence_id)
            REFERENCES session_occurrences(session_id, generation, occurrence_id) ON DELETE CASCADE,
        FOREIGN KEY(session_id, generation, copied_from_occurrence_id)
            REFERENCES session_occurrences(session_id, generation, occurrence_id) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_session_logical_copy_edges_target
        ON session_logical_copy_edges(session_id, generation, copied_from_occurrence_id);

    CREATE TABLE IF NOT EXISTS session_turn_members (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        turn_id TEXT NOT NULL,
        occurrence_id TEXT NOT NULL,
        ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
        PRIMARY KEY(session_id, generation, turn_id, occurrence_id),
        FOREIGN KEY(session_id, generation, turn_id)
            REFERENCES session_turns(session_id, generation, turn_id) ON DELETE CASCADE,
        FOREIGN KEY(session_id, generation, occurrence_id)
            REFERENCES session_occurrences(session_id, generation, occurrence_id) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_session_turn_members_occurrence
        ON session_turn_members(session_id, generation, occurrence_id);

    CREATE TABLE IF NOT EXISTS session_thread_hierarchy_edges (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        parent_thread_id TEXT NOT NULL,
        child_thread_id TEXT NOT NULL,
        ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
        PRIMARY KEY(session_id, generation, parent_thread_id, child_thread_id),
        CHECK(parent_thread_id <> child_thread_id),
        FOREIGN KEY(session_id, generation, parent_thread_id)
            REFERENCES session_threads(session_id, generation, thread_id) ON DELETE CASCADE,
        FOREIGN KEY(session_id, generation, child_thread_id)
            REFERENCES session_threads(session_id, generation, thread_id) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_session_thread_hierarchy_edges_child
        ON session_thread_hierarchy_edges(session_id, generation, child_thread_id);

    CREATE TABLE IF NOT EXISTS session_agent_hierarchy_edges (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        parent_agent_id TEXT NOT NULL,
        child_agent_id TEXT NOT NULL,
        ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
        PRIMARY KEY(session_id, generation, parent_agent_id, child_agent_id),
        CHECK(parent_agent_id <> child_agent_id),
        FOREIGN KEY(session_id, generation, parent_agent_id)
            REFERENCES session_agents(session_id, generation, agent_id) ON DELETE CASCADE,
        FOREIGN KEY(session_id, generation, child_agent_id)
            REFERENCES session_agents(session_id, generation, agent_id) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_session_agent_hierarchy_edges_child
        ON session_agent_hierarchy_edges(session_id, generation, child_agent_id);

    CREATE TABLE IF NOT EXISTS session_assertions (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        assertion_id TEXT NOT NULL,
        assertion_kind TEXT NOT NULL CHECK(
            assertion_kind IN ('corrects', 'supersedes', 'contradicts', 'supports')
        ),
        subject_anchor_id TEXT NOT NULL,
        object_anchor_id TEXT NOT NULL,
        knowledge_at INTEGER NOT NULL,
        valid_time_json TEXT NOT NULL CHECK(
            json_valid(valid_time_json)
            AND json_type(valid_time_json, '$.kind') IS 'text'
            AND (
                (
                    json_extract(valid_time_json, '$.kind') = 'unknown'
                    AND json_type(valid_time_json, '$.valid_at') IS NULL
                )
                OR (
                    json_extract(valid_time_json, '$.kind') = 'known'
                    AND json_type(valid_time_json, '$.valid_at') IS 'integer'
                )
            )
        ),
        evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
        PRIMARY KEY(session_id, generation, assertion_id),
        CHECK(subject_anchor_id <> object_anchor_id),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE,
        FOREIGN KEY(subject_anchor_id) REFERENCES retrieval_anchors(anchor_id),
        FOREIGN KEY(object_anchor_id) REFERENCES retrieval_anchors(anchor_id)
    );
    CREATE INDEX IF NOT EXISTS idx_session_assertions_subject
        ON session_assertions(session_id, generation, subject_anchor_id);
    CREATE INDEX IF NOT EXISTS idx_session_assertions_object_order
        ON session_assertions(
            session_id, generation, object_anchor_id, knowledge_at, assertion_id
        );
    CREATE INDEX IF NOT EXISTS idx_session_assertions_kind_order
        ON session_assertions(session_id, generation, assertion_kind, knowledge_at, assertion_id);
    CREATE INDEX IF NOT EXISTS idx_session_assertions_generation_order
        ON session_assertions(session_id, generation, knowledge_at, assertion_id);

    CREATE TABLE IF NOT EXISTS session_assertion_supersession (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        superseded_assertion_id TEXT NOT NULL,
        superseding_assertion_id TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY(
            session_id, generation, superseded_assertion_id, superseding_assertion_id
        ),
        CHECK(superseded_assertion_id <> superseding_assertion_id),
        FOREIGN KEY(session_id, generation, superseded_assertion_id)
            REFERENCES session_assertions(session_id, generation, assertion_id) ON DELETE CASCADE,
        FOREIGN KEY(session_id, generation, superseding_assertion_id)
            REFERENCES session_assertions(session_id, generation, assertion_id) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_session_assertion_supersession_successor
        ON session_assertion_supersession(session_id, generation, superseding_assertion_id);

    CREATE TABLE IF NOT EXISTS session_current_entities (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        entity_kind TEXT NOT NULL,
        entity_id TEXT NOT NULL,
        current_assertion_id TEXT,
        current_occurrence_id TEXT,
        coverage_json TEXT NOT NULL CHECK(json_valid(coverage_json)),
        PRIMARY KEY(session_id, generation, entity_kind, entity_id),
        CHECK(entity_kind IN ('assertion_anchor', 'occurrence_anchor')),
        CHECK((current_assertion_id IS NULL) <> (current_occurrence_id IS NULL)),
        CHECK(
            (entity_kind = 'assertion_anchor' AND current_assertion_id IS NOT NULL)
            OR (entity_kind = 'occurrence_anchor' AND current_occurrence_id IS NOT NULL)
        ),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE,
        FOREIGN KEY(session_id, generation, current_assertion_id)
            REFERENCES session_assertions(session_id, generation, assertion_id),
        FOREIGN KEY(session_id, generation, current_occurrence_id)
            REFERENCES session_occurrences(session_id, generation, occurrence_id)
    );
    CREATE INDEX IF NOT EXISTS idx_session_current_entities_assertion
        ON session_current_entities(session_id, generation, current_assertion_id);
    CREATE INDEX IF NOT EXISTS idx_session_current_entities_occurrence
        ON session_current_entities(session_id, generation, current_occurrence_id);

    CREATE TABLE IF NOT EXISTS session_summary_availability (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        summary_id TEXT NOT NULL,
        availability TEXT NOT NULL CHECK(availability IN ('available', 'unavailable', 'stale')),
        source_horizon_json TEXT NOT NULL CHECK(json_valid(source_horizon_json)),
        reason TEXT,
        checked_at INTEGER NOT NULL,
        PRIMARY KEY(session_id, generation, summary_id),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE,
        FOREIGN KEY(summary_id) REFERENCES session_summary_nodes(summary_id)
    );
    CREATE INDEX IF NOT EXISTS idx_session_summary_availability_generation
        ON session_summary_availability(session_id, generation, availability);

    CREATE TABLE IF NOT EXISTS session_temporal_migration_receipts (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        batch_ordinal INTEGER NOT NULL CHECK(batch_ordinal >= 0),
        source_digest TEXT NOT NULL,
        frozen_watermarks_json TEXT NOT NULL CHECK(json_valid(frozen_watermarks_json)),
        imported_items INTEGER NOT NULL CHECK(imported_items >= 0),
        committed_at INTEGER NOT NULL,
        PRIMARY KEY(session_id, generation, batch_ordinal),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_session_temporal_migration_receipts_source
        ON session_temporal_migration_receipts(session_id, source_digest, generation);


    CREATE TABLE IF NOT EXISTS session_temporal_migration_dispositions (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        batch_ordinal INTEGER NOT NULL CHECK(batch_ordinal >= 0),
        disposition_ordinal INTEGER NOT NULL CHECK(disposition_ordinal >= 0),
        provider TEXT NOT NULL,
        message_id TEXT NOT NULL,
        output_ordinal INTEGER NOT NULL CHECK(output_ordinal >= 0),
        observation_id TEXT,
        retrieval_anchor_id TEXT,
        disposition TEXT NOT NULL CHECK(disposition IN (
            'eligible', 'quarantined', 'policy_excluded', 'unbound', 'ineligible'
        )),
        reason TEXT NOT NULL,
        row_digest TEXT NOT NULL,
        PRIMARY KEY(session_id, generation, batch_ordinal, disposition_ordinal),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_session_temporal_migration_dispositions_row
        ON session_temporal_migration_dispositions(
            session_id, provider, message_id, output_ordinal
        );
    CREATE INDEX IF NOT EXISTS idx_session_temporal_migration_dispositions_kind
        ON session_temporal_migration_dispositions(session_id, disposition, generation);

    CREATE VIRTUAL TABLE IF NOT EXISTS session_occurrences_fts USING fts5(
        index_text,
        snippet_text,
        content='session_occurrences',
        content_rowid='rowid'
    );
    CREATE VIRTUAL TABLE IF NOT EXISTS session_summary_nodes_fts USING fts5(
        summary_text,
        index_text,
        content='session_summary_nodes',
        content_rowid='rowid'
    );
"#;

pub(super) const TEMPORAL_TABLE_COLUMNS: &[(&str, &[&str])] = &[
    (
        "session_temporal_schema_migrations",
        &["name", "version", "applied_at"],
    ),
    (
        "session_summary_nodes",
        &[
            "summary_id",
            "session_id",
            "summary_anchor_id",
            "summary_text",
            "index_text",
            "source_horizon_json",
            "publication_json",
            "created_at",
        ],
    ),
    (
        "session_summary_sources",
        &[
            "summary_id",
            "source_ordinal",
            "source_kind",
            "source_anchor_id",
            "source_summary_id",
        ],
    ),
    (
        "session_summary_successors",
        &[
            "predecessor_summary_id",
            "successor_summary_id",
            "created_at",
        ],
    ),
    (
        "session_external_payload_manifests",
        &[
            "payload_ref",
            "session_id",
            "payload_digest",
            "manifest_json",
            "receipt_id",
            "created_at",
        ],
    ),
    (
        "session_refresh_operations",
        &[
            "session_id",
            "operation_id",
            "request_digest",
            "target_frontier_json",
            "state",
            "created_at",
            "updated_at",
            "terminal_at",
            "failure_code",
        ],
    ),
    (
        "session_refresh_bindings",
        &[
            "session_id",
            "operation_id",
            "scope_kind",
            "source_frontier",
            "target_frontier",
            "projector_version",
            "config_digest",
            "generation",
            "frozen_watermarks_json",
            "binding_digest",
            "created_at",
        ],
    ),
    (
        "session_refresh_progress",
        &[
            "session_id",
            "operation_id",
            "progress_ordinal",
            "frontier_json",
            "coverage_json",
            "committed_batches",
            "committed_records",
            "recorded_at",
        ],
    ),
    (
        "session_refresh_batch_bindings",
        &[
            "session_id",
            "operation_id",
            "progress_ordinal",
            "generation",
            "batch_ordinal",
        ],
    ),
    (
        "session_refresh_receipts",
        &[
            "session_id",
            "operation_id",
            "terminal_state",
            "frontier_json",
            "coverage_json",
            "failure_code",
            "terminal_at",
        ],
    ),
    (
        "session_query_cursor_keys",
        &[
            "key_id",
            "key_version",
            "key_material",
            "created_at",
            "retired_at",
        ],
    ),
    (
        "session_temporal_generations",
        &[
            "session_id",
            "generation",
            "state",
            "frozen_watermarks_json",
            "created_at",
            "ready_at",
            "activated_at",
            "completed_at",
        ],
    ),
    (
        "session_temporal_projection_receipts",
        &[
            "session_id",
            "generation",
            "batch_ordinal",
            "batch_digest",
            "frozen_watermarks_json",
            "source_through",
            "projection_through",
            "occurrence_count",
            "occurrence_digest",
            "dimension_count",
            "dimension_digest",
            "copy_count",
            "copy_digest",
            "assertion_count",
            "assertion_digest",
            "supersession_count",
            "supersession_digest",
            "current_count",
            "current_digest",
            "fts_count",
            "fts_digest",
            "committed_at",
        ],
    ),
    (
        "session_temporal_observation_effects",
        &[
            "observation_id",
            "observation_sequence",
            "session_id",
            "receipt_id",
            "effect_digest",
            "output_count",
            "recorded_at",
        ],
    ),
    (
        "session_turns",
        &[
            "session_id",
            "generation",
            "turn_id",
            "ordinal",
            "grouping_provenance",
            "created_at",
        ],
    ),
    (
        "session_threads",
        &[
            "session_id",
            "generation",
            "thread_id",
            "grouping_provenance",
            "created_at",
        ],
    ),
    (
        "session_agents",
        &[
            "session_id",
            "generation",
            "agent_id",
            "agent_json",
            "created_at",
        ],
    ),
    (
        "session_occurrences",
        &[
            "session_id",
            "generation",
            "occurrence_id",
            "source_observation_id",
            "projection_output_ordinal",
            "retrieval_anchor_id",
            "thread_id",
            "thread_grouping_json",
            "turn_id",
            "turn_grouping_json",
            "message_id",
            "agent_id",
            "role",
            "knowledge_at",
            "valid_time_json",
            "evidence_json",
            "snippet_text",
            "index_text",
        ],
    ),
    (
        "session_logical_copy_edges",
        &[
            "session_id",
            "generation",
            "occurrence_id",
            "copied_from_occurrence_id",
            "proof_json",
            "knowledge_at",
            "valid_time_json",
            "created_at",
        ],
    ),
    (
        "session_turn_members",
        &[
            "session_id",
            "generation",
            "turn_id",
            "occurrence_id",
            "ordinal",
        ],
    ),
    (
        "session_thread_hierarchy_edges",
        &[
            "session_id",
            "generation",
            "parent_thread_id",
            "child_thread_id",
            "ordinal",
        ],
    ),
    (
        "session_agent_hierarchy_edges",
        &[
            "session_id",
            "generation",
            "parent_agent_id",
            "child_agent_id",
            "ordinal",
        ],
    ),
    (
        "session_assertions",
        &[
            "session_id",
            "generation",
            "assertion_id",
            "assertion_kind",
            "subject_anchor_id",
            "object_anchor_id",
            "knowledge_at",
            "valid_time_json",
            "evidence_json",
        ],
    ),
    (
        "session_assertion_supersession",
        &[
            "session_id",
            "generation",
            "superseded_assertion_id",
            "superseding_assertion_id",
            "created_at",
        ],
    ),
    (
        "session_current_entities",
        &[
            "session_id",
            "generation",
            "entity_kind",
            "entity_id",
            "current_assertion_id",
            "current_occurrence_id",
            "coverage_json",
        ],
    ),
    (
        "session_summary_availability",
        &[
            "session_id",
            "generation",
            "summary_id",
            "availability",
            "source_horizon_json",
            "reason",
            "checked_at",
        ],
    ),
    (
        "session_temporal_migration_receipts",
        &[
            "session_id",
            "generation",
            "batch_ordinal",
            "source_digest",
            "frozen_watermarks_json",
            "imported_items",
            "committed_at",
        ],
    ),
    (
        "session_temporal_migration_dispositions",
        &[
            "session_id",
            "generation",
            "batch_ordinal",
            "disposition_ordinal",
            "provider",
            "message_id",
            "output_ordinal",
            "observation_id",
            "retrieval_anchor_id",
            "disposition",
            "reason",
            "row_digest",
        ],
    ),
    ("session_occurrences_fts", &["index_text", "snippet_text"]),
    ("session_summary_nodes_fts", &["summary_text", "index_text"]),
];

pub(in crate::global_db) async fn ensure_session_temporal_schema(
    conn: &Connection,
) -> crate::errors::Result<()> {
    let version = schema_version(conn).await?;
    if let Some(version) = version
        && version > SESSION_TEMPORAL_SCHEMA_VERSION
    {
        return Err(global_db_operation_message(
            OPERATION,
            format!(
                "database session temporal schema version {version} is newer than supported version {SESSION_TEMPORAL_SCHEMA_VERSION}"
            ),
        ));
    }

    let rebuild_fts = version.is_none() || temporal_fts_is_missing(conn).await?;
    if version.is_some() {
        remove_unstarted_refresh_reservations(conn).await?;
    }
    conn.execute_batch(TEMPORAL_SCHEMA_DDL)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    migrate_logical_copy_bitemporality(conn, version).await?;
    validate_temporal_table_shapes(conn).await?;
    validate_temporal_fts_contracts(conn).await?;
    if rebuild_fts {
        rebuild_temporal_fts(conn).await?;
    }
    validate_temporal_fts_match(conn).await?;
    conn.execute(
        "INSERT INTO session_temporal_schema_migrations(name, version, applied_at)
         VALUES (?1, ?2, unixepoch())
         ON CONFLICT(name) DO UPDATE SET
            version = excluded.version,
            applied_at = excluded.applied_at
         WHERE session_temporal_schema_migrations.version < excluded.version",
        params![MIGRATION_NAME, SESSION_TEMPORAL_SCHEMA_VERSION],
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(())
}

async fn remove_unstarted_refresh_reservations(conn: &Connection) -> crate::errors::Result<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS session_refresh_operations_delete_guard_v1;
         DELETE FROM session_refresh_operations
         WHERE state = 'running'
           AND NOT EXISTS (
               SELECT 1 FROM session_refresh_bindings
               WHERE session_refresh_bindings.session_id = session_refresh_operations.session_id
                 AND session_refresh_bindings.operation_id = session_refresh_operations.operation_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM session_refresh_progress
               WHERE session_refresh_progress.session_id = session_refresh_operations.session_id
                 AND session_refresh_progress.operation_id = session_refresh_operations.operation_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM session_refresh_batch_bindings
               WHERE session_refresh_batch_bindings.session_id = session_refresh_operations.session_id
                 AND session_refresh_batch_bindings.operation_id = session_refresh_operations.operation_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM session_refresh_receipts
               WHERE session_refresh_receipts.session_id = session_refresh_operations.session_id
                 AND session_refresh_receipts.operation_id = session_refresh_operations.operation_id
           );",
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(())
}

/// Upgrade pre-v3 copy edges to carry bitemporal columns while preserving
/// legacy unknown validity and the prior created_at knowledge watermark.
async fn migrate_logical_copy_bitemporality(
    conn: &Connection,
    version: Option<i64>,
) -> crate::errors::Result<()> {
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_table_info('session_logical_copy_edges') ORDER BY cid",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut columns = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        columns.push(
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        );
    }
    if columns.is_empty() {
        return Ok(());
    }
    let expected = [
        "session_id",
        "generation",
        "occurrence_id",
        "copied_from_occurrence_id",
        "proof_json",
        "knowledge_at",
        "valid_time_json",
        "created_at",
    ];
    if columns == expected {
        return Ok(());
    }
    let legacy = [
        "session_id",
        "generation",
        "occurrence_id",
        "copied_from_occurrence_id",
        "proof_json",
        "created_at",
    ];
    if columns != legacy {
        return Err(global_db_operation_message(
            OPERATION,
            format!(
                "table 'session_logical_copy_edges' has an incompatible temporal schema for migration from version {version:?}"
            ),
        ));
    }
    conn.execute_batch(
        r#"
        CREATE TABLE session_logical_copy_edges_v3 (
            session_id TEXT NOT NULL,
            generation INTEGER NOT NULL,
            occurrence_id TEXT NOT NULL,
            copied_from_occurrence_id TEXT NOT NULL,
            proof_json TEXT NOT NULL CHECK(json_valid(proof_json)),
            knowledge_at INTEGER NOT NULL,
            valid_time_json TEXT NOT NULL CHECK(
                json_valid(valid_time_json)
                AND json_type(valid_time_json, '$.kind') IS 'text'
                AND (
                    (
                        json_extract(valid_time_json, '$.kind') = 'unknown'
                        AND json_type(valid_time_json, '$.valid_at') IS NULL
                    )
                    OR (
                        json_extract(valid_time_json, '$.kind') = 'known'
                        AND json_type(valid_time_json, '$.valid_at') IS 'integer'
                    )
                )
            ),
            created_at INTEGER NOT NULL,
            PRIMARY KEY(session_id, generation, occurrence_id, copied_from_occurrence_id),
            CHECK(occurrence_id <> copied_from_occurrence_id),
            FOREIGN KEY(session_id, generation, occurrence_id)
                REFERENCES session_occurrences(session_id, generation, occurrence_id) ON DELETE CASCADE,
            FOREIGN KEY(session_id, generation, copied_from_occurrence_id)
                REFERENCES session_occurrences(session_id, generation, occurrence_id) ON DELETE CASCADE
        );
        INSERT INTO session_logical_copy_edges_v3 (
            session_id, generation, occurrence_id, copied_from_occurrence_id,
            proof_json, knowledge_at, valid_time_json, created_at
        )
        SELECT
            session_id,
            generation,
            occurrence_id,
            copied_from_occurrence_id,
            proof_json,
            COALESCE(
                (
                    SELECT occurrence.knowledge_at
                    FROM session_occurrences AS occurrence
                    WHERE occurrence.session_id = session_logical_copy_edges.session_id
                      AND occurrence.generation = session_logical_copy_edges.generation
                      AND occurrence.occurrence_id = session_logical_copy_edges.occurrence_id
                ),
                created_at
            ),
            '{"kind":"unknown"}',
            created_at
        FROM session_logical_copy_edges;
        DROP TABLE session_logical_copy_edges;
        ALTER TABLE session_logical_copy_edges_v3 RENAME TO session_logical_copy_edges;
        CREATE INDEX IF NOT EXISTS idx_session_logical_copy_edges_target
            ON session_logical_copy_edges(session_id, generation, copied_from_occurrence_id);
        "#,
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(())
}

async fn temporal_fts_is_missing(conn: &Connection) -> crate::errors::Result<bool> {
    for (table, _) in TEMPORAL_FTS_CONTRACTS {
        let mut rows = conn
            .query(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .is_none()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn validate_temporal_fts_contracts(conn: &Connection) -> crate::errors::Result<()> {
    for (table, expected_sql) in TEMPORAL_FTS_CONTRACTS {
        let mut rows = conn
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        else {
            return Err(global_db_operation_message(
                OPERATION,
                format!("temporal FTS table '{table}' is missing"),
            ));
        };
        let sql = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if normalize_fts_sql(&sql) != *expected_sql {
            return Err(global_db_operation_message(
                OPERATION,
                format!("table '{table}' has an incompatible temporal FTS contract"),
            ));
        }
    }
    Ok(())
}

fn normalize_fts_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace() && *character != ';')
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .replace("ifnotexists", "")
}

async fn rebuild_temporal_fts(conn: &Connection) -> crate::errors::Result<()> {
    for (table, _) in TEMPORAL_FTS_CONTRACTS {
        conn.execute(
            &format!("INSERT INTO {table}({table}) VALUES ('rebuild')"),
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    Ok(())
}

async fn validate_temporal_fts_match(conn: &Connection) -> crate::errors::Result<()> {
    for (table, _) in TEMPORAL_FTS_CONTRACTS {
        conn.query(
            &format!("SELECT rowid FROM {table} WHERE {table} MATCH ?1 LIMIT 1"),
            params!["__tracedecay_temporal_fts_probe__"],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    Ok(())
}

async fn validate_temporal_table_shapes(conn: &Connection) -> crate::errors::Result<()> {
    for &(table, expected_columns) in TEMPORAL_TABLE_COLUMNS {
        let mut rows = conn
            .query(
                "SELECT name FROM pragma_table_info(?1) ORDER BY cid",
                params![table],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut actual_columns = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            actual_columns.push(
                row.get::<String>(0)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
            );
        }
        let expected_columns = expected_columns
            .iter()
            .map(|column| (*column).to_string())
            .collect::<Vec<_>>();
        if actual_columns != expected_columns {
            return Err(global_db_operation_message(
                OPERATION,
                format!("table '{table}' has an incompatible temporal schema"),
            ));
        }
    }
    Ok(())
}

async fn schema_version(conn: &Connection) -> crate::errors::Result<Option<i64>> {
    let mut tables = conn
        .query(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'session_temporal_schema_migrations'",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if tables
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .is_none()
    {
        return Ok(None);
    }

    let mut rows = conn
        .query(
            "SELECT version FROM session_temporal_schema_migrations WHERE name = ?1",
            params![MIGRATION_NAME],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .map(|row| {
            row.get(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))
        })
        .transpose()
}
