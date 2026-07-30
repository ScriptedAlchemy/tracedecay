//! Daemon-owned adapter for the canonical Remote Brain protocol router.
//!
//! The HTTP/TLS listener owns transport admission. This adapter owns the
//! application hand-off so a listener cannot substitute ad-hoc route handlers
//! or bypass the canonical enrollment, replay, query, recovery, and promotion
//! operations.

use std::sync::Arc;

use tracedecay_application::remote::auth::OpaqueRemoteCredential;
use tracedecay_application::remote::protocol::{
    EnrollmentRequestV1, RemoteEnrollmentProtocolPortV1, RemoteProtocolPortV1,
    RemoteProtocolRequestV1, RemoteProtocolResponseV1,
};
use tracedecay_application::remote::query::{RemoteQueryRequestV1, RemoteQueryResultV1};
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_application::remote::replay::{RemoteReplayOutcomeV1, RemoteReplayRequestV1};
use tracedecay_domain::EnrollmentCredentialRecordV1;

/// The daemon's concrete owner composition. Implementations must delegate each
/// method to the corresponding canonical authority; this boundary intentionally
/// has no generic JSON or database escape hatch.
pub trait DaemonRemoteProtocolOwnersV1: Send + Sync {
    fn enroll(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        grant_credential: OpaqueRemoteCredential,
        enrollment_credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<EnrollmentCredentialRecordV1>;

    fn replay(
        &self,
        request: RemoteProtocolRequestV1<RemoteReplayRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<RemoteReplayOutcomeV1>;

    fn query(
        &self,
        request: RemoteProtocolRequestV1<RemoteQueryRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<RemoteQueryResultV1>;

    fn backup(
        &self,
        request: RemoteProtocolRequestV1<BackupRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<BackupOperationStateV1>;

    fn restore(
        &self,
        request: RemoteProtocolRequestV1<StagedRestoreConfirmationV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<StagedRestoreProgressV1>;

    fn promote(
        &self,
        request: RemoteProtocolRequestV1<PromotionConfirmationV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<PromotionCasReceiptV1>;
}

/// Concrete daemon-owned protocol port mounted by
/// [`super::remote_https::RemoteBrainHttpsService::bind_protocol`].
#[derive(Clone)]
pub struct DaemonRemoteProtocolPortV1 {
    owners: Arc<dyn DaemonRemoteProtocolOwnersV1>,
}

impl DaemonRemoteProtocolPortV1 {
    pub fn new(owners: Arc<dyn DaemonRemoteProtocolOwnersV1>) -> Self {
        Self { owners }
    }
}

impl RemoteEnrollmentProtocolPortV1 for DaemonRemoteProtocolPortV1 {
    fn execute_enrollment(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        grant_credential: OpaqueRemoteCredential,
        enrollment_credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<EnrollmentCredentialRecordV1> {
        self.owners
            .enroll(request, grant_credential, enrollment_credential)
    }
}

macro_rules! delegate_remote_operation {
    ($request:ty, $output:ty, $method:ident) => {
        impl RemoteProtocolPortV1<$request> for DaemonRemoteProtocolPortV1 {
            type Output = $output;

            fn execute(
                &self,
                request: RemoteProtocolRequestV1<$request>,
                credential: OpaqueRemoteCredential,
            ) -> RemoteProtocolResponseV1<Self::Output> {
                self.owners.$method(request, credential)
            }
        }
    };
}

delegate_remote_operation!(RemoteReplayRequestV1, RemoteReplayOutcomeV1, replay);
delegate_remote_operation!(RemoteQueryRequestV1, RemoteQueryResultV1, query);
delegate_remote_operation!(BackupRequestV1, BackupOperationStateV1, backup);
delegate_remote_operation!(
    StagedRestoreConfirmationV1,
    StagedRestoreProgressV1,
    restore
);
delegate_remote_operation!(PromotionConfirmationV1, PromotionCasReceiptV1, promote);
