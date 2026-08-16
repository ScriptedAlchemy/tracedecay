use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteStorageStatusSnapshotV1 {
    pub pending_spool_items: u64,
    pub quarantined_spool_items: u64,
    pub authority: CurrentRemoteAuthorityStateV1,
}

impl RemoteSqliteStorageV1 {
    pub fn status(
        &self,
        brain_id: &BrainId,
    ) -> Result<RemoteStorageStatusSnapshotV1, RemoteSqliteStorageErrorV1> {
        let rows = query(
            self.handle(),
            "SELECT
                SUM(CASE WHEN state = 'pending' THEN 1 ELSE 0 END),
                SUM(CASE WHEN state = 'quarantined' THEN 1 ELSE 0 END)
             FROM remote_spool_frames",
            Vec::new(),
        )?;
        let row = one_row(rows)?;
        let pending_spool_items = count(&row, 0)?;
        let quarantined_spool_items = count(&row, 1)?;
        Ok(RemoteStorageStatusSnapshotV1 {
            pending_spool_items,
            quarantined_spool_items,
            authority: load_authority_state(self.handle(), brain_id)?,
        })
    }
}

fn count(
    row: &crate::exact_sql::ExactSqlRow,
    index: usize,
) -> Result<u64, RemoteSqliteStorageErrorV1> {
    match row.values.get(index) {
        Some(ExactSqlValue::Integer(value)) => {
            u64::try_from(*value).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
        }
        Some(ExactSqlValue::Null) => Ok(0),
        _ => Err(RemoteSqliteStorageErrorV1::Corruption),
    }
}
