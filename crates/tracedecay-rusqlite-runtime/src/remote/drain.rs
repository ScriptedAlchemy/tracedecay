//! Sender half of the ordered drain: a joined node hands its pending frames to
//! the owning authority and releases each one only after the authority's
//! durable receipt.

use tracedecay_contracts::remote::transfer::RemoteFrameTransferReceiptV1;

use super::*;

impl RemoteSqliteStorageV1 {
    /// Event ids of this joined node's frames still awaiting the authority's
    /// receipt, in capture sequence order.
    pub fn pending_drain_frames(
        &self,
        enrollment_id: &EntityId,
    ) -> Result<Vec<String>, RemoteSqliteStorageErrorV1> {
        if !self.is_joined() {
            return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
        }
        query(
            self.handle(),
            "SELECT event_id FROM remote_spool_frames
             WHERE enrollment_id = ?1 AND state = 'pending'
             ORDER BY sequence",
            vec![text(enrollment_id.as_str())],
        )?
        .rows
        .iter()
        .map(|row| {
            row_text(row, 0)
                .map(str::to_owned)
                .map_err(RemoteSqliteStorageErrorV1::from)
        })
        .collect()
    }

    /// Records the authority's durable receipt for one drained frame, then
    /// releases the frame for garbage collection in the same commit. A frame
    /// that is not pending under exactly that receipt's identity is a
    /// `Conflict`.
    pub fn acknowledge_drained_frame(
        &self,
        receipt: &RemoteFrameTransferReceiptV1,
    ) -> Result<(), RemoteSqliteStorageErrorV1> {
        if !self.is_joined() {
            return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
        }
        let sequence =
            i64::try_from(receipt.sequence).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let transaction = self.handle().begin_immediate()?;
        let acknowledged = transaction.execute(ExactSqlStatement::new(
            "UPDATE remote_spool_frames SET state = 'acknowledged'
             WHERE event_id = ?1 AND sequence = ?2 AND state = 'pending'"
                .to_owned(),
            vec![text(&receipt.event_id), ExactSqlValue::Integer(sequence)],
        )?)?;
        if acknowledged.changed_rows != 1 {
            transaction.rollback()?;
            return Err(RemoteSqliteStorageErrorV1::Conflict);
        }
        transaction.execute(ExactSqlStatement::new(
            "UPDATE remote_spool_frames SET state = 'garbage_collection_eligible'
             WHERE event_id = ?1 AND state = 'acknowledged'"
                .to_owned(),
            vec![text(&receipt.event_id)],
        )?)?;
        transaction.commit()?;
        Ok(())
    }
}
