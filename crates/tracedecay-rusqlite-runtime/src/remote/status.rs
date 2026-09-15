use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteStorageStatusSnapshotV1 {
    pub pending_spool_items: u64,
    pub quarantined_spool_items: u64,
    /// True when any enrollment's retained spool frames are no longer
    /// sequence-contiguous — a truthful recoverable state, never silently
    /// collapsed into an empty spool.
    pub has_sequence_gap: bool,
    pub authority: CurrentRemoteAuthorityStateV1,
}

impl RemoteSqliteStorageV1 {
    pub fn status(
        &self,
        brain_id: &BrainId,
    ) -> Result<RemoteStorageStatusSnapshotV1, RemoteSqliteStorageErrorV1> {
        self.status_at(brain_id, UtcMicros(0))
    }

    /// Status read that reports a never-published authority as the typed
    /// `Unavailable { PlacementUnknown }` state observed at `observed_at`,
    /// instead of a storage error.
    pub fn status_at(
        &self,
        brain_id: &BrainId,
        observed_at: UtcMicros,
    ) -> Result<RemoteStorageStatusSnapshotV1, RemoteSqliteStorageErrorV1> {
        let rows = query(
            self.handle(),
            "SELECT
                SUM(CASE WHEN state = 'pending' THEN 1 ELSE 0 END),
                SUM(CASE WHEN state = 'quarantined' THEN 1 ELSE 0 END),
                EXISTS(
                    SELECT 1 FROM remote_spool_frames
                    GROUP BY enrollment_id
                    HAVING COUNT(*) != MAX(sequence) - MIN(sequence) + 1
                )
             FROM remote_spool_frames",
            Vec::new(),
        )?;
        let row = one_row(rows)?;
        let pending_spool_items = count(&row, 0)?;
        let quarantined_spool_items = count(&row, 1)?;
        let has_sequence_gap = count(&row, 2)? != 0;
        let authority = match load_optional_authority_state(self.handle(), brain_id)? {
            Some(authority) => authority,
            None => CurrentRemoteAuthorityStateV1::Unavailable {
                reason: tracedecay_domain::RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
                observed_at,
            },
        };
        Ok(RemoteStorageStatusSnapshotV1 {
            pending_spool_items,
            quarantined_spool_items,
            has_sequence_gap,
            authority,
        })
    }
}

/// Loads the published authority state, treating an absent registry row as a
/// typed `None` rather than a storage error.
fn load_optional_authority_state(
    handle: &crate::exact_sql::ExactSqlHandle,
    brain_id: &BrainId,
) -> Result<Option<CurrentRemoteAuthorityStateV1>, RemoteSqliteStorageErrorV1> {
    let rows = query(
        handle,
        "SELECT EXISTS(SELECT 1 FROM remote_authorities WHERE brain_id = ?1)",
        vec![text(brain_id.as_str())],
    )?;
    if count(&one_row(rows)?, 0)? == 0 {
        return Ok(None);
    }
    load_authority_state(handle, brain_id).map(Some)
}

/// Read-only summary of the durable recovery journal for one node store:
/// whether the most recent backup completed verification, whether a promotion
/// is currently executing, and whether any recovery operation requires
/// forward recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteRecoveryOperationalSnapshotV1 {
    pub current_backup_verified: bool,
    pub failover_in_progress: bool,
    pub recovery_required: bool,
}

impl RemoteSqliteStorageV1 {
    pub fn recovery_operational_snapshot(
        &self,
    ) -> Result<RemoteRecoveryOperationalSnapshotV1, RemoteSqliteStorageErrorV1> {
        let rows = query(
            self.handle(),
            "SELECT
                (SELECT state FROM remote_recovery_operations
                 WHERE operation_kind = 'backup'
                 ORDER BY updated_at DESC, operation_id DESC LIMIT 1),
                EXISTS(
                    SELECT 1 FROM remote_recovery_operations
                    WHERE operation_kind = 'promotion' AND state = 'executing'
                ),
                EXISTS(
                    SELECT 1 FROM remote_recovery_operations
                    WHERE state = 'forward_recovery_required'
                )",
            Vec::new(),
        )?;
        let row = one_row(rows)?;
        let current_backup_verified = match row.values.first() {
            Some(ExactSqlValue::Text(state)) => state == "completed",
            Some(ExactSqlValue::Null) => false,
            _ => return Err(RemoteSqliteStorageErrorV1::Corruption),
        };
        Ok(RemoteRecoveryOperationalSnapshotV1 {
            current_backup_verified,
            failover_in_progress: count(&row, 1)? != 0,
            recovery_required: count(&row, 2)? != 0,
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
