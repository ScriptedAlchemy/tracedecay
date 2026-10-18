//! Reachability of a joined store's owning authority, as the node last
//! observed it.

use tracedecay_domain::RemoteAuthorityUnavailableReasonV1;

use super::*;

impl RemoteSqliteStorageV1 {
    /// Records that the owning authority of this joined store did not answer
    /// at `observed_at`. The writer it last published stays stored, so offline
    /// capture is admitted only under that writer's fence, and the next
    /// successful discovery records the authority as available again. A store
    /// this daemon serves as the authority is never unreachable from itself.
    pub fn record_authority_unreachable(
        &self,
        observed_at: UtcMicros,
    ) -> Result<(), RemoteSqliteStorageErrorV1> {
        if !self.is_joined() {
            return Err(RemoteSqliteStorageErrorV1::BindingMismatch);
        }
        let state = CurrentRemoteAuthorityStateV1::Unavailable {
            reason: RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
            observed_at,
        };
        let state_json =
            serde_json::to_string(&state).map_err(|_| RemoteSqliteStorageErrorV1::Corruption)?;
        let updated = self.handle().execute(ExactSqlStatement::new(
            "UPDATE remote_authorities SET authority_state_json = ?1, updated_at = ?2
             WHERE brain_id = ?3"
                .to_owned(),
            vec![
                text(&state_json),
                ExactSqlValue::Integer(observed_at.0),
                text(self.member_brain.as_str()),
            ],
        )?)?;
        if updated.changed_rows == 0 {
            return Err(RemoteSqliteStorageErrorV1::WriterAuthorityUnpublished);
        }
        Ok(())
    }
}
