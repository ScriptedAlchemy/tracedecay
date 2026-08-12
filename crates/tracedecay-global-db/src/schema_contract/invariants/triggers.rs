use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use crate::global_db_operation_error;

use super::rows::authority_violation;
use super::{OPERATION, normalize_trigger_sql};

pub(in crate::schema_contract) struct Trigger {
    pub(in crate::schema_contract) name: &'static str,
    pub(in crate::schema_contract) table: &'static str,
    pub(in crate::schema_contract) create_sql: &'static str,
}

pub(in crate::schema_contract) struct Invariant {
    pub(in crate::schema_contract) triggers: &'static [Trigger],
    pub(super) audit_query: Option<&'static str>,
    pub(super) violation: &'static str,
}

const OBSERVATION_IMMUTABILITY: &[Trigger] = &[
    Trigger {
        name: "observations_immutable_update",
        table: "observations",
        create_sql: "CREATE TRIGGER observations_immutable_update
            BEFORE UPDATE ON observations BEGIN
                SELECT RAISE(ABORT, 'observations are immutable');
            END",
    },
    Trigger {
        name: "observations_immutable_delete",
        table: "observations",
        create_sql: "CREATE TRIGGER observations_immutable_delete
            BEFORE DELETE ON observations BEGIN
                SELECT RAISE(ABORT, 'observations are immutable');
            END",
    },
];

const RECEIPT_IMMUTABILITY: &[Trigger] = &[
    Trigger {
        name: "sanitization_receipts_immutable_update_v1",
        table: "sanitization_receipts",
        create_sql: "CREATE TRIGGER sanitization_receipts_immutable_update_v1
            BEFORE UPDATE ON sanitization_receipts BEGIN
                SELECT RAISE(ABORT, 'sanitization receipts are immutable');
            END",
    },
    Trigger {
        name: "sanitization_receipts_immutable_delete_v1",
        table: "sanitization_receipts",
        create_sql: "CREATE TRIGGER sanitization_receipts_immutable_delete_v1
            BEFORE DELETE ON sanitization_receipts BEGIN
                SELECT RAISE(ABORT, 'sanitization receipts are immutable');
            END",
    },
];

const SOURCE_CURSOR_ADVANCE_IMMUTABILITY: &[Trigger] = &[
    Trigger {
        name: "source_cursor_advances_immutable_update_v1",
        table: "source_cursor_advances",
        create_sql: "CREATE TRIGGER source_cursor_advances_immutable_update_v1
            BEFORE UPDATE ON source_cursor_advances BEGIN
                SELECT RAISE(ABORT, 'source cursor advances are immutable');
            END",
    },
    Trigger {
        name: "source_cursor_advances_immutable_delete_v1",
        table: "source_cursor_advances",
        create_sql: "CREATE TRIGGER source_cursor_advances_immutable_delete_v1
            BEFORE DELETE ON source_cursor_advances BEGIN
                SELECT RAISE(ABORT, 'source cursor advances are immutable');
            END",
    },
];

const PROJECTION_AUDIT_INVALIDATION: &[Trigger] = &[
    Trigger {
        name: "receipt_audit_invalidate_nonappend_insert_v1",
        table: "sanitization_receipts",
        create_sql: "CREATE TRIGGER receipt_audit_invalidate_nonappend_insert_v1
            AFTER INSERT ON sanitization_receipts
            WHEN NEW.rowid <= COALESCE((
                SELECT receipt_rowid FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority'
            ), 0) BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "observation_audit_invalidate_nonappend_insert_v1",
        table: "observations",
        create_sql: "CREATE TRIGGER observation_audit_invalidate_nonappend_insert_v1
            AFTER INSERT ON observations
            WHEN NEW.sequence <= COALESCE((
                SELECT observation_sequence FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority'
            ), 0) BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "source_cursor_audit_invalidate_key_update_v1",
        table: "source_cursors",
        create_sql: "CREATE TRIGGER source_cursor_audit_invalidate_key_update_v1
            AFTER UPDATE OF source_json, scope_json ON source_cursors BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "source_cursor_audit_invalidate_delete_v1",
        table: "source_cursors",
        create_sql: "CREATE TRIGGER source_cursor_audit_invalidate_delete_v1
            AFTER DELETE ON source_cursors BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_provenance_audit_invalidate_update_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_audit_invalidate_update_v1
            AFTER UPDATE ON observation_projection_provenance BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_provenance_audit_invalidate_delete_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_audit_invalidate_delete_v1
            AFTER DELETE ON observation_projection_provenance BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "workflow_fact_audit_invalidate_insert_v1",
        table: "observation_workflow_facts",
        create_sql: "CREATE TRIGGER workflow_fact_audit_invalidate_insert_v1
            AFTER INSERT ON observation_workflow_facts BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "workflow_fact_audit_invalidate_update_v1",
        table: "observation_workflow_facts",
        create_sql: "CREATE TRIGGER workflow_fact_audit_invalidate_update_v1
            AFTER UPDATE ON observation_workflow_facts BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "workflow_fact_audit_invalidate_delete_v1",
        table: "observation_workflow_facts",
        create_sql: "CREATE TRIGGER workflow_fact_audit_invalidate_delete_v1
            AFTER DELETE ON observation_workflow_facts BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_disposition_audit_invalidate_update_v1",
        table: "observation_projection_dispositions",
        create_sql: "CREATE TRIGGER projection_disposition_audit_invalidate_update_v1
            AFTER UPDATE ON observation_projection_dispositions BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_disposition_audit_invalidate_delete_v1",
        table: "observation_projection_dispositions",
        create_sql: "CREATE TRIGGER projection_disposition_audit_invalidate_delete_v1
            AFTER DELETE ON observation_projection_dispositions BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_alias_audit_invalidate_update_v1",
        table: "observation_projection_aliases",
        create_sql: "CREATE TRIGGER projection_alias_audit_invalidate_update_v1
            AFTER UPDATE ON observation_projection_aliases BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_alias_audit_invalidate_delete_v1",
        table: "observation_projection_aliases",
        create_sql: "CREATE TRIGGER projection_alias_audit_invalidate_delete_v1
            AFTER DELETE ON observation_projection_aliases BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_output_audit_invalidate_update_v1",
        table: "session_messages",
        create_sql: "CREATE TRIGGER projection_output_audit_invalidate_update_v1
            AFTER UPDATE ON session_messages
            WHEN EXISTS (
                SELECT 1 FROM observation_projection_provenance
                WHERE projector_version = 'claude-session-message-v4'
                  AND output_provider = OLD.provider
                  AND output_message_id = OLD.message_id
            ) BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_output_audit_invalidate_delete_v1",
        table: "session_messages",
        create_sql: "CREATE TRIGGER projection_output_audit_invalidate_delete_v1
            AFTER DELETE ON session_messages
            WHEN EXISTS (
                SELECT 1 FROM observation_projection_provenance
                WHERE projector_version = 'claude-session-message-v4'
                  AND output_provider = OLD.provider
                  AND output_message_id = OLD.message_id
            ) BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_checkpoint_audit_invalidate_regression_v1",
        table: "observation_projection_checkpoints",
        create_sql: "CREATE TRIGGER projection_checkpoint_audit_invalidate_regression_v1
            AFTER UPDATE OF last_sequence ON observation_projection_checkpoints
            WHEN NEW.last_sequence < OLD.last_sequence BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
    Trigger {
        name: "projection_checkpoint_audit_invalidate_delete_v1",
        table: "observation_projection_checkpoints",
        create_sql: "CREATE TRIGGER projection_checkpoint_audit_invalidate_delete_v1
            AFTER DELETE ON observation_projection_checkpoints BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END",
    },
];

const STORE_PROJECT_IMMUTABILITY: &[Trigger] = &[Trigger {
    name: "store_instances_project_immutable_v1",
    table: "store_instances",
    create_sql: "CREATE TRIGGER store_instances_project_immutable_v1
        BEFORE UPDATE OF project_id ON store_instances
        WHEN OLD.project_id IS NOT NEW.project_id
        BEGIN SELECT RAISE(ABORT, 'store project identity is immutable'); END",
}];

const GRAPH_SCOPE_IDENTITY: &[Trigger] = &[
    Trigger {
        name: "graph_scopes_store_project_insert_v1",
        table: "graph_scopes",
        create_sql: "CREATE TRIGGER graph_scopes_store_project_insert_v1
            BEFORE INSERT ON graph_scopes WHEN NOT EXISTS (
                SELECT 1 FROM store_instances
                WHERE store_id = NEW.store_id AND project_id = NEW.project_id
            ) BEGIN SELECT RAISE(ABORT, 'graph scope store/project mismatch'); END",
    },
    Trigger {
        name: "graph_scopes_store_project_update_v1",
        table: "graph_scopes",
        create_sql: "CREATE TRIGGER graph_scopes_store_project_update_v1
            BEFORE UPDATE OF store_id, project_id ON graph_scopes WHEN NOT EXISTS (
                SELECT 1 FROM store_instances
                WHERE store_id = NEW.store_id AND project_id = NEW.project_id
            ) BEGIN SELECT RAISE(ABORT, 'graph scope store/project mismatch'); END",
    },
];

const QUEUE_IDENTITY: &[Trigger] = &[
    Trigger {
        name: "projection_queue_identity_insert_v1",
        table: "projection_queue",
        create_sql: "CREATE TRIGGER projection_queue_identity_insert_v1
            BEFORE INSERT ON projection_queue WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id
                  AND sequence = NEW.observation_sequence
            ) BEGIN SELECT RAISE(ABORT, 'projection queue observation identity mismatch'); END",
    },
    Trigger {
        name: "projection_queue_identity_update_v1",
        table: "projection_queue",
        create_sql: "CREATE TRIGGER projection_queue_identity_update_v1
            BEFORE UPDATE OF observation_id, observation_sequence ON projection_queue
            WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id
                  AND sequence = NEW.observation_sequence
            ) BEGIN SELECT RAISE(ABORT, 'projection queue observation identity mismatch'); END",
    },
];

