//! Final project-wide memory support schema.

use crate::errors::Result;

use super::super::{MemoryV2Executor, db_error};

pub(super) const FINAL_MEMORY_SUPPORT_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS memory_v2_operation_receipts (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            operation_id TEXT NOT NULL CHECK(length(operation_id) > 0),
            operation_kind TEXT NOT NULL CHECK(operation_kind IN (
                'add', 'update', 'remove', 'feedback', 'retrieval',
                'curation', 'merge', 'automatic_fact_apply'
            )),
            request_digest TEXT NOT NULL CHECK(length(request_digest) > 0),
            fact_id TEXT,
            event_id TEXT,
            receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
            recorded_at INTEGER NOT NULL,
            PRIMARY KEY(owner_kind, project_id, operation_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(event_id IS NULL OR fact_id IS NOT NULL),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_feedback_history (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            event_id TEXT NOT NULL,
            action TEXT NOT NULL CHECK(action IN ('helpful', 'unhelpful')),
            old_trust REAL NOT NULL CHECK(old_trust >= 0.0 AND old_trust <= 1.0),
            new_trust REAL NOT NULL CHECK(new_trust >= 0.0 AND new_trust <= 1.0),
            occurred_at INTEGER NOT NULL,
            source TEXT,
            note TEXT,
            details_availability TEXT NOT NULL CHECK(
                details_availability IN ('available', 'redacted', 'unknown')
            ),
            PRIMARY KEY(owner_kind, project_id, fact_id, event_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            ),
            CHECK(
                details_availability = 'available' OR (source IS NULL AND note IS NULL)
            )
        );

        CREATE INDEX IF NOT EXISTS idx_memory_v2_operation_receipts_fact
            ON memory_v2_operation_receipts(
                fact_id, owner_kind, project_id, recorded_at
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_operation_receipts_automation_run
            ON memory_v2_operation_receipts(
                owner_kind, project_id, operation_kind,
                json_extract(receipt_json, '$.automation_run_id'),
                recorded_at, operation_id
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_automatic_fact_receipts_automation_run
            ON memory_v2_automatic_fact_receipts(
                owner_kind, project_id,
                json_extract(request_json, '$.automation_run_id'),
                recorded_at, apply_id
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_feedback_history_fact
            ON memory_v2_feedback_history(
                owner_kind, project_id, fact_id, occurred_at, event_id
            );
        CREATE TRIGGER IF NOT EXISTS memory_v2_operation_receipts_no_update
        BEFORE UPDATE ON memory_v2_operation_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 operation receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_operation_receipts_no_delete
        BEFORE DELETE ON memory_v2_operation_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 operation receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_operation_receipts_no_payload
        BEFORE INSERT ON memory_v2_operation_receipts
        WHEN EXISTS (
            SELECT 1 FROM json_tree(NEW.receipt_json)
            WHERE lower(CAST(key AS TEXT)) IN (
                'content', 'payload', 'payload_json', 'metadata',
                'vector', 'vectors', 'embedding', 'embeddings',
                'vector_watermark', 'vector_watermark_json'
            )
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 operation receipts cannot retain payload data');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_only_redaction
        BEFORE UPDATE ON memory_v2_feedback_history
        WHEN NOT (
            NEW.owner_kind IS OLD.owner_kind
            AND NEW.project_id IS OLD.project_id
            AND NEW.fact_id IS OLD.fact_id
            AND NEW.event_id IS OLD.event_id
            AND NEW.action IS OLD.action
            AND NEW.old_trust IS OLD.old_trust
            AND NEW.new_trust IS OLD.new_trust
            AND NEW.occurred_at IS OLD.occurred_at
            AND NEW.source IS NULL
            AND NEW.note IS NULL
            AND (
                (OLD.details_availability = 'available'
                    AND NEW.details_availability = 'redacted')
                OR (
                    OLD.source IS NULL AND OLD.note IS NULL
                    AND NEW.details_availability IS OLD.details_availability
                )
            )
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history permits only detail redaction');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_no_delete
        BEFORE DELETE ON memory_v2_feedback_history BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history records are immutable');
        END;";

pub(super) async fn install_final_memory_support(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch(FINAL_MEMORY_SUPPORT_SCHEMA)
        .await
        .map_err(|error| db_error(operation, error))
}
