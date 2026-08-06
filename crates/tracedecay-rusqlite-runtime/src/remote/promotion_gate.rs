use tracedecay_domain::{ManifestDigest, RemoteWriterFenceV1, canonical_sha256};

use crate::exact_sql::{ExactSqlHandle, ExactSqlRows, ExactSqlTransaction, ExactSqlValue};

use super::{RemoteSqliteStorageErrorV1, one_row, query, statement, text};

fn promotion_authority_key(
    writer: &RemoteWriterFenceV1,
) -> Result<ManifestDigest, RemoteSqliteStorageErrorV1> {
    canonical_sha256(&(
        "tracedecay.remote-recovery-authority.v1",
        &writer.brain_id,
        &writer.shard_id,
        &writer.generation_id,
    ))
    .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
}

pub(super) fn promotion_pending(
    handle: &ExactSqlHandle,
    writer: &RemoteWriterFenceV1,
) -> Result<bool, RemoteSqliteStorageErrorV1> {
    let authority_key = promotion_authority_key(writer)?;
    let rows = query(
        handle,
        "SELECT EXISTS(
            SELECT 1 FROM remote_recovery_operations
            WHERE expected_authority_key = ?1 AND operation_kind = 'promotion'
              AND state IN ('executing', 'forward_recovery_required')
         )",
        vec![text(authority_key.as_str())],
    )?;
    pending_value(rows)
}

pub(super) fn promotion_pending_in(
    transaction: &ExactSqlTransaction,
    writer: &RemoteWriterFenceV1,
) -> Result<bool, RemoteSqliteStorageErrorV1> {
    let authority_key = promotion_authority_key(writer)?;
    let rows = transaction.query(statement(
        "SELECT EXISTS(
            SELECT 1 FROM remote_recovery_operations
            WHERE expected_authority_key = ?1 AND operation_kind = 'promotion'
              AND state IN ('executing', 'forward_recovery_required')
         )",
        vec![text(authority_key.as_str())],
    )?)?;
    pending_value(rows)
}

fn pending_value(rows: ExactSqlRows) -> Result<bool, RemoteSqliteStorageErrorV1> {
    let row = one_row(rows)?;
    match row.values.first() {
        Some(ExactSqlValue::Integer(value)) => Ok(*value == 1),
        _ => Err(RemoteSqliteStorageErrorV1::Corruption),
    }
}