const PROVENANCE_RECEIPT: &[Trigger] = &[
    Trigger {
        name: "projection_provenance_receipt_insert_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_receipt_insert_v1
            BEFORE INSERT ON observation_projection_provenance WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
            ) BEGIN SELECT RAISE(ABORT, 'projection provenance receipt mismatch'); END",
    },
    Trigger {
        name: "projection_provenance_receipt_update_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_receipt_update_v1
            BEFORE UPDATE OF observation_id, receipt_id
            ON observation_projection_provenance WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
            ) BEGIN SELECT RAISE(ABORT, 'projection provenance receipt mismatch'); END",
    },
];

const WORKFLOW_FACT_RECEIPT: &[Trigger] = &[
    Trigger {
        name: "workflow_fact_receipt_insert_v1",
        table: "observation_workflow_facts",
        create_sql: "CREATE TRIGGER workflow_fact_receipt_insert_v1
            BEFORE INSERT ON observation_workflow_facts WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id
                  AND receipt_id = NEW.receipt_id
                  AND sequence = NEW.observation_sequence
            ) BEGIN SELECT RAISE(ABORT, 'workflow fact observation receipt mismatch'); END",
    },
    Trigger {
        name: "workflow_fact_receipt_update_v1",
        table: "observation_workflow_facts",
        create_sql: "CREATE TRIGGER workflow_fact_receipt_update_v1
            BEFORE UPDATE OF observation_id, receipt_id, observation_sequence
            ON observation_workflow_facts WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id
                  AND receipt_id = NEW.receipt_id
                  AND sequence = NEW.observation_sequence
            ) BEGIN SELECT RAISE(ABORT, 'workflow fact observation receipt mismatch'); END",
    },
];

const DISPOSITION_RECEIPT: &[Trigger] = &[
    Trigger {
        name: "projection_disposition_receipt_insert_v1",
        table: "observation_projection_dispositions",
        create_sql: "CREATE TRIGGER projection_disposition_receipt_insert_v1
            BEFORE INSERT ON observation_projection_dispositions WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
            ) BEGIN SELECT RAISE(ABORT, 'projection disposition receipt mismatch'); END",
    },
    Trigger {
        name: "projection_disposition_receipt_update_v1",
        table: "observation_projection_dispositions",
        create_sql: "CREATE TRIGGER projection_disposition_receipt_update_v1
            BEFORE UPDATE OF observation_id, receipt_id
            ON observation_projection_dispositions WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id AND receipt_id = NEW.receipt_id
            ) BEGIN SELECT RAISE(ABORT, 'projection disposition receipt mismatch'); END",
    },
];

const MESSAGE_CREATED_DOMAIN: &[Trigger] = &[
    Trigger {
        name: "projection_provenance_message_created_insert_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_message_created_insert_v1
            BEFORE INSERT ON observation_projection_provenance
            WHEN NEW.message_created NOT IN (0, 1)
            BEGIN SELECT RAISE(ABORT, 'invalid projection message_created'); END",
    },
    Trigger {
        name: "projection_provenance_message_created_update_v1",
        table: "observation_projection_provenance",
        create_sql: "CREATE TRIGGER projection_provenance_message_created_update_v1
            BEFORE UPDATE OF message_created ON observation_projection_provenance
            WHEN NEW.message_created NOT IN (0, 1)
            BEGIN SELECT RAISE(ABORT, 'invalid projection message_created'); END",
    },
];

const CHECKPOINT_DOMAIN: &[Trigger] = &[
    Trigger {
        name: "projection_checkpoint_sequence_insert_v1",
        table: "observation_projection_checkpoints",
        create_sql: "CREATE TRIGGER projection_checkpoint_sequence_insert_v1
            BEFORE INSERT ON observation_projection_checkpoints
            WHEN NEW.last_sequence < 0
            BEGIN SELECT RAISE(ABORT, 'invalid projection checkpoint sequence'); END",
    },
    Trigger {
        name: "projection_checkpoint_sequence_update_v1",
        table: "observation_projection_checkpoints",
        create_sql: "CREATE TRIGGER projection_checkpoint_sequence_update_v1
            BEFORE UPDATE OF last_sequence ON observation_projection_checkpoints
            WHEN NEW.last_sequence < 0
            BEGIN SELECT RAISE(ABORT, 'invalid projection checkpoint sequence'); END",
    },
];

const SESSION_SUMMARY_AUTHORITY_IMMUTABILITY: &[Trigger] = &[
    Trigger {
        name: "session_summary_nodes_immutable_update_v1",
        table: "session_summary_nodes",
        create_sql: "CREATE TRIGGER session_summary_nodes_immutable_update_v1
            BEFORE UPDATE ON session_summary_nodes BEGIN
                SELECT RAISE(ABORT, 'session summary nodes are immutable');
            END",
    },
    Trigger {
        name: "session_summary_nodes_immutable_delete_v1",
        table: "session_summary_nodes",
        create_sql: "CREATE TRIGGER session_summary_nodes_immutable_delete_v1
            BEFORE DELETE ON session_summary_nodes BEGIN
                SELECT RAISE(ABORT, 'session summary nodes are immutable');
            END",
    },
    Trigger {
        name: "session_external_payload_manifests_immutable_update_v1",
        table: "session_external_payload_manifests",
        create_sql: "CREATE TRIGGER session_external_payload_manifests_immutable_update_v1
            BEFORE UPDATE ON session_external_payload_manifests BEGIN
                SELECT RAISE(ABORT, 'session external payload manifests are immutable');
            END",
    },
    Trigger {
        name: "session_external_payload_manifests_immutable_delete_v1",
        table: "session_external_payload_manifests",
        create_sql: "CREATE TRIGGER session_external_payload_manifests_immutable_delete_v1
            BEFORE DELETE ON session_external_payload_manifests BEGIN
                SELECT RAISE(ABORT, 'session external payload manifests are immutable');
            END",
    },
];

