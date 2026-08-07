use crate::global_db_operation_error;
use tracedecay_runtime_core::db::engine::Executor;

/// Adds the durable, project-local authority for native integration
/// transactions (Plan 36).
///
/// Preview and approval rows are immutable commitments; the transaction row
/// retains the one mutable phase/revision status; terminal receipts are
/// append-only. A repository quarantine row is retained after creation so the
/// needs-inspection fence survives restart. The approval column on the
/// transaction table is `UNIQUE`, which is what makes an approval one-use:
/// a second transaction can never bind the same approval.
pub async fn ensure_native_integration_schema(
    connection: &impl Executor,
) -> tracedecay_runtime_core::errors::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS native_integration_previews (
                preview_id TEXT PRIMARY KEY,
                preview_digest TEXT NOT NULL UNIQUE,
                repository_id TEXT NOT NULL,
                destination_ref TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                preview_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS native_integration_approvals (
                approval_id TEXT PRIMARY KEY,
                approval_digest TEXT NOT NULL UNIQUE,
                preview_id TEXT NOT NULL,
                preview_digest TEXT NOT NULL,
                principal TEXT NOT NULL,
                issued_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                approval_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS native_integration_transactions (
                transaction_id TEXT PRIMARY KEY,
                approval_id TEXT NOT NULL UNIQUE,
                preview_id TEXT NOT NULL,
                preview_digest TEXT NOT NULL,
                repository_id TEXT NOT NULL,
                destination_ref TEXT NOT NULL,
                expected_destination_tip TEXT NOT NULL,
                phase TEXT NOT NULL,
                phase_revision INTEGER NOT NULL,
                cancellation_requested INTEGER NOT NULL CHECK(cancellation_requested IN (0, 1)),
                terminal_outcome TEXT,
                updated_at INTEGER NOT NULL,
                status_json TEXT NOT NULL,
                approval_json TEXT NOT NULL,
                FOREIGN KEY(preview_id)
                    REFERENCES native_integration_previews(preview_id)
                    ON DELETE RESTRICT
            );
            CREATE TABLE IF NOT EXISTS native_integration_receipts (
                transaction_id TEXT PRIMARY KEY,
                receipt_digest TEXT NOT NULL UNIQUE,
                preview_id TEXT NOT NULL,
                outcome TEXT NOT NULL,
                completed_at INTEGER NOT NULL,
                receipt_json TEXT NOT NULL,
                FOREIGN KEY(transaction_id)
                    REFERENCES native_integration_transactions(transaction_id)
                    ON DELETE RESTRICT
            );
            CREATE TABLE IF NOT EXISTS native_integration_repository_quarantines (
                repository_id TEXT NOT NULL,
                transaction_id TEXT NOT NULL,
                active INTEGER NOT NULL CHECK(active IN (0, 1)),
                created_at INTEGER NOT NULL,
                PRIMARY KEY(repository_id, transaction_id),
                FOREIGN KEY(transaction_id)
                    REFERENCES native_integration_transactions(transaction_id)
                    ON DELETE RESTRICT
            );

            CREATE INDEX IF NOT EXISTS idx_native_integration_previews_repository
                ON native_integration_previews(repository_id, created_at, preview_id);
            CREATE INDEX IF NOT EXISTS idx_native_integration_approvals_preview
                ON native_integration_approvals(preview_id, approval_id);
            CREATE INDEX IF NOT EXISTS idx_native_integration_transactions_recovery
                ON native_integration_transactions(repository_id, phase, updated_at, transaction_id);
            CREATE INDEX IF NOT EXISTS idx_native_integration_quarantines_active
                ON native_integration_repository_quarantines(repository_id, transaction_id)
                WHERE active = 1;

            CREATE TRIGGER IF NOT EXISTS native_integration_previews_immutable_update
            BEFORE UPDATE ON native_integration_previews
            BEGIN
                SELECT RAISE(ABORT, 'native integration previews are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS native_integration_previews_immutable_delete
            BEFORE DELETE ON native_integration_previews
            BEGIN
                SELECT RAISE(ABORT, 'native integration previews are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS native_integration_approvals_immutable_update
            BEFORE UPDATE ON native_integration_approvals
            BEGIN
                SELECT RAISE(ABORT, 'native integration approvals are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS native_integration_approvals_immutable_delete
            BEFORE DELETE ON native_integration_approvals
            BEGIN
                SELECT RAISE(ABORT, 'native integration approvals are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS native_integration_transactions_identity_immutable
            BEFORE UPDATE ON native_integration_transactions
            WHEN OLD.transaction_id != NEW.transaction_id
              OR OLD.approval_id != NEW.approval_id
              OR OLD.preview_id != NEW.preview_id
              OR OLD.preview_digest != NEW.preview_digest
              OR OLD.repository_id != NEW.repository_id
              OR OLD.destination_ref != NEW.destination_ref
              OR OLD.expected_destination_tip != NEW.expected_destination_tip
              OR OLD.approval_json != NEW.approval_json
            BEGIN
                SELECT RAISE(ABORT, 'native integration transaction identity is immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS native_integration_transactions_immutable_delete
            BEFORE DELETE ON native_integration_transactions
            BEGIN
                SELECT RAISE(ABORT, 'native integration transactions are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS native_integration_receipts_immutable_update
            BEFORE UPDATE ON native_integration_receipts
            BEGIN
                SELECT RAISE(ABORT, 'native integration receipts are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS native_integration_receipts_immutable_delete
            BEFORE DELETE ON native_integration_receipts
            BEGIN
                SELECT RAISE(ABORT, 'native integration receipts are immutable');
            END;
            CREATE TRIGGER IF NOT EXISTS native_integration_quarantines_immutable_delete
            BEFORE DELETE ON native_integration_repository_quarantines
            BEGIN
                SELECT RAISE(ABORT, 'native integration repository quarantines are retained');
            END;
            CREATE TRIGGER IF NOT EXISTS native_integration_quarantines_identity_immutable
            BEFORE UPDATE ON native_integration_repository_quarantines
            WHEN OLD.repository_id != NEW.repository_id
              OR OLD.transaction_id != NEW.transaction_id
              OR OLD.created_at != NEW.created_at
            BEGIN
                SELECT RAISE(ABORT, 'native integration repository quarantine identity is immutable');
            END;",
        )
        .await
        .map_err(|error| {
            global_db_operation_error("initialize native integration schema", error)
        })
}
