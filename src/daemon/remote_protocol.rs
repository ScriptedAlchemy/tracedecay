//! Daemon-owned adapter for the canonical Remote Brain protocol router.
//!
//! The HTTP/TLS listener owns transport admission. This adapter owns the
//! application hand-off so a listener cannot substitute ad-hoc route handlers
//! or bypass the canonical enrollment, replay, query, recovery, and promotion
//! operations.

use std::path::Path;
use std::sync::Arc;

use thiserror::Error;
use tracedecay_application::remote::auth::{
    OpaqueRemoteCredential, RemoteEnrollmentAuthorityErrorV1,
};
use tracedecay_application::remote::protocol::{
    EnrollmentRequestV1, RemoteEnrollmentProtocolPortV1, RemoteProtocolPortV1,
    RemoteProtocolRequestV1, RemoteProtocolResponseV1,
};
use tracedecay_application::remote::query::{
    RemoteExactObservationQueryProtocolAdapterV1, RemoteExactObservationQueryServiceV1,
    RemoteQueryRequestV1, RemoteQueryResultV1,
};
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_application::remote::replay::{
    RemoteReplayApplicationErrorV1, RemoteReplayOutcomeV1, RemoteReplayProtocolAdapterV1,
    RemoteReplayRequestV1, RemoteReplayServiceV1, RemoteReplayTransactionPortV1,
};
use tracedecay_domain::EnrollmentCredentialRecordV1;
use tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle;
use tracedecay_rusqlite_runtime::remote_authority::{
    RegisteredRemoteEnrollmentAuthorityV1, RegisteredRemoteReplayPolicyAuthorityV1,
    RusqliteRemoteAuthorityStoreV1, RusqliteRemoteExactObservationQueryPortV1,
};
use tracedecay_rusqlite_runtime::remote_spool::RemoteCaptureSpool;
use tracedecay_rusqlite_runtime::repository::RepositoryRuntimePhysicalAttachment;

use super::remote_enrollment::{
    DaemonRemoteEnrollmentProvisionerV1, DaemonRemoteEnrollmentProvisioningErrorV1,
};

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

pub type ReplayOwnerV1 =
    dyn RemoteProtocolPortV1<RemoteReplayRequestV1, Output = RemoteReplayOutcomeV1> + Send + Sync;
pub type QueryOwnerV1 =
    dyn RemoteProtocolPortV1<RemoteQueryRequestV1, Output = RemoteQueryResultV1> + Send + Sync;
pub type BackupOwnerV1 =
    dyn RemoteProtocolPortV1<BackupRequestV1, Output = BackupOperationStateV1> + Send + Sync;
pub type RestoreOwnerV1 = dyn RemoteProtocolPortV1<StagedRestoreConfirmationV1, Output = StagedRestoreProgressV1>
    + Send
    + Sync;
pub type PromotionOwnerV1 =
    dyn RemoteProtocolPortV1<PromotionConfirmationV1, Output = PromotionCasReceiptV1> + Send + Sync;

/// Production aggregate of the six canonical operation owners. It introduces
/// no state or fallback behavior: each operation is delegated to the authority
/// that owns its typed application service.
pub struct CanonicalDaemonRemoteProtocolOwnersV1 {
    enrollment: Arc<dyn RemoteEnrollmentProtocolPortV1>,
    replay: Arc<ReplayOwnerV1>,
    query: Arc<QueryOwnerV1>,
    backup: Arc<BackupOwnerV1>,
    restore: Arc<RestoreOwnerV1>,
    promotion: Arc<PromotionOwnerV1>,
}