const SESSION_RECEIPT_IMMUTABILITY: &[Trigger] = &[
    Trigger {
        name: "session_temporal_projection_receipts_insert_guard_v1",
        table: "session_temporal_projection_receipts",
        create_sql: "CREATE TRIGGER session_temporal_projection_receipts_insert_guard_v1
            BEFORE INSERT ON session_temporal_projection_receipts
            WHEN NOT EXISTS (
                SELECT 1 FROM session_temporal_generations
                WHERE session_id = NEW.session_id
                  AND generation = NEW.generation
                  AND state = 'building'
                  AND frozen_watermarks_json = NEW.frozen_watermarks_json
            )
            OR (
                NEW.batch_ordinal = 0
                AND EXISTS (
                    SELECT 1 FROM session_temporal_projection_receipts
                    WHERE session_id = NEW.session_id AND generation = NEW.generation
                )
            )
            OR (
                NEW.batch_ordinal > 0
                AND NOT EXISTS (
                    SELECT 1 FROM session_temporal_projection_receipts
                    WHERE session_id = NEW.session_id AND generation = NEW.generation
                      AND batch_ordinal = NEW.batch_ordinal - 1
                      AND source_through <= NEW.source_through
                      AND projection_through <= NEW.projection_through
                )
            )
            BEGIN SELECT RAISE(ABORT, 'invalid session temporal projection receipt checkpoint'); END",
    },
    Trigger {
        name: "session_temporal_observation_effects_insert_guard_v1",
        table: "session_temporal_observation_effects",
        create_sql: "CREATE TRIGGER session_temporal_observation_effects_insert_guard_v1
            BEFORE INSERT ON session_temporal_observation_effects
            WHEN NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observation_id = NEW.observation_id
                  AND sequence = NEW.observation_sequence
                  AND receipt_id = NEW.receipt_id
            )
            BEGIN SELECT RAISE(ABORT, 'session temporal observation effect authority mismatch'); END",
    },
    Trigger {
        name: "session_temporal_observation_effects_immutable_update_v1",
        table: "session_temporal_observation_effects",
        create_sql: "CREATE TRIGGER session_temporal_observation_effects_immutable_update_v1
            BEFORE UPDATE ON session_temporal_observation_effects BEGIN
                SELECT RAISE(ABORT, 'session temporal observation effects are immutable');
            END",
    },
    Trigger {
        name: "session_temporal_observation_effects_immutable_delete_v1",
        table: "session_temporal_observation_effects",
        create_sql: "CREATE TRIGGER session_temporal_observation_effects_immutable_delete_v1
            BEFORE DELETE ON session_temporal_observation_effects BEGIN
                SELECT RAISE(ABORT, 'session temporal observation effects are immutable');
            END",
    },
    Trigger {
        name: "session_temporal_projection_receipts_immutable_update_v1",
        table: "session_temporal_projection_receipts",
        create_sql: "CREATE TRIGGER session_temporal_projection_receipts_immutable_update_v1
            BEFORE UPDATE ON session_temporal_projection_receipts BEGIN
                SELECT RAISE(ABORT, 'session temporal projection receipts are immutable');
            END",
    },
    Trigger {
        name: "session_temporal_projection_receipts_immutable_delete_v1",
        table: "session_temporal_projection_receipts",
        create_sql: "CREATE TRIGGER session_temporal_projection_receipts_immutable_delete_v1
            BEFORE DELETE ON session_temporal_projection_receipts BEGIN
                SELECT RAISE(ABORT, 'session temporal projection receipts are immutable');
            END",
    },
    Trigger {
        name: "session_refresh_bindings_immutable_update_v1",
        table: "session_refresh_bindings",
        create_sql: "CREATE TRIGGER session_refresh_bindings_immutable_update_v1
            BEFORE UPDATE ON session_refresh_bindings BEGIN
                SELECT RAISE(ABORT, 'session refresh bindings are immutable');
            END",
    },
    Trigger {
        name: "session_refresh_bindings_immutable_delete_v1",
        table: "session_refresh_bindings",
        create_sql: "CREATE TRIGGER session_refresh_bindings_immutable_delete_v1
            BEFORE DELETE ON session_refresh_bindings BEGIN
                SELECT RAISE(ABORT, 'session refresh bindings are immutable');
            END",
    },
    Trigger {
        name: "session_refresh_progress_immutable_update_v1",
        table: "session_refresh_progress",
        create_sql: "CREATE TRIGGER session_refresh_progress_immutable_update_v1
            BEFORE UPDATE ON session_refresh_progress BEGIN
                SELECT RAISE(ABORT, 'session refresh progress is append-only');
            END",
    },
    Trigger {
        name: "session_refresh_batch_bindings_immutable_update_v1",
        table: "session_refresh_batch_bindings",
        create_sql: "CREATE TRIGGER session_refresh_batch_bindings_immutable_update_v1
            BEFORE UPDATE ON session_refresh_batch_bindings BEGIN
                SELECT RAISE(ABORT, 'session refresh batch bindings are immutable');
            END",
    },
    Trigger {
        name: "session_refresh_batch_bindings_immutable_delete_v1",
        table: "session_refresh_batch_bindings",
        create_sql: "CREATE TRIGGER session_refresh_batch_bindings_immutable_delete_v1
            BEFORE DELETE ON session_refresh_batch_bindings BEGIN
                SELECT RAISE(ABORT, 'session refresh batch bindings are immutable');
            END",
    },
    Trigger {
        name: "session_refresh_progress_immutable_delete_v1",
        table: "session_refresh_progress",
        create_sql: "CREATE TRIGGER session_refresh_progress_immutable_delete_v1
            BEFORE DELETE ON session_refresh_progress BEGIN
                SELECT RAISE(ABORT, 'session refresh progress is append-only');
            END",
    },
    Trigger {
        name: "session_refresh_receipts_immutable_update_v1",
        table: "session_refresh_receipts",
        create_sql: "CREATE TRIGGER session_refresh_receipts_immutable_update_v1
            BEFORE UPDATE ON session_refresh_receipts BEGIN
                SELECT RAISE(ABORT, 'session refresh receipts are immutable');
            END",
    },
    Trigger {
        name: "session_refresh_receipts_immutable_delete_v1",
        table: "session_refresh_receipts",
        create_sql: "CREATE TRIGGER session_refresh_receipts_immutable_delete_v1
            BEFORE DELETE ON session_refresh_receipts BEGIN
                SELECT RAISE(ABORT, 'session refresh receipts are immutable');
            END",
    },
    Trigger {
        name: "session_query_cursor_keys_immutable_delete_v1",
        table: "session_query_cursor_keys",
        create_sql: "CREATE TRIGGER session_query_cursor_keys_immutable_delete_v1
            BEFORE DELETE ON session_query_cursor_keys BEGIN
                SELECT RAISE(ABORT, 'session cursor keys are append-only');
            END",
    },
];

