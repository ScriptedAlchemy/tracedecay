use tracedecay_application::remote::capture::RemoteCapturePersistenceErrorV1;

use crate::exact_sql::ExactSqlTransaction;

use super::{RemoteSpoolLimitsV1, persistence_one_row, row_u64, statement};

pub(super) fn enforce(
    transaction: &ExactSqlTransaction,
    limits: RemoteSpoolLimitsV1,
    new_ciphertext_bytes: usize,
) -> Result<(), RemoteCapturePersistenceErrorV1> {
    let usage = transaction
        .query(statement(
            "SELECT COUNT(*), COALESCE(SUM(length(ciphertext)), 0)
             FROM remote_spool_frames
             WHERE state != 'garbage_collection_eligible'",
            Vec::new(),
        )?)
        .map_err(super::map_persistence_error)?;
    let usage = persistence_one_row(usage)?;
    let event_count = row_u64(&usage, 0)?;
    let ciphertext_bytes = row_u64(&usage, 1)?;
    let new_ciphertext_bytes = u64::try_from(new_ciphertext_bytes)
        .map_err(|_| RemoteCapturePersistenceErrorV1::Overflow)?;
    if event_count >= limits.maximum_events
        || ciphertext_bytes
            .checked_add(new_ciphertext_bytes)
            .is_none_or(|bytes| bytes > limits.maximum_ciphertext_bytes)
    {
        return Err(RemoteCapturePersistenceErrorV1::Overflow);
    }
    Ok(())
}
