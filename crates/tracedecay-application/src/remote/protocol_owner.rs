//! One canonical owner for the complete authenticated Remote Brain protocol.
//!
//! The owner composes operation authorities without flattening them into a
//! store-aware service or a generic untyped dispatcher. Each operation keeps
//! its existing application port and exact output contract.

use std::sync::Arc;

use tracedecay_domain::EnrollmentCredentialRecordV1;

use super::{
    auth::OpaqueRemoteCredential,
    capture::RemoteCaptureReceiptV1,
    capture_protocol::RemoteCaptureRequestV1,
    protocol::{
        EnrollmentRequestV1, RemoteEnrollmentProtocolPortV1, RemoteProtocolPortV1,
        RemoteProtocolRequestV1, RemoteProtocolResponseV1,
    },
    query::{RemoteQueryRequestV1, RemoteQueryResultV1},
    recovery::{
        BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
        StagedRestoreConfirmationV1, StagedRestoreProgressV1,
    },
    replay::{RemoteReplayOutcomeV1, RemoteReplayRequestV1},
};

pub type RemoteCaptureProtocolOwnerPortV1 =
    dyn RemoteProtocolPortV1<RemoteCaptureRequestV1, Output = RemoteCaptureReceiptV1> + Send + Sync;
pub type RemoteReplayProtocolOwnerPortV1 =
    dyn RemoteProtocolPortV1<RemoteReplayRequestV1, Output = RemoteReplayOutcomeV1> + Send + Sync;
pub type RemoteQueryProtocolOwnerPortV1 =
    dyn RemoteProtocolPortV1<RemoteQueryRequestV1, Output = RemoteQueryResultV1> + Send + Sync;
pub type RemoteBackupProtocolOwnerPortV1 =
    dyn RemoteProtocolPortV1<BackupRequestV1, Output = BackupOperationStateV1> + Send + Sync;
pub type RemoteRestoreProtocolOwnerPortV1 = dyn RemoteProtocolPortV1<StagedRestoreConfirmationV1, Output = StagedRestoreProgressV1>
    + Send
    + Sync;
pub type RemotePromotionProtocolOwnerPortV1 =
    dyn RemoteProtocolPortV1<PromotionConfirmationV1, Output = PromotionCasReceiptV1> + Send + Sync;

pub struct RemoteProtocolOwnerV1 {
    enrollment: Arc<dyn RemoteEnrollmentProtocolPortV1>,
    capture: Arc<RemoteCaptureProtocolOwnerPortV1>,
    replay: Arc<RemoteReplayProtocolOwnerPortV1>,
    query: Arc<RemoteQueryProtocolOwnerPortV1>,
    backup: Arc<RemoteBackupProtocolOwnerPortV1>,
    restore: Arc<RemoteRestoreProtocolOwnerPortV1>,
    promotion: Arc<RemotePromotionProtocolOwnerPortV1>,
}

impl RemoteProtocolOwnerV1 {
    pub fn new(
        enrollment: Arc<dyn RemoteEnrollmentProtocolPortV1>,
        capture: Arc<RemoteCaptureProtocolOwnerPortV1>,
        replay: Arc<RemoteReplayProtocolOwnerPortV1>,
        query: Arc<RemoteQueryProtocolOwnerPortV1>,
        backup: Arc<RemoteBackupProtocolOwnerPortV1>,
        restore: Arc<RemoteRestoreProtocolOwnerPortV1>,
        promotion: Arc<RemotePromotionProtocolOwnerPortV1>,
    ) -> Self {
        Self {
            enrollment,
            capture,
            replay,
            query,
            backup,
            restore,
            promotion,
        }
    }
}

impl RemoteEnrollmentProtocolPortV1 for RemoteProtocolOwnerV1 {
    fn execute_enrollment(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        grant_credential: OpaqueRemoteCredential,
        enrollment_credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<EnrollmentCredentialRecordV1> {
        self.enrollment
            .execute_enrollment(request, grant_credential, enrollment_credential)
    }
}

macro_rules! delegate_remote_operation {
    ($request:ty, $output:ty, $field:ident) => {
        impl RemoteProtocolPortV1<$request> for RemoteProtocolOwnerV1 {
            type Output = $output;

            fn execute(
                &self,
                request: RemoteProtocolRequestV1<$request>,
                credential: OpaqueRemoteCredential,
            ) -> RemoteProtocolResponseV1<Self::Output> {
                self.$field.execute(request, credential)
            }

            fn execute_controlled(
                &self,
                request: RemoteProtocolRequestV1<$request>,
                credential: OpaqueRemoteCredential,
                control: crate::remote::protocol::RemoteProtocolExecutionControlV1,
            ) -> RemoteProtocolResponseV1<Self::Output> {
                self.$field.execute_controlled(request, credential, control)
            }
        }
    };
}

delegate_remote_operation!(RemoteCaptureRequestV1, RemoteCaptureReceiptV1, capture);
delegate_remote_operation!(RemoteReplayRequestV1, RemoteReplayOutcomeV1, replay);
delegate_remote_operation!(RemoteQueryRequestV1, RemoteQueryResultV1, query);
delegate_remote_operation!(BackupRequestV1, BackupOperationStateV1, backup);
delegate_remote_operation!(
    StagedRestoreConfirmationV1,
    StagedRestoreProgressV1,
    restore
);
delegate_remote_operation!(PromotionConfirmationV1, PromotionCasReceiptV1, promotion);