const SESSION_REFRESH_STATE_GUARDS: &[Trigger] = &[
    Trigger {
        name: "session_refresh_operations_insert_guard_v1",
        table: "session_refresh_operations",
        create_sql: "CREATE TRIGGER session_refresh_operations_insert_guard_v1
            BEFORE INSERT ON session_refresh_operations
            WHEN NEW.state <> 'running'
              OR NEW.updated_at <> NEW.created_at
              OR NEW.terminal_at IS NOT NULL
              OR NEW.failure_code IS NOT NULL
              OR json_type(NEW.target_frontier_json, '$.observed_through') IS NOT 'integer'
              OR json_type(NEW.target_frontier_json, '$.committed_through') IS NOT 'integer'
              OR json_extract(NEW.target_frontier_json, '$.observed_through') < 0
              OR json_extract(NEW.target_frontier_json, '$.committed_through') < 0
              OR json_extract(NEW.target_frontier_json, '$.committed_through')
                  > json_extract(NEW.target_frontier_json, '$.observed_through')
            BEGIN SELECT RAISE(ABORT, 'session refresh operations must start running'); END",
    },
    Trigger {
        name: "session_refresh_operations_state_guard_v1",
        table: "session_refresh_operations",
        create_sql: "CREATE TRIGGER session_refresh_operations_state_guard_v1
            BEFORE UPDATE ON session_refresh_operations
            WHEN OLD.session_id IS NOT NEW.session_id
              OR OLD.operation_id IS NOT NEW.operation_id
              OR OLD.request_digest IS NOT NEW.request_digest
              OR OLD.target_frontier_json IS NOT NEW.target_frontier_json
              OR OLD.created_at IS NOT NEW.created_at
              OR OLD.state <> 'running'
              OR NEW.state NOT IN ('running', 'complete', 'failed', 'cancelled')
              OR NEW.updated_at < OLD.updated_at
              OR (NEW.state = 'running'
                  AND (NEW.terminal_at IS NOT NULL OR NEW.failure_code IS NOT NULL))
              OR (NEW.state = 'complete'
                  AND (NEW.terminal_at IS NULL
                    OR NEW.terminal_at <> NEW.updated_at
                    OR NEW.failure_code IS NOT NULL))
              OR (NEW.state = 'failed'
                  AND (NEW.terminal_at IS NULL
                    OR NEW.terminal_at <> NEW.updated_at
                    OR NEW.failure_code IS NULL))
              OR (NEW.state = 'cancelled'
                  AND (NEW.terminal_at IS NULL
                    OR NEW.terminal_at <> NEW.updated_at
                    OR NEW.failure_code IS NOT NULL))
              OR (NEW.state <> 'running' AND NOT EXISTS (
                    SELECT 1
                    FROM session_refresh_bindings AS binding
                    JOIN session_temporal_generations AS generation
                      ON generation.session_id = binding.session_id
                     AND generation.generation = binding.generation
                    JOIN session_refresh_progress AS progress
                      ON progress.session_id = binding.session_id
                     AND progress.operation_id = binding.operation_id
                    WHERE binding.session_id = NEW.session_id
                      AND binding.operation_id = NEW.operation_id
                      AND generation.state = CASE NEW.state
                          WHEN 'complete' THEN 'active'
                          WHEN 'failed' THEN 'failed'
                          WHEN 'cancelled' THEN 'cancelled'
                      END
                      AND progress.progress_ordinal = (
                          SELECT MAX(latest.progress_ordinal)
                          FROM session_refresh_progress AS latest
                          WHERE latest.session_id = NEW.session_id
                            AND latest.operation_id = NEW.operation_id
                      )
                      AND (
                        (
                          NEW.state IN ('failed', 'cancelled')
                          AND progress.progress_ordinal = 0
                          AND progress.committed_batches = 0
                          AND progress.committed_records = 0
                          AND json_extract(
                              progress.frontier_json, '$.committed_through'
                          ) = binding.source_frontier
                        )
                        OR (
                          progress.committed_batches > 0
                          AND progress.progress_ordinal = progress.committed_batches - 1
                          AND (NEW.state <> 'complete'
                            OR json_extract(
                                progress.frontier_json, '$.committed_through'
                            ) = binding.target_frontier)
                          AND EXISTS (
                              SELECT 1
                              FROM session_refresh_batch_bindings AS terminal_batch
                              WHERE terminal_batch.session_id = progress.session_id
                                AND terminal_batch.operation_id = progress.operation_id
                                AND terminal_batch.progress_ordinal =
                                    progress.progress_ordinal
                                AND terminal_batch.generation = binding.generation
                                AND terminal_batch.batch_ordinal =
                                    progress.progress_ordinal
                          )
                        )
                      )
              ))
            BEGIN SELECT RAISE(ABORT, 'invalid session refresh state transition'); END",
    },
    Trigger {
        name: "session_refresh_operations_delete_guard_v1",
        table: "session_refresh_operations",
        create_sql: "CREATE TRIGGER session_refresh_operations_delete_guard_v1
            BEFORE DELETE ON session_refresh_operations BEGIN
                SELECT RAISE(ABORT, 'session refresh operations are durable');
            END",
    },
    Trigger {
        name: "session_refresh_bindings_insert_guard_v1",
        table: "session_refresh_bindings",
        create_sql: "CREATE TRIGGER session_refresh_bindings_insert_guard_v1
            BEFORE INSERT ON session_refresh_bindings
            WHEN NEW.projector_version <> 'session-temporal-projector.v1'
              OR length(NEW.config_digest) <> 71
              OR NEW.config_digest NOT GLOB 'sha256:[0-9a-f]*'
              OR substr(NEW.config_digest, 8) GLOB '*[^0-9a-f]*'
              OR length(NEW.binding_digest) <> 71
              OR NEW.binding_digest NOT GLOB 'sha256:[0-9a-f]*'
              OR substr(NEW.binding_digest, 8) GLOB '*[^0-9a-f]*'
              OR NOT EXISTS (
                SELECT 1
                FROM session_refresh_operations AS operation
                JOIN session_temporal_generations AS generation
                  ON generation.session_id = NEW.session_id
                 AND generation.generation = NEW.generation
                WHERE operation.session_id = NEW.session_id
                  AND operation.operation_id = NEW.operation_id
                  AND operation.state = 'running'
                  AND operation.request_digest = NEW.binding_digest
                  AND operation.created_at = NEW.created_at
                  AND NEW.scope_kind = 'session_store'
                  AND NEW.source_frontier =
                      json_extract(operation.target_frontier_json, '$.committed_through')
                  AND NEW.target_frontier =
                      json_extract(operation.target_frontier_json, '$.observed_through')
                  AND generation.state = 'building'
                  AND generation.frozen_watermarks_json = NEW.frozen_watermarks_json
            )
            BEGIN SELECT RAISE(ABORT, 'invalid session refresh binding'); END",
    },
    Trigger {
        name: "session_refresh_progress_insert_guard_v1",
        table: "session_refresh_progress",
        create_sql: "CREATE TRIGGER session_refresh_progress_insert_guard_v1
            BEFORE INSERT ON session_refresh_progress
            WHEN NOT EXISTS (
                SELECT 1
                FROM session_refresh_operations AS operation
                JOIN session_refresh_bindings AS binding
                  ON binding.session_id = operation.session_id
                 AND binding.operation_id = operation.operation_id
                JOIN session_temporal_generations AS generation
                  ON generation.session_id = binding.session_id
                 AND generation.generation = binding.generation
                WHERE operation.session_id = NEW.session_id
                  AND operation.operation_id = NEW.operation_id
                  AND operation.state = 'running'
                  AND NEW.recorded_at >= operation.created_at
                  AND binding.source_frontier =
                      json_extract(operation.target_frontier_json, '$.committed_through')
                  AND binding.target_frontier =
                      json_extract(operation.target_frontier_json, '$.observed_through')
                  AND generation.state = 'building'
                  AND generation.frozen_watermarks_json = binding.frozen_watermarks_json
                  AND json_type(NEW.frontier_json, '$.observed_through') IS 'integer'
                  AND json_type(NEW.frontier_json, '$.committed_through') IS 'integer'
                  AND json_extract(NEW.frontier_json, '$.observed_through')
                      = binding.target_frontier
                  AND json_extract(NEW.frontier_json, '$.committed_through')
                      BETWEEN binding.source_frontier AND binding.target_frontier
                  AND json_type(NEW.coverage_json, '$.visible') IS 'integer'
                  AND json_type(NEW.coverage_json, '$.hidden') IS 'integer'
                  AND json_type(NEW.coverage_json, '$.unknown') IS 'integer'
                  AND json_type(NEW.coverage_json, '$.redacted') IS 'integer'
                  AND json_extract(NEW.coverage_json, '$.visible') >= 0
                  AND json_extract(NEW.coverage_json, '$.hidden') >= 0
                  AND json_extract(NEW.coverage_json, '$.unknown') >= 0
                  AND json_extract(NEW.coverage_json, '$.redacted') >= 0
                  AND NEW.committed_records =
                      json_extract(NEW.coverage_json, '$.visible')
                      + json_extract(NEW.coverage_json, '$.hidden')
                      + json_extract(NEW.coverage_json, '$.unknown')
                      + json_extract(NEW.coverage_json, '$.redacted')
                  AND (
                    (
                      NEW.progress_ordinal = 0
                      AND NEW.committed_batches = 0
                      AND NEW.committed_records = 0
                      AND json_extract(NEW.frontier_json, '$.committed_through')
                          = binding.source_frontier
                      AND NOT EXISTS (
                          SELECT 1
                          FROM session_refresh_progress AS seeded
                          WHERE seeded.session_id = NEW.session_id
                            AND seeded.operation_id = NEW.operation_id
                      )
                    )
                    OR (
                      NEW.committed_batches > 0
                      AND NEW.progress_ordinal = NEW.committed_batches - 1
                      AND EXISTS (
                          SELECT 1
                          FROM session_temporal_projection_receipts AS receipt
                          WHERE receipt.session_id = binding.session_id
                            AND receipt.generation = binding.generation
                            AND receipt.batch_ordinal = NEW.progress_ordinal
                            AND length(receipt.batch_digest) = 71
                            AND receipt.batch_digest GLOB 'sha256:[0-9a-f]*'
                            AND substr(receipt.batch_digest, 8) NOT GLOB '*[^0-9a-f]*'
                            AND receipt.projection_through =
                                json_extract(NEW.frontier_json, '$.committed_through')
                            AND NEW.committed_records =
                                receipt.occurrence_count
                                + receipt.copy_count
                                + receipt.assertion_count
                            AND (
                              (
                                NEW.progress_ordinal = 0
                                AND receipt.source_through >= binding.source_frontier
                                AND receipt.source_through <=
                                    json_extract(NEW.frontier_json, '$.committed_through')
                                AND NOT EXISTS (
                                    SELECT 1
                                    FROM session_refresh_progress AS first_previous
                                    WHERE first_previous.session_id = NEW.session_id
                                      AND first_previous.operation_id = NEW.operation_id
                                )
                              )
                              OR EXISTS (
                                  SELECT 1
                                  FROM session_refresh_progress AS previous
                                  WHERE previous.session_id = NEW.session_id
                                    AND previous.operation_id = NEW.operation_id
                                    AND previous.progress_ordinal =
                                        NEW.progress_ordinal - 1
                                    AND previous.progress_ordinal = (
                                        SELECT MAX(latest.progress_ordinal)
                                        FROM session_refresh_progress AS latest
                                        WHERE latest.session_id = NEW.session_id
                                          AND latest.operation_id = NEW.operation_id
                                    )
                                    AND NEW.committed_batches =
                                        previous.committed_batches + 1
                                    AND NEW.committed_records >= previous.committed_records
                                    AND NEW.recorded_at > previous.recorded_at
                                    AND json_extract(
                                        NEW.frontier_json, '$.committed_through'
                                    ) > json_extract(
                                        previous.frontier_json, '$.committed_through'
                                    )
                                    AND receipt.source_through >= json_extract(
                                        previous.frontier_json, '$.committed_through'
                                    )
                                    AND receipt.source_through <= json_extract(
                                        NEW.frontier_json, '$.committed_through'
                                    )
                                    AND json_extract(NEW.coverage_json, '$.visible')
                                        >= json_extract(previous.coverage_json, '$.visible')
                                    AND json_extract(NEW.coverage_json, '$.hidden')
                                        >= json_extract(previous.coverage_json, '$.hidden')
                                    AND json_extract(NEW.coverage_json, '$.unknown')
                                        >= json_extract(previous.coverage_json, '$.unknown')
                                    AND json_extract(NEW.coverage_json, '$.redacted')
                                        >= json_extract(previous.coverage_json, '$.redacted')
                              )
                            )
                      )
                  )
            )
            )
            BEGIN SELECT RAISE(ABORT, 'invalid session refresh progress'); END",
    },
    Trigger {
        name: "session_refresh_batch_bindings_insert_guard_v1",
        table: "session_refresh_batch_bindings",
        create_sql: "CREATE TRIGGER session_refresh_batch_bindings_insert_guard_v1
            BEFORE INSERT ON session_refresh_batch_bindings
            WHEN NEW.progress_ordinal <> NEW.batch_ordinal
              OR NOT EXISTS (
                  SELECT 1
                  FROM session_refresh_bindings AS binding
                  JOIN session_refresh_progress AS progress
                    ON progress.session_id = binding.session_id
                   AND progress.operation_id = binding.operation_id
                   AND progress.progress_ordinal = NEW.progress_ordinal
                  JOIN session_temporal_projection_receipts AS receipt
                    ON receipt.session_id = binding.session_id
                   AND receipt.generation = binding.generation
                   AND receipt.batch_ordinal = NEW.batch_ordinal
                  WHERE binding.session_id = NEW.session_id
                    AND binding.operation_id = NEW.operation_id
                    AND binding.generation = NEW.generation
              )
            BEGIN SELECT RAISE(ABORT, 'invalid session refresh batch binding'); END",
    },
    Trigger {
        name: "session_refresh_receipts_insert_guard_v1",
        table: "session_refresh_receipts",
        create_sql: "CREATE TRIGGER session_refresh_receipts_insert_guard_v1
            BEFORE INSERT ON session_refresh_receipts
            WHEN NOT EXISTS (
                SELECT 1
                FROM session_refresh_operations AS operation
                JOIN session_refresh_bindings AS binding
                  ON binding.session_id = operation.session_id
                 AND binding.operation_id = operation.operation_id
                JOIN session_temporal_generations AS generation
                  ON generation.session_id = binding.session_id
                 AND generation.generation = binding.generation
                JOIN session_refresh_progress AS progress
                  ON progress.session_id = operation.session_id
                 AND progress.operation_id = operation.operation_id
                WHERE operation.session_id = NEW.session_id
                  AND operation.operation_id = NEW.operation_id
                  AND operation.state = NEW.terminal_state
                  AND operation.state IN ('complete', 'failed', 'cancelled')
                  AND operation.terminal_at = NEW.terminal_at
                  AND operation.failure_code IS NEW.failure_code
                  AND NEW.terminal_at = operation.updated_at
                  AND generation.state = CASE NEW.terminal_state
                      WHEN 'complete' THEN 'active'
                      WHEN 'failed' THEN 'failed'
                      WHEN 'cancelled' THEN 'cancelled'
                  END
                  AND binding.source_frontier =
                      json_extract(operation.target_frontier_json, '$.committed_through')
                  AND binding.target_frontier =
                      json_extract(operation.target_frontier_json, '$.observed_through')
                  AND progress.progress_ordinal = (
                      SELECT MAX(latest.progress_ordinal)
                      FROM session_refresh_progress AS latest
                      WHERE latest.session_id = NEW.session_id
                        AND latest.operation_id = NEW.operation_id
                  )
                  AND progress.frontier_json = NEW.frontier_json
                  AND progress.coverage_json = NEW.coverage_json
                  AND NEW.terminal_at >= progress.recorded_at
                  AND json_type(NEW.frontier_json, '$.observed_through') IS 'integer'
                  AND json_type(NEW.frontier_json, '$.committed_through') IS 'integer'
                  AND json_extract(NEW.frontier_json, '$.observed_through')
                      = binding.target_frontier
                  AND json_extract(NEW.frontier_json, '$.committed_through')
                      BETWEEN binding.source_frontier AND binding.target_frontier
                  AND (
                    NEW.terminal_state <> 'complete'
                    OR json_extract(NEW.frontier_json, '$.committed_through')
                        = binding.target_frontier
                  )
                  AND (
                    (
                      NEW.terminal_state IN ('failed', 'cancelled')
                      AND progress.progress_ordinal = 0
                      AND progress.committed_batches = 0
                      AND progress.committed_records = 0
                      AND json_extract(progress.frontier_json, '$.committed_through')
                          = binding.source_frontier
                      AND NOT EXISTS (
                          SELECT 1
                          FROM session_refresh_batch_bindings AS empty_binding
                          WHERE empty_binding.session_id = progress.session_id
                            AND empty_binding.operation_id = progress.operation_id
                      )
                    )
                    OR (
                      progress.committed_batches > 0
                      AND progress.progress_ordinal = progress.committed_batches - 1
                      AND EXISTS (
                          SELECT 1
                          FROM session_refresh_batch_bindings AS batch_binding
                          JOIN session_temporal_projection_receipts AS receipt
                            ON receipt.session_id = batch_binding.session_id
                           AND receipt.generation = batch_binding.generation
                           AND receipt.batch_ordinal = batch_binding.batch_ordinal
                          WHERE batch_binding.session_id = progress.session_id
                            AND batch_binding.operation_id = progress.operation_id
                            AND batch_binding.progress_ordinal = progress.progress_ordinal
                            AND batch_binding.generation = binding.generation
                            AND batch_binding.batch_ordinal = progress.progress_ordinal
                            AND length(receipt.batch_digest) = 71
                            AND receipt.batch_digest GLOB 'sha256:[0-9a-f]*'
                            AND substr(receipt.batch_digest, 8) NOT GLOB '*[^0-9a-f]*'
                            AND receipt.projection_through =
                                json_extract(progress.frontier_json, '$.committed_through')
                            AND progress.committed_records =
                                receipt.occurrence_count
                                + receipt.copy_count
                                + receipt.assertion_count
                      )
                    )
                  )
            )
            BEGIN SELECT RAISE(ABORT, 'invalid session refresh terminal receipt'); END",
    },
];

