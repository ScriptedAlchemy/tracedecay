use tracedecay_application::remote::{
    capture::RemoteCapturePersistenceErrorV1,
    replay::{RemoteReplayCurrentWriterPortV1, RemoteReplayCurrentWriterV1, RemoteReplayFrameV1},
};
use tracedecay_domain::CurrentRemoteAuthorityStateV1;

use super::*;

impl RemoteReplayCurrentWriterPortV1 for RemoteSqliteStorageV1 {
    fn current_writer(
        &self,
        frame: &RemoteReplayFrameV1,
    ) -> Result<RemoteReplayCurrentWriterV1, RemoteCapturePersistenceErrorV1> {
        if promotion_pending(&self.handle, &frame.capture.writer.authority.fence)
            .map_err(map_persistence_error)?
        {
            return Err(RemoteCapturePersistenceErrorV1::Unavailable);
        }
        let rows = query(
            &self.handle,
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
