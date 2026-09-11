//! Typed operation capabilities supplied to the Remote Brain HTTP router.

use std::sync::Arc;

use super::{
    capture::RemoteCaptureReceiptV1,
    capture_protocol::RemoteCaptureRequestV1,
    protocol::RemoteProtocolPortV1,
    query::{RemoteQueryRequestV1, RemoteQueryResultV1},
    recovery::{
        BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
        StagedRestoreConfirmationV1, StagedRestoreProgressV1,
    },
    replay::{RemoteReplayOutcomeV1, RemoteReplayRequestV1},
    transfer::{RemoteFrameTransferReceiptV1, RemoteFrameTransferRequestV1},
};

pub type RemoteCaptureProtocolOwnerPortV1 =
    dyn RemoteProtocolPortV1<RemoteCaptureRequestV1, Output = RemoteCaptureReceiptV1> + Send + Sync;
pub type RemoteReplayProtocolOwnerPortV1 =
    dyn RemoteProtocolPortV1<RemoteReplayRequestV1, Output = RemoteReplayOutcomeV1> + Send + Sync;
pub type RemoteFrameTransferProtocolOwnerPortV1 = dyn RemoteProtocolPortV1<RemoteFrameTransferRequestV1, Output = RemoteFrameTransferReceiptV1>
    + Send
    + Sync;
pub type RemoteQueryProtocolOwnerPortV1 =
    dyn RemoteProtocolPortV1<RemoteQueryRequestV1, Output = RemoteQueryResultV1> + Send + Sync;
pub type RemoteBackupProtocolOwnerPortV1 =
    dyn RemoteProtocolPortV1<BackupRequestV1, Output = BackupOperationStateV1> + Send + Sync;
pub type RemoteRestoreProtocolOwnerPortV1 = dyn RemoteProtocolPortV1<StagedRestoreConfirmationV1, Output = StagedRestoreProgressV1>
    + Send
    + Sync;
pub type RemotePromotionProtocolOwnerPortV1 =
    dyn RemoteProtocolPortV1<PromotionConfirmationV1, Output = PromotionCasReceiptV1> + Send + Sync;

pub struct RemoteOperationProtocolPortsV1 {
    pub capture: Arc<RemoteCaptureProtocolOwnerPortV1>,
    pub replay: Arc<RemoteReplayProtocolOwnerPortV1>,
    pub frame_transfer: Arc<RemoteFrameTransferProtocolOwnerPortV1>,
    pub query: Arc<RemoteQueryProtocolOwnerPortV1>,
    pub backup: Arc<RemoteBackupProtocolOwnerPortV1>,
    pub restore: Arc<RemoteRestoreProtocolOwnerPortV1>,
    pub promotion: Arc<RemotePromotionProtocolOwnerPortV1>,
}