const SESSION_TEMPORAL_GENERATION_GUARDS: &[Trigger] = &[
    Trigger {
        name: "session_temporal_generations_insert_guard_v1",
        table: "session_temporal_generations",
        create_sql: "CREATE TRIGGER session_temporal_generations_insert_guard_v1
            BEFORE INSERT ON session_temporal_generations
            WHEN NEW.state <> 'building'
            BEGIN SELECT RAISE(ABORT, 'session temporal generations must start building'); END",
    },
    Trigger {
        name: "session_temporal_generations_state_guard_v1",
        table: "session_temporal_generations",
        create_sql: "CREATE TRIGGER session_temporal_generations_state_guard_v1
            BEFORE UPDATE ON session_temporal_generations
            WHEN OLD.session_id IS NOT NEW.session_id
              OR OLD.generation IS NOT NEW.generation
              OR OLD.frozen_watermarks_json IS NOT NEW.frozen_watermarks_json
              OR OLD.created_at IS NOT NEW.created_at
              OR NEW.state = OLD.state
              OR NOT (
                (OLD.state = 'building' AND NEW.state = 'ready'
                    AND NEW.ready_at >= OLD.created_at
                    AND NEW.activated_at IS NULL
                    AND NEW.completed_at IS NULL)
                OR (OLD.state = 'building' AND NEW.state IN ('failed', 'cancelled')
                    AND NEW.ready_at IS NULL
                    AND NEW.activated_at IS NULL
                    AND NEW.completed_at >= OLD.created_at)
                OR (OLD.state = 'ready' AND NEW.state = 'active'
                    AND NEW.ready_at IS OLD.ready_at
                    AND NEW.activated_at >= OLD.ready_at
                    AND NEW.completed_at IS NULL)
                OR (OLD.state = 'ready' AND NEW.state IN ('failed', 'cancelled')
                    AND NEW.ready_at IS OLD.ready_at
                    AND NEW.activated_at IS NULL
                    AND NEW.completed_at >= OLD.ready_at)
                OR (OLD.state = 'active' AND NEW.state = 'superseded'
                    AND NEW.ready_at IS OLD.ready_at
                    AND NEW.activated_at IS OLD.activated_at
                    AND NEW.completed_at >= OLD.activated_at)
              )
            BEGIN SELECT RAISE(ABORT, 'invalid session temporal generation transition'); END",
    },
    Trigger {
        name: "session_temporal_generations_single_active_insert_v1",
        table: "session_temporal_generations",
        create_sql: "CREATE TRIGGER session_temporal_generations_single_active_insert_v1
            BEFORE INSERT ON session_temporal_generations
            WHEN NEW.state = 'active' AND EXISTS (
                SELECT 1 FROM session_temporal_generations
                WHERE session_id = NEW.session_id AND state = 'active'
            ) BEGIN SELECT RAISE(ABORT, 'session already has an active temporal generation'); END",
    },
    Trigger {
        name: "session_temporal_generations_single_active_update_v1",
        table: "session_temporal_generations",
        create_sql: "CREATE TRIGGER session_temporal_generations_single_active_update_v1
            BEFORE UPDATE OF state ON session_temporal_generations
            WHEN NEW.state = 'active' AND EXISTS (
                SELECT 1 FROM session_temporal_generations
                WHERE session_id = NEW.session_id
                  AND state = 'active'
                  AND generation <> NEW.generation
            ) BEGIN SELECT RAISE(ABORT, 'session already has an active temporal generation'); END",
    },
    Trigger {
        name: "session_temporal_generations_delete_guard_v1",
        table: "session_temporal_generations",
        create_sql: "CREATE TRIGGER session_temporal_generations_delete_guard_v1
            BEFORE DELETE ON session_temporal_generations BEGIN
                SELECT RAISE(ABORT, 'session temporal generations are durable');
            END",
    },
];

