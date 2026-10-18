use super::{LedgerError, sqlite::LedgerTransaction};

pub const RUNTIME_LEDGER_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS td_runtime_writer_checkpoint_v1 (
    shard_json TEXT NOT NULL,
    incarnation INTEGER NOT NULL CHECK (incarnation > 0),
    authority_epoch INTEGER NOT NULL CHECK (authority_epoch > 0),
    commit_sequence INTEGER NOT NULL CHECK (commit_sequence > 0),
    watermark_json TEXT NOT NULL,
    transaction_scope_json TEXT NOT NULL,
    original_receipt_json TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    durability_json TEXT NOT NULL,
    committed_at_micros INTEGER NOT NULL,
    PRIMARY KEY (shard_json, incarnation)
) WITHOUT ROWID;

-- The receipt and transaction scope are rebuilt from these columns and the
-- key: every other field they carry is the row's binding. A row of about
-- 0.5 KB fits an index b-tree leaf, so the key is the table and no separate
-- unique index repeats it.
CREATE TABLE IF NOT EXISTS td_runtime_writer_idempotency_v2 (
    shard_json TEXT NOT NULL,
    incarnation INTEGER NOT NULL CHECK (incarnation > 0),
    authority_epoch INTEGER NOT NULL CHECK (authority_epoch > 0),
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    transaction_id TEXT NOT NULL,
    commit_sequence INTEGER NOT NULL CHECK (commit_sequence > 0),
    durability_json TEXT NOT NULL,
    priority_json TEXT NOT NULL,
    opened_at_micros INTEGER NOT NULL,
    committed_at_micros INTEGER NOT NULL,
    PRIMARY KEY (shard_json, incarnation, authority_epoch, idempotency_key)
) WITHOUT ROWID;
"#;

/// Drops the transactional outbox and inbox tables, which no production code
/// ever wrote. Registered stores run it on every schema install, so a store
/// created before the retirement converges to the same shape as a fresh one.
pub const RETIRE_RUNTIME_EFFECTS_V1: &str = "
DROP TABLE IF EXISTS td_runtime_writer_outbox_v1;
DROP TABLE IF EXISTS td_runtime_writer_inbox_v1;
";

/// Installs the runtime ledger tables.
///
/// Store creation owns the durable schema. Standalone runtime requests may
/// install it in their first transaction; writer startup must not mutate the
/// store before its owning authority validates admission. Cache a successful
/// install only while its transaction has not rolled back.
pub(crate) fn initialize_schema(transaction: &impl LedgerTransaction) -> Result<(), LedgerError> {
    transaction
        .execute_batch(RUNTIME_LEDGER_SCHEMA)
        .map_err(Into::into)
}
