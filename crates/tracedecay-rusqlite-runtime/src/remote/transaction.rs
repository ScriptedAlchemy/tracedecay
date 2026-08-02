use tracedecay_application::remote::replay::{
    RemoteReplayCommitReceiptV1, RemoteReplayTransactionErrorV1, RemoteReplayTransactionOutcomeV1,
    RemoteReplayTransactionPortV1,
};

use super::*;

impl RemoteReplayTransactionPortV1 for RemoteSqliteStorageV1 {
    fn commit(
        &self,
        frame: &RemoteReplayFrameV1,
        current_writer: &RemoteWriterAuthorityV1,
        committed_at: UtcMicros,
    ) -> Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1> {
        frame
            .validate()
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        current_writer
            .validate()
            .map_err(|_| RemoteReplayTransactionErrorV1::FenceMismatch)?;
        if committed_at < frame.capture.captured_at {
            return Err(RemoteReplayTransactionErrorV1::CanonicalEffect);
        }
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
        validate_transaction_authority(&transaction, &self.binding, current_writer)?;
        let existing = transaction
            .query(
                MigrationSqlStatement::new(
                    "SELECT event_id, replay_receipt_json, enrollment_id, node_id,
                            capture_sequence, previous_event_id
                     FROM remote_observations_v1
                     WHERE observation_id = ?1 OR event_id = ?2"
                        .to_owned(),
                    vec![
                        text(frame.capture.observation.observation_id().as_str()),
                        text(&frame.event_id),
                    ],
                )
                .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?,
            )
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
        if !existing.rows.is_empty() {
            if existing.rows.len() != 1 {
                return Err(RemoteReplayTransactionErrorV1::IdempotencyConflict);
            }
            let row = &existing.rows[0];
            if transaction_row_text(row, 0)? != frame.event_id
                || transaction_row_text(row, 2)? != frame.capture.enrollment_id.as_str()
                || transaction_row_text(row, 3)? != frame.capture.node_id.as_str()
                || transaction_row_u64(row, 4)? != frame.capture.sequence.sequence
                || transaction_row_optional_text(row, 5)?
                    != frame.capture.sequence.previous_event_id.as_deref()
            {
                return Err(RemoteReplayTransactionErrorV1::IdempotencyConflict);
            }
            let receipt: RemoteReplayCommitReceiptV1 =
                serde_json::from_str(transaction_row_text(row, 1)?)
                    .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
            receipt
                .validate_for(frame, current_writer)
                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
            transaction
                .commit()
                .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
            return Ok(RemoteReplayTransactionOutcomeV1::Duplicate(receipt));
        }
        validate_capture_predecessor(&transaction, frame)?;
        let sequence_rows = transaction
            .query(
                MigrationSqlStatement::new(
                    "SELECT COALESCE(MAX(sequence), 0) FROM remote_observations_v1".to_owned(),
                    Vec::new(),
                )
                .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?,
            )
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
        let sequence_row = sequence_rows
            .rows
            .first()
            .ok_or(RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let last_sequence = match sequence_row.values.first() {
            Some(MigrationSqlValue::Integer(value)) => u64::try_from(*value)
                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
            _ => return Err(RemoteReplayTransactionErrorV1::CanonicalEffect),
        };
        let commit_sequence = last_sequence
            .checked_add(1)
            .ok_or(RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let observation_json = serde_json::to_string(&frame.capture.observation)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let binding_json = serde_json::to_string(&self.binding)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let writer_fence_json = serde_json::to_string(&current_writer.authority.fence)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let receipt = RemoteReplayCommitReceiptV1 {
            event_id: frame.event_id.clone(),
            writer_fence: current_writer.authority.fence.clone(),
            commit_sequence,
            committed_at,
            budget: OperationBudgetUsage {
                units_consumed: 1,
                bytes_consumed: u64::try_from(observation_json.len())
                    .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
                elapsed_micros: 0,
            },
        };
        receipt
            .validate_for(frame, current_writer)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let receipt_json = serde_json::to_string(&receipt)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        transaction
            .execute(
                MigrationSqlStatement::new(
                    "INSERT INTO remote_observations_v1 (
                        observation_id, event_id, enrollment_id, node_id,
                        capture_sequence, previous_event_id, sequence, observation_json,
                        runtime_binding_json, writer_fence_json, replay_receipt_json, committed_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
                        .to_owned(),
                    vec![
                        text(frame.capture.observation.observation_id().as_str()),
                        text(&frame.event_id),
                        text(frame.capture.enrollment_id.as_str()),
                        text(frame.capture.node_id.as_str()),
                        MigrationSqlValue::Integer(
                            i64::try_from(frame.capture.sequence.sequence)
                                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
                        ),
                        optional_text(frame.capture.sequence.previous_event_id.as_deref()),
                        MigrationSqlValue::Integer(
                            i64::try_from(commit_sequence)
                                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
                        ),
                        text(&observation_json),
                        text(&binding_json),
                        text(&writer_fence_json),
                        text(&receipt_json),
                        MigrationSqlValue::Integer(committed_at.0),
                    ],
                )
                .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?,
            )
            .map_err(|_| RemoteReplayTransactionErrorV1::IdempotencyConflict)?;
        transaction
            .commit()
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
        Ok(RemoteReplayTransactionOutcomeV1::Admitted(receipt))
    }
}

fn validate_capture_predecessor(
    transaction: &crate::migration_sql::MigrationSqlTransaction,
    frame: &RemoteReplayFrameV1,
) -> Result<(), RemoteReplayTransactionErrorV1> {
    if frame.capture.sequence.sequence == 1 {
        return Ok(());
    }
    let predecessor_sequence = frame
        .capture
        .sequence
        .sequence
        .checked_sub(1)
        .ok_or(RemoteReplayTransactionErrorV1::SequenceGap)?;
    let rows = transaction
        .query(
            MigrationSqlStatement::new(
                "SELECT event_id FROM remote_observations_v1
                 WHERE enrollment_id = ?1 AND capture_sequence = ?2"
                    .to_owned(),
                vec![
                    text(frame.capture.enrollment_id.as_str()),
                    MigrationSqlValue::Integer(
                        i64::try_from(predecessor_sequence)
                            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
                    ),
                ],
            )
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?,
        )
        .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
    let expected = frame
        .capture
        .sequence
        .previous_event_id
        .as_deref()
        .ok_or(RemoteReplayTransactionErrorV1::SequenceGap)?;
    match rows.rows.as_slice() {
        [row] if transaction_row_text(row, 0)? == expected => Ok(()),
        _ => Err(RemoteReplayTransactionErrorV1::SequenceGap),
    }
}

fn validate_transaction_authority(
    transaction: &crate::migration_sql::MigrationSqlTransaction,
    binding: &StoreRuntimeBindingV1,
    current_writer: &RemoteWriterAuthorityV1,
) -> Result<(), RemoteReplayTransactionErrorV1> {
    let rows = transaction
        .query(
            MigrationSqlStatement::new(
                "SELECT runtime_binding_json, writer_json
                 FROM remote_authorities_v1 WHERE brain_id = ?1"
                    .to_owned(),
                vec![text(current_writer.authority.fence.brain_id.as_str())],
            )
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?,
        )
        .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
    if rows.rows.len() != 1 {
        return Err(RemoteReplayTransactionErrorV1::FenceMismatch);
    }
    let row = &rows.rows[0];
    let stored_binding: StoreRuntimeBindingV1 = serde_json::from_str(transaction_row_text(row, 0)?)
        .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    let stored_writer: RemoteWriterAuthorityV1 =
        serde_json::from_str(transaction_row_text(row, 1)?)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    if &stored_binding != binding || &stored_writer != current_writer {
        return Err(RemoteReplayTransactionErrorV1::FenceMismatch);
    }
    Ok(())
}

fn transaction_row_text(
    row: &crate::migration_sql::MigrationSqlRow,
    index: usize,
) -> Result<&str, RemoteReplayTransactionErrorV1> {
    match row.values.get(index) {
        Some(MigrationSqlValue::Text(value)) => Ok(value),
        _ => Err(RemoteReplayTransactionErrorV1::CanonicalEffect),
    }
}

fn transaction_row_u64(
    row: &crate::migration_sql::MigrationSqlRow,
    index: usize,
) -> Result<u64, RemoteReplayTransactionErrorV1> {
    match row.values.get(index) {
        Some(MigrationSqlValue::Integer(value)) => {
            u64::try_from(*value).map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)
        }
        _ => Err(RemoteReplayTransactionErrorV1::CanonicalEffect),
    }
}

fn transaction_row_optional_text(
    row: &crate::migration_sql::MigrationSqlRow,
    index: usize,
) -> Result<Option<&str>, RemoteReplayTransactionErrorV1> {
    match row.values.get(index) {
        Some(MigrationSqlValue::Text(value)) => Ok(Some(value)),
        Some(MigrationSqlValue::Null) => Ok(None),
        _ => Err(RemoteReplayTransactionErrorV1::CanonicalEffect),
    }
}