const SESSION_CURSOR_KEY_GUARDS: &[Trigger] = &[
    Trigger {
        name: "session_query_cursor_keys_insert_guard_v1",
        table: "session_query_cursor_keys",
        create_sql: "CREATE TRIGGER session_query_cursor_keys_insert_guard_v1
            BEFORE INSERT ON session_query_cursor_keys
            WHEN NEW.retired_at IS NOT NULL
              OR (
                EXISTS (SELECT 1 FROM session_query_cursor_keys)
                AND (
                  SELECT COUNT(*) FROM session_query_cursor_keys
                  WHERE retired_at IS NULL
                ) <> 1
              )
              OR NEW.key_version <= COALESCE((
                SELECT MAX(key_version) FROM session_query_cursor_keys
              ), 0)
              OR NEW.created_at <= COALESCE((
                SELECT MAX(
                    CASE
                        WHEN retired_at > created_at THEN retired_at
                        ELSE created_at
                    END
                )
                FROM session_query_cursor_keys
              ), NEW.created_at - 1)
            BEGIN SELECT RAISE(ABORT, 'session cursor key rotation must be strictly monotonic'); END",
    },
    Trigger {
        name: "session_query_cursor_keys_rotate_insert_v1",
        table: "session_query_cursor_keys",
        create_sql: "CREATE TRIGGER session_query_cursor_keys_rotate_insert_v1
            AFTER INSERT ON session_query_cursor_keys
            WHEN EXISTS (
                SELECT 1 FROM session_query_cursor_keys
                WHERE key_id <> NEW.key_id AND retired_at IS NULL
              )
            BEGIN
                UPDATE session_query_cursor_keys
                SET retired_at = NEW.created_at
                WHERE key_id <> NEW.key_id AND retired_at IS NULL;
            END",
    },
    Trigger {
        name: "session_query_cursor_keys_retire_update_v1",
        table: "session_query_cursor_keys",
        create_sql: "CREATE TRIGGER session_query_cursor_keys_retire_update_v1
            BEFORE UPDATE ON session_query_cursor_keys
            WHEN OLD.key_id IS NOT NEW.key_id
              OR OLD.key_version IS NOT NEW.key_version
              OR OLD.key_material IS NOT NEW.key_material
              OR OLD.created_at IS NOT NEW.created_at
              OR OLD.retired_at IS NOT NULL
              OR NEW.retired_at IS NULL
              OR NEW.retired_at < OLD.created_at
              OR NOT EXISTS (
                SELECT 1 FROM session_query_cursor_keys AS replacement
                WHERE replacement.key_id <> OLD.key_id
                  AND replacement.retired_at IS NULL
                  AND replacement.key_version > OLD.key_version
                  AND replacement.created_at > OLD.created_at
                  AND NEW.retired_at = replacement.created_at
              )
            BEGIN SELECT RAISE(ABORT, 'invalid session cursor key retirement'); END",
    },
];

const SESSION_SUMMARY_OWNER_GUARDS: &[Trigger] = &[
    Trigger {
        name: "session_external_payload_manifests_owner_guard_v1",
        table: "session_external_payload_manifests",
        create_sql: "CREATE TRIGGER session_external_payload_manifests_owner_guard_v1
            BEFORE INSERT ON session_external_payload_manifests
            WHEN NOT EXISTS (
                SELECT 1 FROM lcm_external_payloads
                WHERE payload_ref = NEW.payload_ref AND session_id = NEW.session_id
            ) BEGIN SELECT RAISE(ABORT, 'session payload manifest crosses sessions'); END",
    },
    Trigger {
        name: "session_summary_availability_owner_insert_v1",
        table: "session_summary_availability",
        create_sql: "CREATE TRIGGER session_summary_availability_owner_insert_v1
            BEFORE INSERT ON session_summary_availability
            WHEN NOT EXISTS (
                SELECT 1 FROM session_summary_nodes
                WHERE summary_id = NEW.summary_id AND session_id = NEW.session_id
            ) BEGIN SELECT RAISE(ABORT, 'session summary availability crosses sessions'); END",
    },
    Trigger {
        name: "session_summary_availability_owner_update_v1",
        table: "session_summary_availability",
        create_sql: "CREATE TRIGGER session_summary_availability_owner_update_v1
            BEFORE UPDATE OF session_id, summary_id ON session_summary_availability
            WHEN NOT EXISTS (
                SELECT 1 FROM session_summary_nodes
                WHERE summary_id = NEW.summary_id AND session_id = NEW.session_id
            ) BEGIN SELECT RAISE(ABORT, 'session summary availability crosses sessions'); END",
    },
];

const SESSION_TEMPORAL_FTS: &[Trigger] = &[
    Trigger {
        name: "session_occurrences_fts_insert_v1",
        table: "session_occurrences",
        create_sql: "CREATE TRIGGER session_occurrences_fts_insert_v1
            AFTER INSERT ON session_occurrences BEGIN
                INSERT INTO session_occurrences_fts(rowid, index_text, snippet_text)
                VALUES (NEW.rowid, NEW.index_text, NEW.snippet_text);
            END",
    },
    Trigger {
        name: "session_occurrences_fts_delete_v1",
        table: "session_occurrences",
        create_sql: "CREATE TRIGGER session_occurrences_fts_delete_v1
            AFTER DELETE ON session_occurrences BEGIN
                INSERT INTO session_occurrences_fts(
                    session_occurrences_fts, rowid, index_text, snippet_text
                )
                VALUES ('delete', OLD.rowid, OLD.index_text, OLD.snippet_text);
            END",
    },
    Trigger {
        name: "session_occurrences_fts_update_v1",
        table: "session_occurrences",
        create_sql: "CREATE TRIGGER session_occurrences_fts_update_v1
            AFTER UPDATE OF index_text, snippet_text ON session_occurrences BEGIN
                INSERT INTO session_occurrences_fts(
                    session_occurrences_fts, rowid, index_text, snippet_text
                )
                VALUES ('delete', OLD.rowid, OLD.index_text, OLD.snippet_text);
                INSERT INTO session_occurrences_fts(rowid, index_text, snippet_text)
                VALUES (NEW.rowid, NEW.index_text, NEW.snippet_text);
            END",
    },
    Trigger {
        name: "session_summary_nodes_fts_insert_v1",
        table: "session_summary_nodes",
        create_sql: "CREATE TRIGGER session_summary_nodes_fts_insert_v1
            AFTER INSERT ON session_summary_nodes BEGIN
                INSERT INTO session_summary_nodes_fts(rowid, summary_text, index_text)
                VALUES (NEW.rowid, NEW.summary_text, NEW.index_text);
            END",
    },
    Trigger {
        name: "session_summary_nodes_fts_delete_v1",
        table: "session_summary_nodes",
        create_sql: "CREATE TRIGGER session_summary_nodes_fts_delete_v1
            AFTER DELETE ON session_summary_nodes BEGIN
                INSERT INTO session_summary_nodes_fts(
                    session_summary_nodes_fts, rowid, summary_text, index_text
                )
                VALUES ('delete', OLD.rowid, OLD.summary_text, OLD.index_text);
            END",
    },
    Trigger {
        name: "session_summary_nodes_fts_update_v1",
        table: "session_summary_nodes",
        create_sql: "CREATE TRIGGER session_summary_nodes_fts_update_v1
            AFTER UPDATE OF summary_text, index_text ON session_summary_nodes BEGIN
                INSERT INTO session_summary_nodes_fts(
                    session_summary_nodes_fts, rowid, summary_text, index_text
                )
                VALUES ('delete', OLD.rowid, OLD.summary_text, OLD.index_text);
                INSERT INTO session_summary_nodes_fts(rowid, summary_text, index_text)
                VALUES (NEW.rowid, NEW.summary_text, NEW.index_text);
            END",
    },
];

pub(super) const FOREIGN_KEY_AUDIT_QUERY: &str = "SELECT * FROM pragma_foreign_key_check";

