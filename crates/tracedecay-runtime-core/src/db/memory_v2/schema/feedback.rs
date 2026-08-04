//! Final feedback-history schema.

use crate::errors::Result;

use super::super::{MemoryV2Executor, db_error};

pub(super) async fn install_feedback_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE memory_v2_feedback_history (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            result_id TEXT NOT NULL,
            action TEXT NOT NULL CHECK(action IN ('helpful', 'unhelpful')),
            old_trust REAL NOT NULL CHECK(old_trust >= 0.0 AND old_trust <= 1.0),
            new_trust REAL NOT NULL CHECK(new_trust >= 0.0 AND new_trust <= 1.0),
            occurred_at INTEGER NOT NULL,
            source TEXT,
            note TEXT,
            details_availability TEXT NOT NULL CHECK(
                details_availability IN ('available', 'unknown')
            ),
            PRIMARY KEY(owner_kind, project_id, fact_id, result_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            ),
            CHECK(
                details_availability = 'available' OR (source IS NULL AND note IS NULL)
            )
        );

        CREATE INDEX idx_memory_v2_feedback_history_fact
            ON memory_v2_feedback_history(
                owner_kind, project_id, fact_id, occurred_at, result_id
            );
        CREATE TRIGGER memory_v2_feedback_history_only_redaction
        BEFORE UPDATE ON memory_v2_feedback_history
        WHEN NOT (
            NEW.owner_kind IS OLD.owner_kind
            AND NEW.project_id IS OLD.project_id
            AND NEW.fact_id IS OLD.fact_id
            AND NEW.result_id IS OLD.result_id
            AND NEW.action IS OLD.action
            AND NEW.old_trust IS OLD.old_trust
            AND NEW.new_trust IS OLD.new_trust
            AND NEW.occurred_at IS OLD.occurred_at
            AND NEW.source IS NULL
            AND NEW.note IS NULL
            AND (
                (OLD.details_availability = 'available'
                    AND NEW.details_availability = 'unknown')
                OR (
                    OLD.source IS NULL AND OLD.note IS NULL
                    AND NEW.details_availability IS OLD.details_availability
                )
            )
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history permits only detail redaction');
        END;
        CREATE TRIGGER memory_v2_feedback_history_no_delete
        BEFORE DELETE ON memory_v2_feedback_history BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history records are immutable');
        END;
        ",
    )
    .await
    .map_err(|error| db_error(operation, error))
}
