use tracedecay_runtime_core::db::engine::{Executor, params};

use crate::schema_contract::validate_session_graph_publication_schema_contract;
use crate::{global_db_operation_error, global_db_operation_message};

#[path = "session_temporal_schema/admission.rs"]
mod admission;

pub(crate) use admission::{
    SessionTemporalSchemaAdmission, require_admissible_session_temporal_schema,
};
use admission::{validate_temporal_fts_contracts, validate_temporal_fts_match};

const OPERATION: &str = "initialize session temporal schema";
const MIGRATION_NAME: &str = "session-temporal";
const SESSION_TEMPORAL_AUTHORITY: &str = "session temporal";
const RELEASED_SESSION_TEMPORAL_SCHEMA_VERSION: i64 = 3;
pub(crate) use tracedecay_session_temporal_store::SESSION_TEMPORAL_SCHEMA_VERSION;

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

const TEMPORAL_SCHEMA_DDL: &str = r"
    CREATE TABLE IF NOT EXISTS session_temporal_schema_migrations (
        name TEXT PRIMARY KEY,
        version INTEGER NOT NULL CHECK(version > 0),
        applied_at INTEGER NOT NULL
    );
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

    CREATE TABLE IF NOT EXISTS session_relation_receipts (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL CHECK(generation > 0),
        scope_kind TEXT NOT NULL
            CHECK(scope_kind IN ('project_sessions', 'profile_sessions')),
        scope_id TEXT NOT NULL,
        expected_graph_watermark TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('pending', 'applied')),
        graph_watermark TEXT,
        created_at INTEGER NOT NULL,
        applied_at INTEGER,
        recovery_state TEXT NOT NULL DEFAULT 'pending'
            CHECK(recovery_state IN ('pending', 'retryable', 'permanent')),
        recovery_failure_code TEXT,
        recovery_failure_count INTEGER NOT NULL DEFAULT 0
            CHECK(recovery_failure_count >= 0),
        recovery_next_attempt_at INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY(session_id, generation),
        CHECK(
            (state = 'pending' AND graph_watermark IS NULL AND applied_at IS NULL)
            OR (state = 'applied' AND graph_watermark = expected_graph_watermark
                AND applied_at IS NOT NULL)
        ),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_session_relation_receipts_pending
        ON session_relation_receipts(state, created_at, session_id, generation);
    CREATE INDEX IF NOT EXISTS idx_session_relation_receipts_recovery_due
        ON session_relation_receipts(
            state, recovery_state, recovery_next_attempt_at,
            created_at, session_id, generation
        );

    CREATE TABLE IF NOT EXISTS session_relation_effect_journal (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL CHECK(generation > 0),
        projection_json TEXT NOT NULL CHECK(json_valid(projection_json)),
        created_at INTEGER NOT NULL,
        PRIMARY KEY(session_id, generation),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_relation_receipts(session_id, generation) ON DELETE CASCADE
    );

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
        batch_item_count INTEGER NOT NULL DEFAULT 0 CHECK(batch_item_count >= 0),
        committed_item_count INTEGER NOT NULL DEFAULT 0 CHECK(committed_item_count >= 0),
        committed_copy_count INTEGER NOT NULL DEFAULT 0 CHECK(committed_copy_count >= 0),
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
        source_provider TEXT NOT NULL CHECK(
            source_provider <> ''
            AND length(source_provider) <= 512
            AND source_provider = trim(source_provider)
        ),
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
        sanitized_content_digest TEXT NOT NULL CHECK(
            length(sanitized_content_digest) = 64
            AND sanitized_content_digest NOT GLOB '*[^0-9a-f]*'
        ),
        sanitized_content_bytes INTEGER NOT NULL CHECK(sanitized_content_bytes >= 0),
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

    CREATE TABLE IF NOT EXISTS session_derived_evidence (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        evidence_kind TEXT NOT NULL CHECK(evidence_kind IN ('span', 'burst')),
        evidence_id TEXT NOT NULL,
        retrieval_anchor_id TEXT NOT NULL,
        thread_id TEXT,
        first_occurrence_id TEXT NOT NULL,
        last_occurrence_id TEXT NOT NULL,
        algorithm_version TEXT NOT NULL,
        configuration_digest TEXT NOT NULL,
        member_count INTEGER NOT NULL CHECK(member_count > 0),
        member_digest TEXT NOT NULL,
        evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
        PRIMARY KEY(session_id, generation, evidence_kind, evidence_id),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE,
        FOREIGN KEY(retrieval_anchor_id) REFERENCES retrieval_anchors(anchor_id),
        FOREIGN KEY(session_id, generation, first_occurrence_id)
            REFERENCES session_occurrences(session_id, generation, occurrence_id),
        FOREIGN KEY(session_id, generation, last_occurrence_id)
            REFERENCES session_occurrences(session_id, generation, occurrence_id)
    );
    CREATE INDEX IF NOT EXISTS idx_session_derived_evidence_scope_order
        ON session_derived_evidence(
            session_id, generation, evidence_kind, first_occurrence_id, evidence_id
        );
    CREATE INDEX IF NOT EXISTS idx_session_derived_evidence_anchor
        ON session_derived_evidence(
            session_id, generation, retrieval_anchor_id, evidence_kind, evidence_id
        );
    CREATE INDEX IF NOT EXISTS idx_session_derived_evidence_thread_order
        ON session_derived_evidence(
            session_id, generation, thread_id, evidence_kind, first_occurrence_id, evidence_id
        );

    CREATE TABLE IF NOT EXISTS session_derived_evidence_members (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        evidence_kind TEXT NOT NULL CHECK(evidence_kind IN ('span', 'burst')),
        evidence_id TEXT NOT NULL,
        ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
        occurrence_id TEXT NOT NULL,
        member_role TEXT NOT NULL CHECK(member_role IN ('member', 'first', 'last')),
        PRIMARY KEY(session_id, generation, evidence_kind, evidence_id, ordinal),
        UNIQUE(session_id, generation, evidence_kind, evidence_id, occurrence_id),
        FOREIGN KEY(session_id, generation, evidence_kind, evidence_id)
            REFERENCES session_derived_evidence(
                session_id, generation, evidence_kind, evidence_id
            ) ON DELETE CASCADE,
        FOREIGN KEY(session_id, generation, occurrence_id)
            REFERENCES session_occurrences(session_id, generation, occurrence_id)
    );
    CREATE INDEX IF NOT EXISTS idx_session_derived_evidence_members_occurrence
        ON session_derived_evidence_members(
            session_id, generation, occurrence_id, evidence_kind, evidence_id, ordinal
        );

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
";

