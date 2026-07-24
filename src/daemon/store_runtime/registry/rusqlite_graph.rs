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

    fn opened_file_identity(&self) -> Result<u64, String> {
        Ok(self.inner.opened_file_identity())
    }

    fn drain(&self) -> Result<(), String> {
        self.inner.drain()
    }

    fn close_and_join(&self) -> Result<(), String> {
        self.inner.close_and_join()
    }

    fn migration_sql_handle(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle, String> {
        self.inner
            .migration_sql_handle()
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::{fmt::Debug, fs, path::PathBuf, sync::Arc, time::Duration};

    use tracedecay_domain::{
        BrainId, LocatorDigest, ProjectId, RepositoryId, UserProfileId, UtcMicros, WorktreeId,
    };
    use tracedecay_rusqlite_runtime::graph::fixtures::create_graph_fixture_database_v1;
    use tracedecay_store::{
        CodeShardScopeV1, ConsistencyModeV1, OperationPriorityV1, RuntimeCancellationIdV1,
        RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
        RuntimeReadOperationV1, RuntimeReadRequestV1, RuntimeRequestControlV1,
        RuntimeRequestProbeV1, StoreIncarnationV1, StoreShardIdV1, VerifiedStoreLocatorV1,
    };

    use super::*;
    use crate::daemon::store_runtime::registry::{
        ProfileAuthorityPinResult, ResolvedStoreLocator, StoreRuntimeHandle, StoreRuntimeKey,
        StoreRuntimeOpenBegin, StoreRuntimeOpenMode, StoreRuntimeOpenRequest,
        StoreRuntimeOpenResult, StoreRuntimeRegistry, StoreRuntimeRegistryConfig,
        StoreRuntimeResolver,
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
        code_shard_for("worktree.rusqlite-gate")
    }

    fn code_shard_for(worktree_id: &str) -> StoreShardIdV1 {
        StoreShardIdV1::code(
            id::<BrainId>("brain.rusqlite-gate"),
            id::<UserProfileId>("profile.rusqlite-gate"),
            id::<ProjectId>("project.rusqlite-gate"),
            id::<RepositoryId>("repository.rusqlite-gate"),
            CodeShardScopeV1::Worktree {
                worktree_id: id::<WorktreeId>(worktree_id),
            },
        )
    }

    struct FixtureResolver {
        path: PathBuf,
    }

    struct Probe {
        cancellation: RuntimeCancellationIdentityV1,
        deadline: RuntimeDeadlineV1,
    }

    impl RuntimeRequestProbeV1 for Probe {
        fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
            &self.cancellation
        }

        fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
            &self.deadline
        }

        fn interruption(&self) -> Option<tracedecay_store::RuntimeInterruptionV1> {
            None
        }
    }

    impl StoreRuntimeResolver for FixtureResolver {
        fn resolve<'a>(
            &'a self,
            key: &'a StoreRuntimeKey,
            _mode: StoreRuntimeOpenMode,
            _database_authority: Option<&'a crate::db::DatabaseAuthority>,
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

    async fn open_graph_runtime(path: PathBuf) -> (StoreRuntimeHandle, StoreRuntimeHandle) {
        let authority =
            crate::db::DatabaseAuthority::for_runtime(&path, "mount graph runtime fixture")
                .unwrap();
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
            .open(StoreRuntimeOpenRequest::new_authorized(
                code_shard(),
                incarnation,
                Some(pin),
                authority,
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            other @ StoreRuntimeOpenResult::Failed(_) => {
                panic!("code publication failed: {other:?}")
            }
        };
        (profile, code)
    }

    fn graph_quick_check_request(
        binding: &tracedecay_store::StoreRuntimeBindingV1,
    ) -> (RuntimeReadRequestV1, Probe) {
        let cancellation = RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new("cancel.graph-replacement").unwrap(),
            generation: 1,
        };
        let deadline = RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new("deadline.graph-replacement").unwrap(),
        };
        let control = RuntimeRequestControlV1 {
            requested_at: UtcMicros(1),
            deadline: deadline.clone(),
            cancellation: cancellation.clone(),
        };
        (
            RuntimeReadRequestV1::new(
                binding.clone(),
                ConsistencyModeV1::LatestAvailable,
                RuntimeReadOperationV1::GraphQuickCheck,
                OperationPriorityV1::Health,
                1,
                control,
            )
            .unwrap(),
            Probe {
                cancellation,
                deadline,
            },
        )
    }

    #[cfg(unix)]
    fn replace_graph_database(path: &std::path::Path) {
        let replacement = path.with_extension("replacement.db");
        let retired = path.with_extension("retired.db");
        create_graph_fixture_database_v1(&replacement).unwrap();
        fs::rename(path, &retired).unwrap();
        fs::rename(replacement, path).unwrap();
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn eviction_quiesces_accepted_graph_writer_work_without_quarantining_runtime() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("graph.db");
        create_graph_fixture_database_v1(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let registry = StoreRuntimeRegistry::with_config(
            Arc::new(FixtureResolver { path: path.clone() }),
            Arc::new(
                ExplicitPrecutoverRusqliteGraphPublisher::for_test_integration(
                    AdmissionConfigV1::default(),
                ),
            ),
            StoreRuntimeRegistryConfig::new(1).unwrap(),
        )
        .unwrap();
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
        let first_authority =
            crate::db::DatabaseAuthority::for_runtime(&path, "mount first eviction graph").unwrap();
        let first = match registry
            .open(StoreRuntimeOpenRequest::new_authorized(
                code_shard_for("worktree.eviction-first"),
                incarnation,
                Some(pin.clone()),
                first_authority,
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            other @ StoreRuntimeOpenResult::Failed(_) => {
                panic!("first code publication failed: {other:?}")
            }
        };
        let first_binding = first.binding().clone();
        let authority = first
            .database_authority("hold graph writer during eviction")
            .unwrap();
        let handle = first.authorized_migration_sql_handle(authority).unwrap();
        let holder = handle.begin_immediate().unwrap();
        let queued_handle = handle.clone();
        drop(handle);
        let (queued_sender, queued_receiver) = std::sync::mpsc::sync_channel(1);
        let queued = std::thread::spawn(move || {
            queued_sender
                .send(
                    queued_handle.execute_batch(
                        "INSERT INTO metadata (key, value)
                     VALUES ('queued-before-eviction', 'accepted')"
                            .to_owned(),
                    ),
                )
                .unwrap();
        });
        assert!(
            queued_receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "queued graph write unexpectedly bypassed the held transaction"
        );
        drop(first);

        let second_authority =
            crate::db::DatabaseAuthority::for_runtime(&path, "mount second eviction graph")
                .unwrap();
        let second_request = StoreRuntimeOpenRequest::new_authorized(
            code_shard_for("worktree.eviction-second"),
            incarnation,
            Some(pin),
            second_authority,
        );
        let eviction_registry = registry.clone();
        let (eviction_sender, eviction_receiver) = std::sync::mpsc::sync_channel(1);
        let eviction = std::thread::spawn(move || {
            eviction_sender
                .send(eviction_registry.begin_or_join_open(&second_request))
                .unwrap();
        });
        let _ = holder.rollback();

        let begin = eviction_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("eviction must quiesce accepted writer work");
        eviction.join().unwrap();
        let _queued_result = queued_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("accepted queued write must reach a terminal result");
        queued.join().unwrap();
        let join = match begin {
            StoreRuntimeOpenBegin::Started(join) => join,
            other => panic!("eviction did not reserve the replacement runtime: {other:?}"),
        };
        let second = match join.wait().await {
            StoreRuntimeOpenResult::Published(handle) => handle,
            other @ StoreRuntimeOpenResult::Failed(_) => {
                panic!("replacement code publication failed: {other:?}")
            }
        };
        assert_eq!(
            second.runtime().maintenance_state(),
            RuntimeMaintenanceStateV1::Ready
        );
        assert!(matches!(
            registry.lookup(&first_binding),
            crate::daemon::store_runtime::registry::StoreRuntimeLookup::Missing { .. }
        ));
        second.inner.attachment.drain().unwrap();
        second.inner.attachment.close_and_join().unwrap();
        drop(profile);
    }

    #[tokio::test]
    async fn verified_graph_runtime_round_trips_metadata_through_engine() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("graph.db");
        create_graph_fixture_database_v1(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let (profile, code) = open_graph_runtime(path).await;

        let metadata = code
            .graph_metadata()
            .expect("verified graph attachment must expose metadata");
        assert_eq!(metadata.get("stage_d1").await.unwrap(), None);
        metadata.set("stage_d1", "cutover").await.unwrap();
        assert_eq!(
            metadata.get("stage_d1").await.unwrap().as_deref(),
            Some("cutover")
        );
        drop(metadata);

        code.inner.attachment.drain().unwrap();
        code.inner.attachment.close_and_join().unwrap();
        drop(profile);
    }

    #[tokio::test]
    async fn issued_graph_metadata_writer_rejects_daemon_scope_loss() {
        let temporary = tempfile::tempdir().unwrap();
        let profile_root = temporary.path().join("profile");
        let path = profile_root.join("projects/project/graph.db");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        create_graph_fixture_database_v1(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let scope =
            crate::db::enter_daemon_database_scope(&profile_root, 1, "graph-metadata-issued")
                .unwrap();
        let (profile, code) = open_graph_runtime(path).await;
        let metadata = code
            .graph_metadata()
            .expect("verified graph attachment must expose metadata");
        metadata.set("authority", "active").await.unwrap();

        drop(scope);

        let error = metadata
            .set("authority", "revoked")
            .await
            .expect_err("issued graph metadata writer must reject revoked daemon scope");
        assert!(format!("{error:?}").contains("active daemon"));
        assert_eq!(
            metadata.get("authority").await.unwrap().as_deref(),
            Some("active")
        );
        drop(metadata);
        code.inner.attachment.drain().unwrap();
        code.inner.attachment.close_and_join().unwrap();
        drop(profile);
    }

    #[tokio::test]
    async fn graph_transaction_rechecks_daemon_scope_before_commit_and_rolls_back() {
        let temporary = tempfile::tempdir().unwrap();
        let profile_root = temporary.path().join("profile");
        let path = profile_root.join("projects/project/graph.db");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        create_graph_fixture_database_v1(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let scope =
            crate::db::enter_daemon_database_scope(&profile_root, 1, "graph-metadata-precommit")
                .unwrap();
        let (profile, code) = open_graph_runtime(path).await;
        let metadata = code
            .graph_metadata()
            .expect("verified graph attachment must expose metadata");
        let authority = code
            .database_authority("begin graph metadata precommit test")
            .unwrap();
        let transaction = code
            .authorized_migration_sql_handle(authority)
            .unwrap()
            .begin_immediate()
            .unwrap();
        transaction
            .execute_batch(
                "INSERT INTO metadata (key, value)
                 VALUES ('precommit-authority', 'must-roll-back')"
                    .to_owned(),
            )
            .unwrap();

        drop(scope);

        let error = transaction
            .commit()
            .expect_err("commit must recheck the originating daemon scope");
        assert!(matches!(
            error,
            tracedecay_rusqlite_runtime::migration_sql::MigrationSqlError::AuthorityDenied(_)
        ));
        assert_eq!(metadata.get("precommit-authority").await.unwrap(), None);
        drop(metadata);
        code.inner.attachment.drain().unwrap();
        code.inner.attachment.close_and_join().unwrap();
        drop(profile);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_graph_metadata_write_rechecks_authority_on_actor_dequeue() {
        let temporary = tempfile::tempdir().unwrap();
        let profile_root = temporary.path().join("profile");
        let path = profile_root.join("projects/project/graph.db");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        create_graph_fixture_database_v1(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let scope =
            crate::db::enter_daemon_database_scope(&profile_root, 1, "graph-metadata-queued")
                .unwrap();
        let (profile, code) = open_graph_runtime(path).await;
        let metadata = code
            .graph_metadata()
            .expect("verified graph attachment must expose metadata");
        let holder_authority = code
            .database_authority("hold graph metadata writer")
            .unwrap();
        let holder = code
            .authorized_migration_sql_handle(holder_authority)
            .unwrap()
            .begin_immediate()
            .unwrap();
        let error = {
            let queued_write = metadata.set("queued-authority", "must-not-persist");
            tokio::pin!(queued_write);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut queued_write)
                    .await
                    .is_err(),
                "metadata write unexpectedly bypassed the occupied writer actor"
            );

            drop(scope);
            holder.rollback().unwrap();

            queued_write
                .await
                .expect_err("queued graph metadata write must recheck revoked authority")
        };
        assert!(format!("{error:?}").contains("active daemon"));
        assert_eq!(metadata.get("queued-authority").await.unwrap(), None);
        drop(metadata);
        code.inner.attachment.drain().unwrap();
        code.inner.attachment.close_and_join().unwrap();
        drop(profile);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_live_graph_either_blocks_rename_or_detects_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("graph.db");
        let retired = temporary.path().join("graph.retired.db");
        create_graph_fixture_database_v1(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let (profile, code) = open_graph_runtime(path.clone()).await;

        match fs::rename(&path, &retired) {
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                assert!(
                    code.graph_metadata().is_ok(),
                    "a denied rename must leave the mounted graph usable"
                );
                code.inner.attachment.drain().unwrap();
                code.inner.attachment.close_and_join().unwrap();
                fs::rename(&path, &retired)
                    .expect("rename must succeed after every SQLite handle closes");
            }
            Ok(()) => {
                create_graph_fixture_database_v1(&path).unwrap();
                let error = match code.telemetry_read_handle() {
                    Ok(_) => panic!("replacement must invalidate the mounted graph"),
                    Err(error) => error,
                };
                assert!(
                    format!("{error:?}").contains("identity changed"),
                    "unexpected replacement error: {error:?}"
                );
                code.inner.attachment.drain().unwrap();
                code.inner.attachment.close_and_join().unwrap();
            }
            Err(error) => panic!("unexpected Windows rename error: {error}"),
        }
        drop(profile);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn same_path_graph_replacement_denies_existing_read_and_write_capabilities() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("graph.db");
        create_graph_fixture_database_v1(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let (profile, code) = open_graph_runtime(path.clone()).await;
        let metadata = code
            .graph_metadata()
            .expect("verified graph attachment must expose metadata");
        metadata.set("before-replacement", "visible").await.unwrap();

        replace_graph_database(&path);

        let telemetry_error = match code.telemetry_read_handle() {
            Ok(_) => panic!("telemetry must reject a replaced graph path"),
            Err(error) => error,
        };
        assert!(
            format!("{telemetry_error:?}").contains("identity changed"),
            "unexpected telemetry error: {telemetry_error:?}"
        );
        let (request, probe) = graph_quick_check_request(code.binding());
        let dispatch_error = code
            .dispatch_read(request, &probe)
            .expect_err("dispatch reads must reject a replaced graph path");
        assert!(
            format!("{dispatch_error:?}").contains("identity changed"),
            "unexpected dispatch error: {dispatch_error:?}"
        );
        let metadata_read_error = metadata
            .get("before-replacement")
            .await
            .expect_err("issued metadata readers must reject a replaced graph path");
        assert!(
            format!("{metadata_read_error:?}").contains("identity changed"),
            "unexpected metadata read error: {metadata_read_error:?}"
        );
        let metadata_write_error = metadata
            .set("after-replacement", "must-not-persist")
            .await
            .expect_err("issued metadata writers must reject a replaced graph path");
        assert!(
            format!("{metadata_write_error:?}").contains("identity changed"),
            "unexpected metadata write error: {metadata_write_error:?}"
        );
        assert!(
            code.graph_metadata().is_err(),
            "replacement must prevent issuing another graph capability"
        );

        drop(metadata);
        code.inner.attachment.drain().unwrap();
        assert!(code.physical_snapshot().is_drained());
        code.inner.attachment.close_and_join().unwrap();
        drop(profile);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_graph_metadata_write_rechecks_same_path_replacement_on_actor_dequeue() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("graph.db");
        create_graph_fixture_database_v1(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let (profile, code) = open_graph_runtime(path.clone()).await;
        let metadata = code
            .graph_metadata()
            .expect("verified graph attachment must expose metadata");
        let holder = code
            .authorized_migration_sql_handle(
                code.database_authority("hold graph writer before replacement")
                    .unwrap(),
            )
            .unwrap()
            .begin_immediate()
            .unwrap();
        let error = {
            let mut queued_write = Box::pin(metadata.set("queued-replacement", "must-not-persist"));
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut queued_write)
                    .await
                    .is_err(),
                "metadata write unexpectedly bypassed the occupied writer actor"
            );

            replace_graph_database(&path);
            holder.rollback().unwrap();

            queued_write
                .await
                .expect_err("queued graph write must recheck the opened file identity")
        };
        assert!(
            format!("{error:?}").contains("identity changed"),
            "unexpected queued replacement error: {error:?}"
        );
        drop(metadata);
        code.inner.attachment.drain().unwrap();
        assert!(code.physical_snapshot().is_drained());
        code.inner.attachment.close_and_join().unwrap();
        drop(profile);
    }
}