pub(in crate::schema_contract) const INVARIANTS: &[Invariant] = &[
    Invariant {
        triggers: OBSERVATION_IMMUTABILITY,
        audit_query: None,
        violation: "observation immutability trigger contract is unavailable",
    },
    Invariant {
        triggers: RECEIPT_IMMUTABILITY,
        audit_query: None,
        violation: "sanitization receipt immutability trigger contract is unavailable",
    },
    Invariant {
        triggers: PROJECTION_AUDIT_INVALIDATION,
        audit_query: None,
        violation: "projection authority audit invalidation contract is unavailable",
    },
    Invariant {
        triggers: STORE_PROJECT_IMMUTABILITY,
        audit_query: None,
        violation: "store project identity is not immutable",
    },
    Invariant {
        triggers: GRAPH_SCOPE_IDENTITY,
        audit_query: Some(
            "SELECT 1 FROM graph_scopes AS scope
             LEFT JOIN store_instances AS store
               ON store.store_id = scope.store_id AND store.project_id = scope.project_id
             WHERE store.store_id IS NULL LIMIT 1",
        ),
        violation: "graph_scopes contains a store/project identity mismatch",
    },
    Invariant {
        triggers: QUEUE_IDENTITY,
        audit_query: Some(
            "SELECT 1 FROM projection_queue AS queue
             LEFT JOIN observations AS observation
               ON observation.observation_id = queue.observation_id
              AND observation.sequence = queue.observation_sequence
             WHERE observation.observation_id IS NULL LIMIT 1",
        ),
        violation: "projection_queue contains an observation identity mismatch",
    },
    Invariant {
        triggers: PROVENANCE_RECEIPT,
        audit_query: Some(
            "SELECT 1 FROM observation_projection_provenance AS provenance
             LEFT JOIN observations AS observation
               ON observation.observation_id = provenance.observation_id
              AND observation.receipt_id = provenance.receipt_id
             WHERE observation.observation_id IS NULL LIMIT 1",
        ),
        violation: "observation projection provenance contains a receipt mismatch",
    },
    Invariant {
        triggers: WORKFLOW_FACT_RECEIPT,
        audit_query: Some(
            "SELECT 1 FROM observation_workflow_facts AS workflow
             LEFT JOIN observations AS observation
               ON observation.observation_id = workflow.observation_id
              AND observation.receipt_id = workflow.receipt_id
              AND observation.sequence = workflow.observation_sequence
             WHERE observation.observation_id IS NULL LIMIT 1",
        ),
        violation: "workflow projection contains an observation receipt mismatch",
    },
    Invariant {
        triggers: DISPOSITION_RECEIPT,
        audit_query: Some(
            "SELECT 1 FROM observation_projection_dispositions AS disposition
             LEFT JOIN observations AS observation
               ON observation.observation_id = disposition.observation_id
              AND observation.receipt_id = disposition.receipt_id
             WHERE observation.observation_id IS NULL LIMIT 1",
        ),
        violation: "observation projection disposition contains a receipt mismatch",
    },
    Invariant {
        triggers: MESSAGE_CREATED_DOMAIN,
        audit_query: Some(
            "SELECT 1 FROM observation_projection_provenance
             WHERE message_created NOT IN (0, 1) LIMIT 1",
        ),
        violation: "observation projection provenance contains invalid message_created",
    },
    Invariant {
        triggers: CHECKPOINT_DOMAIN,
        audit_query: Some(
            "SELECT 1 FROM observation_projection_checkpoints
             WHERE last_sequence < 0 LIMIT 1",
        ),
        violation: "observation projection checkpoints contains a negative sequence",
    },
    Invariant {
        triggers: &[],
        audit_query: Some(
            "SELECT 1 FROM observations AS observation
             LEFT JOIN sanitization_receipts AS receipt
               ON receipt.receipt_id = observation.receipt_id
             WHERE receipt.receipt_id IS NULL LIMIT 1",
        ),
        violation: "committed observation references a missing receipt",
    },
    Invariant {
        triggers: &[],
        audit_query: Some(
            "SELECT 1 FROM observations
             WHERE NOT json_valid(observation_json)
                OR NOT json_valid(committed_cursor_json)
                OR (json_type(observation_json, '$.observation_id') IS NOT NULL
                    AND json_extract(observation_json, '$.observation_id') != observation_id)
             LIMIT 1",
        ),
        violation: "committed observation contains invalid authority JSON",
    },
    Invariant {
        triggers: &[],
        audit_query: Some(
            "SELECT 1 FROM observation_projection_checkpoints
             WHERE last_sequence > (SELECT COALESCE(MAX(sequence), 0) FROM observations)
             LIMIT 1",
        ),
        violation: "projection checkpoint exceeds the committed observation frontier",
    },
    Invariant {
        triggers: &[],
        audit_query: Some(FOREIGN_KEY_AUDIT_QUERY),
        violation: "global database contains a foreign-key violation",
    },
    Invariant {
        triggers: SOURCE_CURSOR_ADVANCE_IMMUTABILITY,
        audit_query: None,
        violation: "source cursor advance immutability trigger contract is unavailable",
    },
    Invariant {
        triggers: SESSION_SUMMARY_AUTHORITY_IMMUTABILITY,
        audit_query: None,
        violation: "session summary payload authority is mutable",
    },
    Invariant {
        triggers: SESSION_RECEIPT_IMMUTABILITY,
        audit_query: Some(
            "SELECT 1
             WHERE EXISTS (
                SELECT 1
                FROM session_temporal_observation_effects AS effect
                LEFT JOIN observations AS observation
                  ON observation.observation_id = effect.observation_id
                 AND observation.sequence = effect.observation_sequence
                 AND observation.receipt_id = effect.receipt_id
                WHERE observation.observation_id IS NULL
             )
             OR EXISTS (
                SELECT 1
                FROM session_temporal_projection_receipts AS receipt
                LEFT JOIN session_temporal_generations AS generation
                  ON generation.session_id = receipt.session_id
                 AND generation.generation = receipt.generation
                 AND generation.frozen_watermarks_json = receipt.frozen_watermarks_json
                WHERE generation.session_id IS NULL
                   OR (
                        receipt.batch_ordinal > 0
                        AND NOT EXISTS (
                            SELECT 1 FROM session_temporal_projection_receipts AS previous
                            WHERE previous.session_id = receipt.session_id
                              AND previous.generation = receipt.generation
                              AND previous.batch_ordinal = receipt.batch_ordinal - 1
                              AND previous.source_through <= receipt.source_through
                              AND previous.projection_through <= receipt.projection_through
                        )
                   )
             )
             LIMIT 1",
        ),
        violation: "session temporal receipts or cursor keys are mutable",
    },
    Invariant {
        triggers: SESSION_CURSOR_KEY_GUARDS,
        audit_query: Some(
            "SELECT 1
             WHERE EXISTS (SELECT 1 FROM session_query_cursor_keys)
               AND (
                 (SELECT COUNT(*) FROM session_query_cursor_keys WHERE retired_at IS NULL) <> 1
                 OR EXISTS (
                    SELECT 1
                    FROM session_query_cursor_keys AS active
                    WHERE active.retired_at IS NULL
                      AND (
                        active.key_version <> (
                            SELECT MAX(key_version) FROM session_query_cursor_keys
                        )
                        OR active.created_at <> (
                            SELECT MAX(created_at) FROM session_query_cursor_keys
                        )
                      )
                 )
                 OR EXISTS (
                    SELECT 1 FROM session_query_cursor_keys
                    WHERE retired_at IS NOT NULL AND retired_at < created_at
                 )
                 OR EXISTS (
                    SELECT 1
                    FROM session_query_cursor_keys AS older
                    JOIN session_query_cursor_keys AS newer
                      ON newer.key_version > older.key_version
                    WHERE newer.created_at <= older.created_at
                 )
                 OR EXISTS (
                    SELECT 1
                    FROM session_query_cursor_keys AS retired
                    WHERE retired.retired_at IS NOT NULL
                      AND (
                        SELECT successor.created_at
                        FROM session_query_cursor_keys AS successor
                        WHERE successor.key_version > retired.key_version
                        ORDER BY successor.key_version
                        LIMIT 1
                      )
                      IS NOT retired.retired_at
                 )
               )
             LIMIT 1",
        ),
        violation: "session cursor key rotation state is invalid",
    },
    Invariant {
        triggers: SESSION_REFRESH_STATE_GUARDS,
        audit_query: Some(
            "SELECT 1
             WHERE EXISTS (
                SELECT 1 FROM session_refresh_operations
                WHERE (state = 'running' AND (terminal_at IS NOT NULL OR failure_code IS NOT NULL))
                   OR (state = 'complete' AND (terminal_at IS NULL OR failure_code IS NOT NULL))
                   OR (state = 'failed' AND (terminal_at IS NULL OR failure_code IS NULL))
                   OR (state = 'cancelled' AND (terminal_at IS NULL OR failure_code IS NOT NULL))
                   OR updated_at < created_at
                   OR (state <> 'running' AND terminal_at <> updated_at)
             )
             OR EXISTS (
                SELECT 1
                FROM session_refresh_operations
                WHERE state = 'running'
                GROUP BY session_id
                HAVING COUNT(*) > 1
             )
             OR EXISTS (
                SELECT 1
                FROM session_refresh_bindings AS binding
                JOIN session_refresh_operations AS operation
                  ON operation.session_id = binding.session_id
                 AND operation.operation_id = binding.operation_id
                LEFT JOIN session_temporal_generations AS generation
                  ON generation.session_id = binding.session_id
                 AND generation.generation = binding.generation
                WHERE generation.generation IS NULL
                   OR operation.request_digest <> binding.binding_digest
                   OR operation.created_at <> binding.created_at
                   OR binding.source_frontier <>
                       json_extract(operation.target_frontier_json, '$.committed_through')
                   OR binding.target_frontier <>
                       json_extract(operation.target_frontier_json, '$.observed_through')
                   OR generation.frozen_watermarks_json <> binding.frozen_watermarks_json
             )
             OR EXISTS (
                SELECT 1
                FROM session_refresh_batch_bindings AS batch
                LEFT JOIN session_refresh_bindings AS binding
                  ON binding.session_id = batch.session_id
                 AND binding.operation_id = batch.operation_id
                 AND binding.generation = batch.generation
                LEFT JOIN session_refresh_progress AS progress
                  ON progress.session_id = batch.session_id
                 AND progress.operation_id = batch.operation_id
                 AND progress.progress_ordinal = batch.progress_ordinal
                LEFT JOIN session_temporal_projection_receipts AS receipt
                  ON receipt.session_id = batch.session_id
                 AND receipt.generation = batch.generation
                 AND receipt.batch_ordinal = batch.batch_ordinal
                WHERE binding.operation_id IS NULL
                   OR progress.operation_id IS NULL
                   OR receipt.session_id IS NULL
                   OR batch.progress_ordinal <> batch.batch_ordinal
             )
             OR EXISTS (
                SELECT 1
                FROM session_refresh_progress AS progress
                JOIN session_refresh_operations AS operation
                  ON operation.session_id = progress.session_id
                 AND operation.operation_id = progress.operation_id
                WHERE progress.recorded_at < operation.created_at
             )
             OR EXISTS (
                SELECT 1
                FROM session_refresh_operations AS operation
                LEFT JOIN session_refresh_bindings AS binding
                  ON binding.session_id = operation.session_id
                 AND binding.operation_id = operation.operation_id
                LEFT JOIN session_refresh_receipts AS receipt
                  ON receipt.session_id = operation.session_id
                 AND receipt.operation_id = operation.operation_id
                LEFT JOIN session_temporal_generations AS generation
                  ON generation.session_id = binding.session_id
                 AND generation.generation = binding.generation
                WHERE binding.operation_id IS NULL
             )
             LIMIT 1",
        ),
        violation: "session refresh operation state is invalid",
    },
    Invariant {
        triggers: SESSION_TEMPORAL_GENERATION_GUARDS,
        audit_query: Some(
            "SELECT 1 FROM session_temporal_generations
             WHERE state = 'active'
             GROUP BY session_id
             HAVING COUNT(*) > 1
             LIMIT 1",
        ),
        violation: "session temporal generation state is invalid",
    },
    Invariant {
        triggers: SESSION_SUMMARY_OWNER_GUARDS,
        audit_query: Some(
            "SELECT 1
             FROM session_summary_availability AS availability
             JOIN session_summary_nodes AS summary
               ON summary.summary_id = availability.summary_id
             WHERE summary.session_id <> availability.session_id
             LIMIT 1",
        ),
        violation: "session temporal authority ownership is invalid",
    },
    Invariant {
        triggers: SESSION_TEMPORAL_FTS,
        audit_query: None,
        violation: "session temporal full-text trigger contract is unavailable",
    },
];

