use super::*;

/// Durable evidence that startup recovered replay attempts interrupted before
/// their spool transition completed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteReplayStartupRecoveryV1 {
    pub lease_id: String,
    pub interrupted_attempts: u64,
    pub preserved_newer_markers: u64,
    pub recovered_at: UtcMicros,
}

impl RemoteSqliteStorageV1 {
    /// Releases only persisted in-flight markers. Frame state, attempt number,
    /// canonical receipt, and ciphertext remain unchanged so the next replay
    /// must pass the canonical idempotency fence and either obtain the original
    /// receipt or fail closed.
    pub fn recover_interrupted_replay_attempts(
        &self,
        recovered_at: UtcMicros,
    ) -> Result<RemoteReplayStartupRecoveryV1, RemoteSqliteStorageErrorV1> {
        if recovered_at.0 <= 0 {
            return Err(RemoteSqliteStorageErrorV1::Corruption);
        }
        let expires_at = recovered_at
            .0
            .checked_add(30_000_000)
            .ok_or(RemoteSqliteStorageErrorV1::Corruption)?;
        let lease_id = format!(
            "replay.recovery.{}",
            canonical_sha256(&(
                "tracedecay.remote-replay-recovery-lease.v1",
                &self.binding,
                recovered_at,
            ))
            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(RemoteSqliteStorageErrorV1::Corruption)?
        );
        let transaction = self.handle().begin_immediate()?;
        let lease = transaction.execute(ExactSqlStatement::new(
            "INSERT INTO remote_replay_recovery_lease (
                singleton, lease_id, acquired_at, expires_at
             ) VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(singleton) DO UPDATE SET
                lease_id = excluded.lease_id,
                acquired_at = excluded.acquired_at,
                expires_at = excluded.expires_at
             WHERE remote_replay_recovery_lease.expires_at <= excluded.acquired_at
                OR remote_replay_recovery_lease.lease_id = excluded.lease_id"
                .to_owned(),
            vec![
                text(&lease_id),
                ExactSqlValue::Integer(recovered_at.0),
                ExactSqlValue::Integer(expires_at),
            ],
        )?)?;
        if lease.changed_rows != 1 {
            transaction.rollback()?;
            return Err(RemoteSqliteStorageErrorV1::Conflict);
        }
        let markers = transaction.query(ExactSqlStatement::new(
            "SELECT event_id, last_attempt, attempt_started_at
             FROM remote_spool_frames
             WHERE attempt_started_at IS NOT NULL
             ORDER BY event_id"
                .to_owned(),
            Vec::new(),
        )?)?;
        let mut interrupted_attempts = 0_u64;
        let mut preserved_newer_markers = 0_u64;
        for marker in markers.rows {
            let event_id =
                row_text(&marker, 0).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
            let attempt =
                row_u64(&marker, 1).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
            let started_at = match marker.values.get(2) {
                Some(ExactSqlValue::Integer(value)) => *value,
                _ => return Err(RemoteSqliteStorageErrorV1::Corruption),
            };
            let result = transaction.execute(ExactSqlStatement::new(
                "UPDATE remote_spool_frames
                 SET attempt_started_at = NULL
                 WHERE event_id = ?1 AND last_attempt = ?2 AND attempt_started_at = ?3
                   AND EXISTS (
                     SELECT 1 FROM remote_replay_recovery_lease
                     WHERE singleton = 1 AND lease_id = ?4 AND expires_at > ?5
                   )"
                .to_owned(),
                vec![
                    text(event_id),
                    ExactSqlValue::Integer(
                        i64::try_from(attempt)
                            .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?,
                    ),
                    ExactSqlValue::Integer(started_at),
                    text(&lease_id),
                    ExactSqlValue::Integer(recovered_at.0),
                ],
            )?)?;
            match result.changed_rows {
                1 => {
                    interrupted_attempts = interrupted_attempts
                        .checked_add(1)
                        .ok_or(RemoteSqliteStorageErrorV1::Corruption)?;
                }
                0 => {
                    preserved_newer_markers = preserved_newer_markers
                        .checked_add(1)
                        .ok_or(RemoteSqliteStorageErrorV1::Corruption)?;
                }
                _ => return Err(RemoteSqliteStorageErrorV1::Corruption),
            }
        }
        transaction.execute(ExactSqlStatement::new(
            "DELETE FROM remote_replay_recovery_lease
             WHERE singleton = 1 AND lease_id = ?1"
                .to_owned(),
            vec![text(&lease_id)],
        )?)?;
        transaction.commit()?;
        Ok(RemoteReplayStartupRecoveryV1 {
            lease_id,
            interrupted_attempts,
            preserved_newer_markers,
            recovered_at,
        })
    }
}
