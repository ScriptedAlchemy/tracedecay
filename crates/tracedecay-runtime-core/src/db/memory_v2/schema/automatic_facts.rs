//! Automatic fact-receipt integrity triggers and current-projection indexes.

use crate::errors::Result;

use super::super::{MemoryV2Executor, db_error};

pub(super) const CURRENT_PROJECTION_INDEXES_SCHEMA: &str =
    "CREATE INDEX IF NOT EXISTS idx_memory_v2_current_compatibility_search
         ON memory_v2_current_facts(
             owner_kind, project_id, updated_at DESC, fact_id
         );
     CREATE INDEX IF NOT EXISTS idx_memory_v2_current_projection_state
         ON memory_v2_current_facts(owner_kind, project_id, projection_state);";

pub(super) const AUTOMATIC_FACT_RECEIPT_INTEGRITY_SCHEMA: &str =
    "CREATE TRIGGER IF NOT EXISTS memory_v2_automatic_fact_receipts_require_keys
     BEFORE INSERT ON memory_v2_automatic_fact_receipts
     WHEN NEW.idempotency_key IS NULL OR length(NEW.idempotency_key) = 0
       OR NEW.request_digest IS NULL OR length(NEW.request_digest) = 0
     BEGIN
         SELECT RAISE(ABORT, 'memory_v2 automatic fact receipts require idempotency and request digests');
     END;";

/// Installs the current-projection list/search indexes over the baseline
/// `memory_v2_current_facts` shape.
pub(super) async fn install_current_projection_indexes(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch(CURRENT_PROJECTION_INDEXES_SCHEMA)
        .await
        .map_err(|error| db_error(operation, error))
}

/// Installs the automatic fact receipt integrity triggers. `NOT NULL` alone admits empty
/// strings, so the trigger keeps idempotency and request digests non-empty.
pub(super) async fn install_automatic_fact_receipt_integrity_triggers(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    conn.execute_batch(AUTOMATIC_FACT_RECEIPT_INTEGRITY_SCHEMA)
        .await
        .map_err(|error| db_error(operation, error))
}
