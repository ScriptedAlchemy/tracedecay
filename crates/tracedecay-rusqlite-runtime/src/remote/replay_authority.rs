use tracedecay_application::remote::{
    capture::RemoteCapturePersistenceErrorV1,
    query::RemoteExactObservationQueryErrorV1,
    replay::{RemoteReplayCurrentWriterPortV1, RemoteReplayCurrentWriterV1, RemoteReplayFrameV1},
};
use tracedecay_domain::{CurrentRemoteAuthorityStateV1, RemoteRepositoryScopeV1, UtcMicros};

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteQueryAuthoritySnapshotV1 {
    pub authority: CurrentRemoteAuthorityStateV1,
    pub writer: RemoteWriterAuthorityV1,
}

impl RemoteSqliteStorageV1 {
    /// Loads the current query authority from this exact registered RemoteNode
    /// store. The returned snapshot contains no locator or storage handle.
    pub fn query_authority_snapshot(
        &self,
        scope: &RemoteRepositoryScopeV1,
        observed_at: UtcMicros,
    ) -> Result<RemoteQueryAuthoritySnapshotV1, RemoteExactObservationQueryErrorV1> {
        scope
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::ScopeMismatch)?;
        let rows = query(
            self.handle(),
            "SELECT authority_state_json, writer_json, runtime_binding_json, updated_at
             FROM remote_authorities WHERE brain_id = ?1",
            vec![text(self.binding.shard_id.brain_id.as_str())],
        )
        .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?;
        let row =
            one_row(rows).map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?;
        let authority: CurrentRemoteAuthorityStateV1 = serde_json::from_str(
            row_text(&row, 0).map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?,
        )
        .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        let writer: RemoteWriterAuthorityV1 = serde_json::from_str(
            row_text(&row, 1).map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?,
        )
        .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        let binding: StoreRuntimeBindingV1 = serde_json::from_str(
            row_text(&row, 2).map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?,
        )
        .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        let updated_at = match row.values.get(3) {
            Some(ExactSqlValue::Integer(value)) => UtcMicros(*value),
            _ => return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch),
        };
        authority
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        writer
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        if binding != self.binding
            || updated_at > observed_at
            || writer.scope != *scope
            || writer.project_id != scope.project_id
        {
            return Err(RemoteExactObservationQueryErrorV1::ScopeMismatch);
        }
        if let CurrentRemoteAuthorityStateV1::Available(current) = &authority
            && current.fence != writer.authority.fence
        {
            return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
        }
        if promotion_pending(self.handle(), &writer.authority.fence)
            .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?
        {
            return Err(RemoteExactObservationQueryErrorV1::AuthorityUnavailable);
        }
        Ok(RemoteQueryAuthoritySnapshotV1 { authority, writer })
    }
}

impl RemoteReplayCurrentWriterPortV1 for RemoteSqliteStorageV1 {
    fn current_writer(
        &self,
        frame: &RemoteReplayFrameV1,
    ) -> Result<RemoteReplayCurrentWriterV1, RemoteCapturePersistenceErrorV1> {
        if promotion_pending(self.handle(), &frame.capture.writer.authority.fence)
            .map_err(map_persistence_error)?
        {
            return Err(RemoteCapturePersistenceErrorV1::Unavailable);
        }
        let rows = query(
            self.handle(),
            "SELECT authority_state_json, writer_json, runtime_binding_json
             FROM remote_authorities WHERE brain_id = ?1",
            vec![text(frame.capture.writer.authority.fence.brain_id.as_str())],
        )
        .map_err(map_persistence_error)?;
        let row = persistence_one_row(rows)?;
        let state: CurrentRemoteAuthorityStateV1 = serde_json::from_str(row_text(&row, 0)?)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let writer: RemoteWriterAuthorityV1 = serde_json::from_str(row_text(&row, 1)?)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        let binding: StoreRuntimeBindingV1 = serde_json::from_str(row_text(&row, 2)?)
            .map_err(|_| RemoteCapturePersistenceErrorV1::Corruption)?;
        if binding != self.binding {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        match &state {
            CurrentRemoteAuthorityStateV1::Available(authority)
                if authority.fence == writer.authority.fence =>
            {
                Ok(RemoteReplayCurrentWriterV1 {
                    writer: Some(writer),
                    state,
                })
            }
            CurrentRemoteAuthorityStateV1::Available(_) => {
                Err(RemoteCapturePersistenceErrorV1::Corruption)
            }
            CurrentRemoteAuthorityStateV1::Partial { .. }
            | CurrentRemoteAuthorityStateV1::Unavailable { .. } => {
                Ok(RemoteReplayCurrentWriterV1 {
                    writer: None,
                    state,
                })
            }
        }
    }
}