impl CanonicalDaemonRemoteProtocolOwnersV1 {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_registered_enrollment(
        enrollment_store: MigrationSqlHandle,
        enrollment_configuration_path: &Path,
        replay: Arc<ReplayOwnerV1>,
        query: Arc<QueryOwnerV1>,
        backup: Arc<BackupOwnerV1>,
        restore: Arc<RestoreOwnerV1>,
        promotion: Arc<PromotionOwnerV1>,
    ) -> Result<Self, DaemonRemoteEnrollmentProvisioningErrorV1> {
        let enrollment = DaemonRemoteEnrollmentProvisionerV1::from_registered_configured(
            enrollment_store,
            enrollment_configuration_path,
        )?;
        Ok(Self::new(
            enrollment.protocol_port(),
            replay,
            query,
            backup,
            restore,
            promotion,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_registered_enrollment_and_replay(
        enrollment_store: MigrationSqlHandle,
        enrollment_configuration_path: &Path,
        replay_spool: Arc<RemoteCaptureSpool>,
        replay_authority: Arc<RusqliteRemoteAuthorityStoreV1>,
        replay_transaction: Arc<dyn RemoteReplayTransactionPortV1>,
        query: Arc<QueryOwnerV1>,
        backup: Arc<BackupOwnerV1>,
        restore: Arc<RestoreOwnerV1>,
        promotion: Arc<PromotionOwnerV1>,
    ) -> Result<Self, DaemonRemoteRegisteredOwnerErrorV1> {
        let enrollment = DaemonRemoteEnrollmentProvisionerV1::from_registered_configured(
            enrollment_store.clone(),
            enrollment_configuration_path,
        )?;
        let credentials = Arc::new(RegisteredRemoteEnrollmentAuthorityV1::from_registered(
            enrollment_store.clone(),
        )?);
        let policy = Arc::new(RegisteredRemoteReplayPolicyAuthorityV1::from_registered(
            enrollment_store,
        )?);
        let replay = Arc::new(RemoteReplayProtocolAdapterV1::new(
            RemoteReplayServiceV1::new(
                credentials.clone(),
                credentials,
                replay_spool.clone(),
                replay_authority,
                policy.clone(),
                policy,
                replay_transaction,
                replay_spool,
            ),
        ));
        Ok(Self::new(
            enrollment.protocol_port(),
            replay,
            query,
            backup,
            restore,
            promotion,
        ))
    }

    pub(crate) fn new_with_registered_enrollment_and_query(
        enrollment_store: MigrationSqlHandle,
        enrollment_configuration_path: &Path,
        remote_authority: Arc<RusqliteRemoteAuthorityStoreV1>,
        repository: Arc<RepositoryRuntimePhysicalAttachment>,
    ) -> Result<DaemonRemoteQueryProtocolPortV1, DaemonRemoteRegisteredOwnerErrorV1> {
        let enrollment = DaemonRemoteEnrollmentProvisionerV1::from_registered_configured(
            enrollment_store.clone(),
            enrollment_configuration_path,
        )?;
        let credentials = Arc::new(RegisteredRemoteEnrollmentAuthorityV1::from_registered(
            enrollment_store,
        )?);
        let query = Arc::new(RemoteExactObservationQueryProtocolAdapterV1::new(
            RemoteExactObservationQueryServiceV1::new(
                credentials,
                Arc::new(RusqliteRemoteExactObservationQueryPortV1::new(
                    remote_authority,
                    repository,
                )),
            ),
        ));
        Ok(DaemonRemoteQueryProtocolPortV1::new(
            enrollment.protocol_port(),
            query,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_registered_enrollment_replay_and_query(
        enrollment_store: MigrationSqlHandle,
        enrollment_configuration_path: &Path,
        replay_spool: Arc<RemoteCaptureSpool>,
        remote_authority: Arc<RusqliteRemoteAuthorityStoreV1>,
        replay_transaction: Arc<dyn RemoteReplayTransactionPortV1>,
        repository: Arc<RepositoryRuntimePhysicalAttachment>,
        backup: Arc<BackupOwnerV1>,
        restore: Arc<RestoreOwnerV1>,
        promotion: Arc<PromotionOwnerV1>,
    ) -> Result<Self, DaemonRemoteRegisteredOwnerErrorV1> {
        let enrollment = DaemonRemoteEnrollmentProvisionerV1::from_registered_configured(
            enrollment_store.clone(),
            enrollment_configuration_path,
        )?;
        let credentials = Arc::new(RegisteredRemoteEnrollmentAuthorityV1::from_registered(
            enrollment_store.clone(),
        )?);
        let policy = Arc::new(RegisteredRemoteReplayPolicyAuthorityV1::from_registered(
            enrollment_store,
        )?);
        let replay = Arc::new(RemoteReplayProtocolAdapterV1::new(
            RemoteReplayServiceV1::new(
                credentials.clone(),
                credentials.clone(),
                replay_spool.clone(),
                remote_authority.clone(),
                policy.clone(),
                policy,
                replay_transaction,
                replay_spool,
            ),
        ));
        let query = Arc::new(RemoteExactObservationQueryProtocolAdapterV1::new(
            RemoteExactObservationQueryServiceV1::new(
                credentials,
                Arc::new(RusqliteRemoteExactObservationQueryPortV1::new(
                    remote_authority,
                    repository,
                )),
            ),
        ));
        Ok(Self::new(
            enrollment.protocol_port(),
            replay,
            query,
            backup,
            restore,
            promotion,
        ))
    }

    pub fn new(
        enrollment: Arc<dyn RemoteEnrollmentProtocolPortV1>,
        replay: Arc<ReplayOwnerV1>,
        query: Arc<QueryOwnerV1>,
        backup: Arc<BackupOwnerV1>,
        restore: Arc<RestoreOwnerV1>,
        promotion: Arc<PromotionOwnerV1>,
    ) -> Self {
        Self {
            enrollment,
            replay,
            query,
            backup,
            restore,
            promotion,
        }
    }

    pub fn into_protocol_port(self) -> DaemonRemoteProtocolPortV1 {
        DaemonRemoteProtocolPortV1::new(Arc::new(self))
    }
}

#[derive(Clone)]
pub struct DaemonRemoteQueryProtocolPortV1 {
    enrollment: Arc<dyn RemoteEnrollmentProtocolPortV1>,
    query: Arc<QueryOwnerV1>,
}

impl DaemonRemoteQueryProtocolPortV1 {
    fn new(enrollment: Arc<dyn RemoteEnrollmentProtocolPortV1>, query: Arc<QueryOwnerV1>) -> Self {
        Self { enrollment, query }
    }
}

impl RemoteEnrollmentProtocolPortV1 for DaemonRemoteQueryProtocolPortV1 {
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

impl RemoteProtocolPortV1<RemoteQueryRequestV1> for DaemonRemoteQueryProtocolPortV1 {
    type Output = RemoteQueryResultV1;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteQueryRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<Self::Output> {
        self.query.execute(request, credential)
    }
}

#[derive(Debug, Error)]
pub enum DaemonRemoteRegisteredOwnerErrorV1 {
    #[error(transparent)]
    Enrollment(#[from] DaemonRemoteEnrollmentProvisioningErrorV1),
    #[error(transparent)]
    EnrollmentAuthority(#[from] RemoteEnrollmentAuthorityErrorV1),
    #[error(transparent)]
    Replay(#[from] RemoteReplayApplicationErrorV1),
}

impl DaemonRemoteProtocolOwnersV1 for CanonicalDaemonRemoteProtocolOwnersV1 {
    fn enroll(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        grant_credential: OpaqueRemoteCredential,
        enrollment_credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<EnrollmentCredentialRecordV1> {
        self.enrollment
            .execute_enrollment(request, grant_credential, enrollment_credential)
    }

    fn replay(
        &self,
        request: RemoteProtocolRequestV1<RemoteReplayRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<RemoteReplayOutcomeV1> {
        self.replay.execute(request, credential)
    }

    fn query(
        &self,
        request: RemoteProtocolRequestV1<RemoteQueryRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<RemoteQueryResultV1> {
        self.query.execute(request, credential)
    }

    fn backup(
        &self,
        request: RemoteProtocolRequestV1<BackupRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<BackupOperationStateV1> {
        self.backup.execute(request, credential)
    }

    fn restore(
        &self,
        request: RemoteProtocolRequestV1<StagedRestoreConfirmationV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<StagedRestoreProgressV1> {
        self.restore.execute(request, credential)
    }

    fn promote(
        &self,
        request: RemoteProtocolRequestV1<PromotionConfirmationV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<PromotionCasReceiptV1> {
        self.promotion.execute(request, credential)
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_application::remote::protocol::{
        RemoteProtocolFailureV1, remote_protocol_problem,
    };
    use tracedecay_application::{RequestId, ResultContractRef};
    use tracedecay_domain::{
        BrainId, BrainNodeId, CurrentRemoteAuthorityStateV1, EntityId, ProjectId, RefId,
        RemoteAuthorityUnavailableReasonV1, RemoteCapabilityV1, RemoteRepositoryScopeV1,
        RepositoryId, RepositoryStateSnapshotId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::SchemaId;

    use super::*;

    struct CountingEnrollment {
        calls: Arc<AtomicUsize>,
    }

    impl RemoteEnrollmentProtocolPortV1 for CountingEnrollment {
        fn execute_enrollment(
            &self,
            request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
            _grant_credential: OpaqueRemoteCredential,
            _enrollment_credential: OpaqueRemoteCredential,
        ) -> RemoteProtocolResponseV1<EnrollmentCredentialRecordV1> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let request_id = request.request_id;
            RemoteProtocolResponseV1::new(
                request_id.clone(),
                CurrentRemoteAuthorityStateV1::Unavailable {
                    reason: RemoteAuthorityUnavailableReasonV1::AuthorityUnreachable,
                    observed_at: request.sent_at,
                },
                Err(remote_protocol_problem(
                    ResultContractRef::new(SchemaId::new("remote.result").unwrap(), 1).unwrap(),
                    request_id,
                    RemoteProtocolFailureV1::AuthorityUnavailable,
                )),
            )
            .unwrap()
        }
    }

    struct UnusedOwner;

    macro_rules! unused_owner {
        ($request:ty, $output:ty) => {
            impl RemoteProtocolPortV1<$request> for UnusedOwner {
                type Output = $output;

                fn execute(
                    &self,
                    _request: RemoteProtocolRequestV1<$request>,
                    _credential: OpaqueRemoteCredential,
                ) -> RemoteProtocolResponseV1<Self::Output> {
                    unreachable!("the aggregate must delegate enrollment only")
                }
            }
        };
    }

    unused_owner!(RemoteReplayRequestV1, RemoteReplayOutcomeV1);
    unused_owner!(RemoteQueryRequestV1, RemoteQueryResultV1);
    unused_owner!(BackupRequestV1, BackupOperationStateV1);
    unused_owner!(StagedRestoreConfirmationV1, StagedRestoreProgressV1);
    unused_owner!(PromotionConfirmationV1, PromotionCasReceiptV1);

    fn credential(byte: u8) -> OpaqueRemoteCredential {
        OpaqueRemoteCredential::new(vec![byte; 32].into_boxed_slice()).unwrap()
    }

    #[test]
    fn canonical_owner_aggregate_delegates_enrollment_exactly_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let owners = CanonicalDaemonRemoteProtocolOwnersV1::new(
            Arc::new(CountingEnrollment {
                calls: Arc::clone(&calls),
            }),
            Arc::new(UnusedOwner),
            Arc::new(UnusedOwner),
            Arc::new(UnusedOwner),
            Arc::new(UnusedOwner),
            Arc::new(UnusedOwner),
        );
        let scope = RemoteRepositoryScopeV1 {
            project_id: ProjectId::new("project.remote").unwrap(),
            repository_id: RepositoryId::new("repository.remote").unwrap(),
            worktree_id: WorktreeId::new("worktree.remote").unwrap(),
            reference: Some(RefId::new("refs/heads/main").unwrap()),
            snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote").unwrap(),
        };
        let brain_id = BrainId::new("brain.remote").unwrap();
        let node_id = BrainNodeId::new("node.remote").unwrap();
        let request = RemoteProtocolRequestV1::new_initial_enrollment(
            RequestId::new("request.remote.enrollment").unwrap(),
            brain_id.clone(),
            node_id.clone(),
            UtcMicros(10),
            EnrollmentRequestV1 {
                grant_id: EntityId::new("grant.remote").unwrap(),
                grant_revision: 1,
                enrollment_id: EntityId::new("enrollment.remote").unwrap(),
                brain_id,
                node_id,
                expires_at: UtcMicros(20),
                capabilities: BTreeSet::from([RemoteCapabilityV1::Query]),
                scope,
            },
        )
        .unwrap();

        owners.enroll(request, credential(b'g'), credential(b'e'));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
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