pub(super) async fn replace_trigger(
    conn: &impl Executor,
    trigger: &Trigger,
) -> tracedecay_runtime_core::errors::Result<()> {
    conn.execute_batch(&format!(
        "DROP TRIGGER IF EXISTS \"{}\";\n{};",
        trigger.name, trigger.create_sql
    ))
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))
}

pub(super) async fn trigger_contracts_intact(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<bool> {
    for invariant in INVARIANTS {
        for trigger in invariant.triggers {
            if !trigger_matches(conn, trigger).await? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

async fn trigger_matches(
    conn: &impl QueryExecutor,
    trigger: &Trigger,
) -> tracedecay_runtime_core::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT tbl_name, sql FROM sqlite_master
             WHERE type = 'trigger' AND name = ?1 COLLATE NOCASE",
            params![trigger.name],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    else {
        return Ok(false);
    };
    let table = row
        .get::<String>(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let sql = row
        .get::<String>(1)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(table.eq_ignore_ascii_case(trigger.table)
        && normalize_trigger_sql(&sql) == normalize_trigger_sql(trigger.create_sql))
}

pub async fn suspend_immutability_for_canonical_repair(
    conn: &impl Executor,
) -> tracedecay_runtime_core::errors::Result<()> {
    for trigger in OBSERVATION_IMMUTABILITY.iter().chain(RECEIPT_IMMUTABILITY) {
        if !trigger_matches(conn, trigger).await? {
            return Err(authority_violation(format!(
                "cannot suspend incompatible canonical authority trigger '{}'",
                trigger.name
            )));
        }
    }
    for trigger in OBSERVATION_IMMUTABILITY.iter().chain(RECEIPT_IMMUTABILITY) {
        conn.execute(&format!("DROP TRIGGER \"{}\"", trigger.name), ())
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    Ok(())
}

pub async fn restore_immutability_after_canonical_repair(
    conn: &impl Executor,
) -> tracedecay_runtime_core::errors::Result<()> {
    for trigger in OBSERVATION_IMMUTABILITY.iter().chain(RECEIPT_IMMUTABILITY) {
        replace_trigger(conn, trigger).await?;
        if !trigger_matches(conn, trigger).await? {
            return Err(authority_violation(format!(
                "canonical authority trigger '{}' was not restored",
                trigger.name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::tests::harness::RegisteredGlobalDbHarness;
    use tracedecay_runtime_core::db::engine::params;

    /// Registry rows the identity triggers key off. `upsert_code_project`
    /// refuses temporary roots, so the fixture writes the authority rows
    /// directly rather than through the admission door.
    const SEED_PROJECTS: &str = "INSERT INTO code_projects
         (project_id, canonical_root, display_root, created_at, last_seen_at)
         VALUES ('project_one', '/one', '/one', 1, 1), ('project_two', '/two', '/two', 1, 1);
         INSERT INTO store_instances
         (store_id, project_id, store_kind, storage_mode, store_relpath, created_at)
         VALUES ('store_one', 'project_one', 'sessions', 'central', 'sessions', 1);";

    /// A store row carries the only binding between a physical store and the
    /// project that owns it. Letting `project_id` move would silently hand one
    /// user's sessions to another project, so the update is refused outright.
    #[tokio::test]
    async fn store_project_identity_cannot_be_reparented() {
        let harness = RegisteredGlobalDbHarness::open("store-project-reparent").await;
        let transaction = harness
            .registered
            .begin_write_transaction()
            .await
            .expect("begin store identity fixture transaction");
        transaction
            .execute_batch(SEED_PROJECTS)
            .await
            .expect("seed registry identity rows");

        let error = transaction
            .execute(
                "UPDATE store_instances SET project_id = 'project_two'
                 WHERE store_id = 'store_one'",
                (),
            )
            .await
            .expect_err("store project identity must not be reparented");

        assert!(
            error
                .to_string()
                .contains("store project identity is immutable"),
            "{error}"
        );
    }

    /// The identity triggers reject rows whose cross-table keys disagree, so a
    /// graph scope cannot claim a store owned by another project and the
    /// projection queue cannot point at a sequence its observation does not
    /// have.
    #[tokio::test]
    async fn cross_table_identity_constraints_reject_mismatched_rows() {
        let harness = RegisteredGlobalDbHarness::open("cross-table-identity").await;
        let transaction = harness
            .registered
            .begin_write_transaction()
            .await
            .expect("begin cross-table identity fixture transaction");
        transaction
            .execute_batch(SEED_PROJECTS)
            .await
            .expect("seed registry identity rows");

        let graph_error = transaction
            .execute(
                "INSERT INTO graph_scopes
                 (graph_scope_id, project_id, store_id, branch_name, db_relpath)
                 VALUES ('scope_bad', 'project_two', 'store_one', 'main', 'graph.db')",
                (),
            )
            .await
            .expect_err("graph scope must not claim another project's store");
        assert!(
            graph_error.to_string().contains("store/project mismatch"),
            "{graph_error}"
        );

        transaction
            .execute_batch(
                "INSERT INTO sanitization_receipts
                 (receipt_id, sanitizer_version, payload_digest, receipt_json)
                 VALUES ('receipt_one', 'v1', 'digest_one', '{}');
                 INSERT INTO observations
                 (observation_id, payload_digest, receipt_id, observation_json,
                  committed_cursor_json)
                 VALUES ('observation_one', 'digest_one', 'receipt_one', '{}', '{}');",
            )
            .await
            .expect("seed observation identity rows");
        let mut rows = transaction
            .query(
                "SELECT sequence FROM observations WHERE observation_id = 'observation_one'",
                (),
            )
            .await
            .expect("read seeded observation sequence");
        let sequence = rows
            .next()
            .await
            .expect("observation sequence row")
            .expect("observation sequence value")
            .get::<i64>(0)
            .expect("observation sequence column");
        drop(rows);

        let queue_error = transaction
            .execute(
                "INSERT INTO projection_queue(observation_id, observation_sequence)
                 VALUES ('observation_one', ?1)",
                params![sequence + 1],
            )
            .await
            .expect_err("projection queue must not diverge from observation identity");
        assert!(
            queue_error
                .to_string()
                .contains("observation identity mismatch"),
            "{queue_error}"
        );
    }

    /// A sanitization receipt is the proof that a payload was redacted before
    /// it was committed. Rewriting or deleting one would leave sanitized data
    /// with no binding evidence, so both are refused after commit.
    #[tokio::test]
    async fn sanitization_receipts_are_immutable_after_commit() {
        let harness = RegisteredGlobalDbHarness::open("sanitization-receipt-immutability").await;
        let transaction = harness
            .registered
            .begin_write_transaction()
            .await
            .expect("begin sanitization receipt fixture transaction");
        transaction
            .execute(
                "INSERT INTO sanitization_receipts
                 (receipt_id, sanitizer_version, payload_digest, receipt_json)
                 VALUES ('receipt_immutable', 'v1', 'digest_immutable', '{}')",
                (),
            )
            .await
            .expect("seed sanitization receipt");

        for statement in [
            "UPDATE sanitization_receipts SET payload_digest = payload_digest
             WHERE receipt_id = ?1",
            "DELETE FROM sanitization_receipts WHERE receipt_id = ?1",
        ] {
            let error = transaction
                .execute(statement, params!["receipt_immutable"])
                .await
                .expect_err("committed sanitization receipts must be immutable");
            assert!(
                error
                    .to_string()
                    .contains("sanitization receipts are immutable"),
                "{error}"
            );
        }
    }
}
