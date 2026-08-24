use super::{LedgerError, sqlite::LedgerTransaction};

const LEDGER_SCHEMA: &str = r#"
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

CREATE TABLE IF NOT EXISTS td_runtime_writer_idempotency_v1 (
    shard_json TEXT NOT NULL,
    incarnation INTEGER NOT NULL CHECK (incarnation > 0),
    authority_epoch INTEGER NOT NULL CHECK (authority_epoch > 0),
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    original_receipt_json TEXT NOT NULL,
    transaction_scope_json TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    durability_json TEXT NOT NULL,
    committed_at_micros INTEGER NOT NULL,
    PRIMARY KEY (shard_json, incarnation, authority_epoch, idempotency_key)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS td_runtime_writer_outbox_v1 (
    source_shard_json TEXT NOT NULL,
    source_incarnation INTEGER NOT NULL CHECK (source_incarnation > 0),
    source_authority_epoch INTEGER NOT NULL CHECK (source_authority_epoch > 0),
    effect_id TEXT NOT NULL,
    ordering_key TEXT NOT NULL,
    source_sequence INTEGER NOT NULL CHECK (source_sequence >= 0),
    state TEXT NOT NULL CHECK (
        state IN ('pending', 'dispatched', 'effect_unknown', 'acknowledged')
    ),
    entry_json TEXT NOT NULL,
    source_receipt_json TEXT NOT NULL,
    transaction_scope_json TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    durability_json TEXT NOT NULL,
    updated_at_micros INTEGER NOT NULL,
    PRIMARY KEY (source_shard_json, source_incarnation, source_authority_epoch, effect_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS td_runtime_writer_outbox_ordering_v1
ON td_runtime_writer_outbox_v1 (
    source_shard_json,
    source_incarnation,
    source_authority_epoch,
    ordering_key,
    source_sequence,
    effect_id
);

CREATE UNIQUE INDEX IF NOT EXISTS td_runtime_writer_outbox_effect_v1
ON td_runtime_writer_outbox_v1 (source_shard_json, effect_id);

CREATE INDEX IF NOT EXISTS td_runtime_writer_outbox_state_v1
ON td_runtime_writer_outbox_v1 (
    source_shard_json,
    source_incarnation,
    source_authority_epoch,
    state,
    updated_at_micros
);

CREATE TABLE IF NOT EXISTS td_runtime_writer_inbox_v1 (
    target_shard_json TEXT NOT NULL,
    target_incarnation INTEGER NOT NULL CHECK (target_incarnation > 0),
    target_authority_epoch INTEGER NOT NULL CHECK (target_authority_epoch > 0),
    effect_id TEXT NOT NULL,
    ordering_key TEXT NOT NULL,
    source_sequence INTEGER NOT NULL CHECK (source_sequence >= 0),
    target_sequence INTEGER NOT NULL CHECK (target_sequence > 0),
    identity_json TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    committed_at_micros INTEGER NOT NULL,
    PRIMARY KEY (target_shard_json, target_incarnation, target_authority_epoch, effect_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS td_runtime_writer_inbox_ordering_v1
ON td_runtime_writer_inbox_v1 (
    target_shard_json,
    target_incarnation,
    target_authority_epoch,
    ordering_key,
    source_sequence,
    effect_id
);

CREATE UNIQUE INDEX IF NOT EXISTS td_runtime_writer_inbox_effect_v1
ON td_runtime_writer_inbox_v1 (target_shard_json, effect_id);
"#;

pub(crate) fn initialize_schema(transaction: &impl LedgerTransaction) -> Result<(), LedgerError> {
    transaction.execute_batch(LEDGER_SCHEMA)?;
    Ok(())
}
