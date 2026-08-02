use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteStorageStatusSnapshotV1 {
    pub pending_spool_items: u64,
    pub quarantined_spool_items: u64,
    pub authority: CurrentRemoteAuthorityStateV1,
    pub recovery_in_progress: bool,
    pub recovery_required: bool,
}

impl RemoteSqliteStorageV1 {
    pub fn status(
        &self,
        brain_id: &BrainId,
    ) -> Result<RemoteStorageStatusSnapshotV1, RemoteSqliteStorageErrorV1> {
        let rows = query(
            &self.handle,
            "SELECT
                SUM(CASE WHEN state = 'pending' THEN 1 ELSE 0 END),
                SUM(CASE WHEN state = 'quarantined' THEN 1 ELSE 0 END)
             FROM remote_spool_frames_v1",
            Vec::new(),
        )?;
        let row = one_row(rows)?;
        let pending_spool_items = count(&row, 0)?;
        let quarantined_spool_items = count(&row, 1)?;
        let recovery = query(
            &self.handle,
            "SELECT state FROM remote_recovery_journal_v1
             WHERE state NOT IN ('available', 'published', 'rolled_back_before_publication')",
            Vec::new(),
        )?;
        let recovery_required = recovery.rows.iter().any(|row| {
            matches!(
                row.values.first(),
                Some(MigrationSqlValue::Text(state))
                    if state == "failed"
                        || state == "recovery_required"
                        || state == "forward_recovery_required"
            )
        });
        Ok(RemoteStorageStatusSnapshotV1 {
            pending_spool_items,
            quarantined_spool_items,
            authority: load_authority_state(&self.handle, brain_id)?,
            recovery_in_progress: !recovery.rows.is_empty() && !recovery_required,
            recovery_required,
        })
    }
}

fn count(
    row: &crate::migration_sql::MigrationSqlRow,
    index: usize,
) -> Result<u64, RemoteSqliteStorageErrorV1> {
    match row.values.get(index) {
        Some(MigrationSqlValue::Integer(value)) => {
            u64::try_from(*value).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
        }
        Some(MigrationSqlValue::Null) => Ok(0),
        _ => Err(RemoteSqliteStorageErrorV1::Corruption),
    }
}