pub(crate) use tracedecay_session_temporal_store::TEMPORAL_TABLE_COLUMNS;

#[hotpath::measure(future = true, label = "session_temporal.schema.migrate")]
pub(crate) async fn migrate_released_v3_session_temporal_schema(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<()> {
    admission::validate_released_v3_session_temporal_schema(conn).await?;
    conn.execute_batch(
        "DROP TRIGGER session_temporal_projection_receipts_immutable_update_v1;
         ALTER TABLE session_temporal_projection_receipts
             ADD COLUMN batch_item_count INTEGER NOT NULL DEFAULT 0
             CHECK(batch_item_count >= 0);
         ALTER TABLE session_temporal_projection_receipts
             ADD COLUMN committed_item_count INTEGER NOT NULL DEFAULT 0
             CHECK(committed_item_count >= 0);
         ALTER TABLE session_temporal_projection_receipts
             ADD COLUMN committed_copy_count INTEGER NOT NULL DEFAULT 0
             CHECK(committed_copy_count >= 0);
         ALTER TABLE session_relation_receipts
             ADD COLUMN recovery_state TEXT NOT NULL DEFAULT 'pending'
             CHECK(recovery_state IN ('pending', 'retryable', 'permanent'));
         ALTER TABLE session_relation_receipts
             ADD COLUMN recovery_failure_code TEXT;
         ALTER TABLE session_relation_receipts
             ADD COLUMN recovery_failure_count INTEGER NOT NULL DEFAULT 0
             CHECK(recovery_failure_count >= 0);
         ALTER TABLE session_relation_receipts
             ADD COLUMN recovery_next_attempt_at INTEGER NOT NULL DEFAULT 0;
         CREATE INDEX IF NOT EXISTS idx_session_relation_receipts_recovery_due
             ON session_relation_receipts(
                 state, recovery_state, recovery_next_attempt_at,
                 created_at, session_id, generation
             );",
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;

    let mut ambiguous = conn
        .query(
            "SELECT receipt.session_id, receipt.generation, receipt.batch_ordinal
             FROM session_temporal_projection_receipts AS receipt
             LEFT JOIN session_refresh_batch_bindings AS batch_binding
               ON batch_binding.session_id = receipt.session_id
              AND batch_binding.generation = receipt.generation
              AND batch_binding.batch_ordinal = receipt.batch_ordinal
             LEFT JOIN session_refresh_progress AS progress
               ON progress.session_id = batch_binding.session_id
              AND progress.operation_id = batch_binding.operation_id
              AND progress.progress_ordinal = batch_binding.progress_ordinal
             LEFT JOIN session_refresh_bindings AS refresh_binding
               ON refresh_binding.session_id = batch_binding.session_id
              AND refresh_binding.operation_id = batch_binding.operation_id
              AND refresh_binding.generation = batch_binding.generation
             WHERE json_type(receipt.frozen_watermarks_json, '$.active_generation')
                       IS NOT 'integer'
                OR json_extract(receipt.frozen_watermarks_json, '$.active_generation') <= 0
                OR json_extract(receipt.frozen_watermarks_json, '$.active_generation')
                       > receipt.generation
                OR batch_binding.session_id IS NULL
                OR (
                    batch_binding.session_id IS NOT NULL
                    AND (
                        batch_binding.progress_ordinal <> receipt.batch_ordinal
                        OR progress.session_id IS NULL
                        OR refresh_binding.session_id IS NULL
                        OR refresh_binding.frozen_watermarks_json
                            <> receipt.frozen_watermarks_json
                        OR progress.committed_batches <> receipt.batch_ordinal + 1
                        OR progress.committed_records <>
                            receipt.occurrence_count + receipt.copy_count
                                + receipt.assertion_count
                        OR json_extract(progress.frontier_json, '$.committed_through')
                            <> receipt.projection_through
                    )
                )
             LIMIT 1",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if ambiguous
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .is_some()
    {
        return Err(admission::session_temporal_reset_required(
            "released v3 projection receipt batch accounting is unbound or ambiguous",
        ));
    }
    drop(ambiguous);

    let mut invalid = conn
        .query(
            "SELECT current.session_id, current.generation, current.batch_ordinal
             FROM session_temporal_projection_receipts AS current
             LEFT JOIN session_temporal_projection_receipts AS previous
               ON previous.session_id = current.session_id
              AND previous.generation = current.generation
              AND previous.batch_ordinal = current.batch_ordinal - 1
             WHERE (
                 current.batch_ordinal > 0
                 AND (
                   previous.batch_ordinal IS NULL
                   OR current.occurrence_count + current.copy_count + current.assertion_count
                      < previous.occurrence_count + previous.copy_count
                        + previous.assertion_count
                 )
               )
               OR (
                 current.batch_ordinal = 0
                 AND json_extract(
                       current.frozen_watermarks_json, '$.active_generation'
                     ) <> current.generation
                 AND current.occurrence_count + current.copy_count + current.assertion_count
                     < COALESCE((
                         SELECT baseline.occurrence_count + baseline.copy_count
                                + baseline.assertion_count
                         FROM session_temporal_projection_receipts AS baseline
                         WHERE baseline.session_id = current.session_id
                           AND baseline.generation = json_extract(
                               current.frozen_watermarks_json, '$.active_generation'
                           )
                         ORDER BY baseline.batch_ordinal DESC
                         LIMIT 1
                       ), 0)
               )
             LIMIT 1",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if invalid
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .is_some()
    {
        return Err(admission::session_temporal_reset_required(
            "released v3 projection receipt progress is non-monotonic or noncontiguous",
        ));
    }
    drop(invalid);

    conn.execute(
        "UPDATE session_temporal_projection_receipts AS current
         SET batch_item_count =
               current.occurrence_count + current.copy_count + current.assertion_count
               - CASE
                   WHEN current.batch_ordinal > 0 THEN COALESCE((
                     SELECT previous.occurrence_count + previous.copy_count
                            + previous.assertion_count
                     FROM session_temporal_projection_receipts AS previous
                     WHERE previous.session_id = current.session_id
                       AND previous.generation = current.generation
                       AND previous.batch_ordinal = current.batch_ordinal - 1
                   ), 0)
                   WHEN json_extract(
                          current.frozen_watermarks_json, '$.active_generation'
                        ) <> current.generation THEN COALESCE((
                     SELECT baseline.occurrence_count + baseline.copy_count
                            + baseline.assertion_count
                     FROM session_temporal_projection_receipts AS baseline
                     WHERE baseline.session_id = current.session_id
                       AND baseline.generation = json_extract(
                           current.frozen_watermarks_json, '$.active_generation'
                       )
                     ORDER BY baseline.batch_ordinal DESC
                     LIMIT 1
                   ), 0)
                   ELSE 0
                 END,
             committed_item_count =
               current.occurrence_count + current.copy_count + current.assertion_count,
             committed_copy_count = current.copy_count",
        (),
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let updated = conn
        .execute(
            "UPDATE session_temporal_schema_migrations
             SET version = ?1, applied_at = unixepoch()
             WHERE name = ?2 AND version = ?3",
            params![
                SESSION_TEMPORAL_SCHEMA_VERSION,
                MIGRATION_NAME,
                RELEASED_SESSION_TEMPORAL_SCHEMA_VERSION,
            ],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if updated != 1 {
        return Err(admission::session_temporal_reset_required(
            "released v3 temporal schema marker changed during migration",
        ));
    }
    validate_temporal_table_shapes(conn).await?;
    admission::validate_current_session_temporal_schema(conn).await
}

/// Installs the final schema into a store already proven fresh by admission.
#[hotpath::measure(future = true, label = "session_temporal.schema.install")]
pub(crate) async fn install_session_temporal_schema(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<()> {
    conn.execute_batch(TEMPORAL_SCHEMA_DDL)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    conn.execute_batch(tracedecay_rusqlite_runtime::repository::GRAPH_PUBLICATION_SCHEMA_V1)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    validate_session_graph_publication_schema_contract(conn).await?;
    validate_temporal_table_shapes(conn).await?;
    validate_temporal_fts_contracts(conn).await?;
    validate_temporal_fts_match(conn).await?;
    conn.execute(
        "INSERT INTO session_temporal_schema_migrations(name, version, applied_at)
         VALUES (?1, ?2, unixepoch())",
        params![MIGRATION_NAME, SESSION_TEMPORAL_SCHEMA_VERSION],
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(())
}

async fn validate_temporal_table_shapes(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<()> {
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
