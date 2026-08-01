use crate::db::engine::Executor;
use crate::global_db_operation_error;

/// Adds the durable, project-local authority for PR11 index transactions.
///
/// Preview and input rows are immutable commitments; journal rows retain the
/// one mutable phase/epoch state; terminal receipts are append-only.  A
/// repository quarantine is retained after a proven resolution rather than
/// deleted, so recovery evidence survives restart.
pub async fn ensure_git_index_transaction_schema(
    connection: &impl Executor,
) -> crate::errors::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS git_index_preview_commitments (
                preview_id TEXT PRIMARY KEY,
                preview_digest TEXT NOT NULL UNIQUE,
                repository_id TEXT NOT NULL,
                worktree_id TEXT,
                operation TEXT NOT NULL,
                repository_snapshot_digest TEXT NOT NULL,
                commit_intent_digest TEXT,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                preview_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS git_index_transaction_inputs (
                idempotency_key TEXT PRIMARY KEY,
                input_digest TEXT NOT NULL,
                transaction_id TEXT NOT NULL UNIQUE,
                preview_id TEXT NOT NULL,
                preview_digest TEXT NOT NULL,
                repository_id TEXT NOT NULL,
                worktree_id TEXT NOT NULL,
                operation TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(preview_id)
                    REFERENCES git_index_preview_commitments(preview_id)
                    ON DELETE RESTRICT
            );
            CREATE TABLE IF NOT EXISTS git_index_transaction_journals (
                transaction_id TEXT PRIMARY KEY,
                idempotency_key TEXT NOT NULL UNIQUE,
                preview_id TEXT NOT NULL,
                preview_digest TEXT NOT NULL,
                repository_id TEXT NOT NULL,
                worktree_id TEXT NOT NULL,
                operation TEXT NOT NULL,
                expected_snapshot_digest TEXT NOT NULL,
                phase TEXT NOT NULL,
                phase_epoch INTEGER NOT NULL,
                started_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                journal_json TEXT NOT NULL,
                FOREIGN KEY(transaction_id)
                    REFERENCES git_index_transaction_inputs(transaction_id)
                    ON DELETE RESTRICT,
                FOREIGN KEY(idempotency_key)
                    REFERENCES git_index_transaction_inputs(idempotency_key)
                    ON DELETE RESTRICT,
                FOREIGN KEY(preview_id)
                    REFERENCES git_index_preview_commitments(preview_id)
                    ON DELETE RESTRICT
            );
            CREATE TABLE IF NOT EXISTS git_index_transaction_receipts (
                transaction_id TEXT PRIMARY KEY,
                receipt_id TEXT NOT NULL UNIQUE,
                preview_id TEXT NOT NULL,
                receipt_digest TEXT NOT NULL UNIQUE,
                outcome TEXT NOT NULL,
                committed_at INTEGER NOT NULL,
                receipt_json TEXT NOT NULL,
                FOREIGN KEY(transaction_id)
                    REFERENCES git_index_transaction_journals(transaction_id)
                    ON DELETE RESTRICT,
                FOREIGN KEY(preview_id)
                    REFERENCES git_index_preview_commitments(preview_id)
                    ON DELETE RESTRICT
            );
            CREATE TABLE IF NOT EXISTS git_index_repository_quarantines (
                repository_id TEXT NOT NULL,
                transaction_id TEXT NOT NULL,
                active INTEGER NOT NULL CHECK(active IN (0, 1)),
                created_at INTEGER NOT NULL,
                resolved_at INTEGER,
                resolution_receipt_json TEXT,
                PRIMARY KEY(repository_id, transaction_id),
                FOREIGN KEY(transaction_id)
                    REFERENCES git_index_transaction_journals(transaction_id)
                    ON DELETE RESTRICT
            );

            CREATE INDEX IF NOT EXISTS idx_git_index_preview_commitments_repository
                ON git_index_preview_commitments(repository_id, created_at, preview_id);
            CREATE INDEX IF NOT EXISTS idx_git_index_transaction_inputs_repository
                ON git_index_transaction_inputs(repository_id, transaction_id);
            CREATE INDEX IF NOT EXISTS idx_git_index_transaction_journals_recovery
                ON git_index_transaction_journals(repository_id, phase, updated_at, transaction_id);
            CREATE INDEX IF NOT EXISTS idx_git_index_transaction_receipts_preview
                ON git_index_transaction_receipts(preview_id, committed_at, transaction_id);
            CREATE INDEX IF NOT EXISTS idx_git_index_repository_quarantines_active
                ON git_index_repository_quarantines(repository_id, transaction_id)
                WHERE active = 1;

            CREATE TRIGGER IF NOT EXISTS git_index_preview_commitments_immutable_update
            BEFORE UPDATE ON git_index_preview_commitments
            BEGIN
                SELECT RAISE(ABORT, 'git index preview commitments are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS git_index_preview_commitments_immutable_delete
            BEFORE DELETE ON git_index_preview_commitments
            BEGIN
                SELECT RAISE(ABORT, 'git index preview commitments are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS git_index_transaction_inputs_immutable_update
            BEFORE UPDATE ON git_index_transaction_inputs
            BEGIN
                SELECT RAISE(ABORT, 'git index transaction inputs are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS git_index_transaction_inputs_immutable_delete
            BEFORE DELETE ON git_index_transaction_inputs
            BEGIN
                SELECT RAISE(ABORT, 'git index transaction inputs are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS git_index_transaction_journals_identity_immutable
            BEFORE UPDATE ON git_index_transaction_journals
            WHEN OLD.transaction_id != NEW.transaction_id
              OR OLD.idempotency_key != NEW.idempotency_key
              OR OLD.preview_id != NEW.preview_id
              OR OLD.preview_digest != NEW.preview_digest
              OR OLD.repository_id != NEW.repository_id
              OR OLD.worktree_id != NEW.worktree_id
              OR OLD.operation != NEW.operation
              OR OLD.expected_snapshot_digest != NEW.expected_snapshot_digest
              OR OLD.started_at != NEW.started_at
            BEGIN
                SELECT RAISE(ABORT, 'git index transaction journal identity is immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS git_index_transaction_journals_immutable_delete
            BEFORE DELETE ON git_index_transaction_journals
            BEGIN
                SELECT RAISE(ABORT, 'git index transaction journals are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS git_index_transaction_receipts_immutable_update
            BEFORE UPDATE ON git_index_transaction_receipts
            BEGIN
                SELECT RAISE(ABORT, 'git index transaction receipts are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS git_index_transaction_receipts_immutable_delete
            BEFORE DELETE ON git_index_transaction_receipts
            BEGIN
                SELECT RAISE(ABORT, 'git index transaction receipts are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS git_index_repository_quarantines_immutable_delete
            BEFORE DELETE ON git_index_repository_quarantines
            BEGIN
                SELECT RAISE(ABORT, 'git index repository quarantines are retained');
            END;
            CREATE TRIGGER IF NOT EXISTS git_index_repository_quarantines_identity_immutable
            BEFORE UPDATE ON git_index_repository_quarantines
            WHEN OLD.repository_id != NEW.repository_id
              OR OLD.transaction_id != NEW.transaction_id
              OR OLD.created_at != NEW.created_at
            BEGIN
                SELECT RAISE(ABORT, 'git index repository quarantine identity is immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS git_index_repository_quarantines_resolution_required
            BEFORE UPDATE ON git_index_repository_quarantines
            WHEN NEW.active = 0
              AND (NEW.resolved_at IS NULL OR NEW.resolution_receipt_json IS NULL)
            BEGIN
                SELECT RAISE(ABORT, 'git index repository quarantine clear requires a receipt');
            END;",
        )
        .await
        .map_err(|error| {
            global_db_operation_error("initialize git index transaction schema", error)
        })
}
