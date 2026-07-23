//! Test-only publication bridge for the pre-cutover native graph runtime.
//!
//! This module is reachable only under `cfg(test)`. Production construction
//! continues to select [`LifecycleShardRuntimePublisher`], so compiling these
//! integration tests cannot enable live migration, native dogfood, or a hidden
//! runtime cutover.

use std::sync::Arc;

use tracedecay_rusqlite_runtime::graph::{
    CodeShardPhysicalLocator, GraphPhysicalAttachmentFactory, GraphRuntimePhysicalAttachment,
};
use tracedecay_store::{AdmissionConfigV1, RuntimeMaintenanceStateV1, StoreShardScopeV1};

use super::{
    LifecycleShardRuntimePublisher, PhysicalRuntimeAttachment, PhysicalRuntimeSnapshot,
    PublishedShardRuntime, ShardRuntimeBuildRequest, ShardRuntimePublisher,
    StoreRuntimeRegistryFailure, StoreRuntimeRegistryFuture,
};
use crate::daemon::store_runtime::shard::ShardRuntime;

pub(crate) struct ExplicitPrecutoverRusqliteGraphPublisher {
    admission: AdmissionConfigV1,
}

impl ExplicitPrecutoverRusqliteGraphPublisher {
    pub(crate) fn for_test_integration(admission: AdmissionConfigV1) -> Self {
        Self { admission }
    }
}

impl ShardRuntimePublisher for ExplicitPrecutoverRusqliteGraphPublisher {
    fn publish(
        &self,
        request: ShardRuntimeBuildRequest,
    ) -> StoreRuntimeRegistryFuture<'_, Result<PublishedShardRuntime, StoreRuntimeRegistryFailure>>
    {
        let admission = self.admission.clone();
        Box::pin(async move {
            if !matches!(
                &request.binding().shard_id.scope,
                StoreShardScopeV1::Code { .. }
            ) {
                let publisher = LifecycleShardRuntimePublisher;
                return publisher.publish(request).await;
            }

            let physical_locator = CodeShardPhysicalLocator::from_verified_existing(
                request.binding().clone(),
                request.locator().verified().clone(),
                request.locator().path().to_path_buf(),
            )
            .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "prepare rusqlite graph locator",
                message: error.to_string(),
            })?;
            let attachment = GraphPhysicalAttachmentFactory
                .attach(&physical_locator, admission)
                .map_err(|error| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "attach rusqlite graph runtime",
                    message: error.to_string(),
                })?;
            if attachment.binding() != request.binding().clone() {
                return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                    expected: Box::new(request.binding().clone()),
                    actual: Box::new(attachment.binding()),
                });
            }

            let runtime = Arc::new(ShardRuntime::new(request.binding().clone(), false));
            runtime
                .transition(RuntimeMaintenanceStateV1::Opening)
                .and_then(|()| runtime.transition(RuntimeMaintenanceStateV1::Ready))
                .map_err(
                    |error| StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
                        message: error.to_string(),
                    },
                )?;
            Ok(PublishedShardRuntime::new(
                runtime,
                Arc::new(RusqliteGraphAttachment { inner: attachment }),
            ))
        })
    }
}

struct RusqliteGraphAttachment {
    inner: GraphRuntimePhysicalAttachment,
}

impl PhysicalRuntimeAttachment for RusqliteGraphAttachment {
    fn snapshot(&self) -> PhysicalRuntimeSnapshot {
        let snapshot = self.inner.snapshot();
        PhysicalRuntimeSnapshot {
            healthy: snapshot.healthy,
            writer_present: snapshot.writer_present,
            reader_handles: snapshot.reader_handles,
            queued_operations: snapshot.queued_operations,
            queued_bytes: snapshot.queued_bytes,
            wal_bytes: snapshot.wal_bytes,
            memory_estimate_bytes: 0,
        }
    }

    fn drain(&self) -> Result<(), String> {
        self.inner.drain()
    }

    fn close_and_join(&self) -> Result<(), String> {
        self.inner.close_and_join()
    }
}

#[cfg(test)]
mod tests {
    use std::{fmt::Debug, path::PathBuf, sync::Arc};

    use tracedecay_domain::{
        BrainId, LocatorDigest, ProjectId, RepositoryId, UserProfileId, WorktreeId,
    };
    use tracedecay_rusqlite_runtime::graph::fixtures::create_graph_fixture_database_v1;
    use tracedecay_store::{
        CodeShardScopeV1, StoreIncarnationV1, StoreShardIdV1, VerifiedStoreLocatorV1,
    };

    use super::*;
    use crate::daemon::store_runtime::registry::{
        ProfileAuthorityPinResult, ResolvedStoreLocator, StoreRuntimeKey, StoreRuntimeOpenRequest,
        StoreRuntimeOpenResult, StoreRuntimeRegistry, StoreRuntimeResolver,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn profile_shard() -> StoreShardIdV1 {
        StoreShardIdV1::profile(
            id::<BrainId>("brain.rusqlite-gate"),
            id::<UserProfileId>("profile.rusqlite-gate"),
        )
    }

    fn code_shard() -> StoreShardIdV1 {
        StoreShardIdV1::code(
            id::<BrainId>("brain.rusqlite-gate"),
            id::<UserProfileId>("profile.rusqlite-gate"),
            id::<ProjectId>("project.rusqlite-gate"),
            id::<RepositoryId>("repository.rusqlite-gate"),
            CodeShardScopeV1::Worktree {
                worktree_id: id::<WorktreeId>("worktree.rusqlite-gate"),
            },
        )
    }

    struct FixtureResolver {
        path: PathBuf,
    }

    impl StoreRuntimeResolver for FixtureResolver {
        fn resolve<'a>(
            &'a self,
            key: &'a StoreRuntimeKey,
        ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
        {
            let verified = VerifiedStoreLocatorV1::new(
                key.shard_id().clone(),
                key.incarnation(),
                LocatorDigest::new(format!("sha256:{}", "7".repeat(64))).unwrap(),
            );
            let path = self.path.clone();
            Box::pin(async move { Ok(ResolvedStoreLocator::new(verified, path)) })
        }
    }

    #[tokio::test]
    async fn explicit_gate_publishes_and_drains_real_rusqlite_graph_handles() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("graph.db");
        create_graph_fixture_database_v1(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let registry = StoreRuntimeRegistry::new(
            Arc::new(FixtureResolver { path }),
            Arc::new(
                ExplicitPrecutoverRusqliteGraphPublisher::for_test_integration(
                    AdmissionConfigV1::default(),
                ),
            ),
        );
        let incarnation = StoreIncarnationV1::new(1).unwrap();

        let profile = match registry
            .open(StoreRuntimeOpenRequest::new(
                profile_shard(),
                incarnation,
                None,
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            other @ StoreRuntimeOpenResult::Failed(_) => {
                panic!("profile publication failed: {other:?}")
            }
        };
        let pin = match registry.profile_authority_pin(&profile_shard()) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            other => panic!("profile pin failed: {other:?}"),
        };
        let code = match registry
            .open(StoreRuntimeOpenRequest::new(
                code_shard(),
                incarnation,
                Some(pin),
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            other @ StoreRuntimeOpenResult::Failed(_) => {
                panic!("code publication failed: {other:?}")
            }
        };

        let ready = code.physical_snapshot();
        assert!(ready.healthy);
        assert!(ready.writer_present);
        assert_eq!(ready.reader_handles, 3);
        code.inner.attachment.drain().unwrap();
        assert!(code.physical_snapshot().is_drained());
        code.inner.attachment.close_and_join().unwrap();
        drop(profile);
    }
}
