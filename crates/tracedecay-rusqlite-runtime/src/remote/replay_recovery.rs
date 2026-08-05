use super::*;

/// Durable evidence that startup recovered replay attempts interrupted before
/// their spool transition completed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteReplayStartupRecoveryV1 {
    pub interrupted_attempts: u64,
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
        let result = self.handle.execute(ExactSqlStatement::new(
            "UPDATE remote_spool_frames
             SET attempt_started_at = NULL
             WHERE attempt_started_at IS NOT NULL"
                .to_owned(),
            Vec::new(),
        )?)?;
        Ok(RemoteReplayStartupRecoveryV1 {
            interrupted_attempts: u64::try_from(result.changed_rows)
                .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?,
            recovered_at,
        })
    }
}
