use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use tracedecay_application::RequestId;
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, PromotionCasReceiptV1, RecoveryAuthorityExpectationV1,
    RemoteRecoveryCallerV1, RemoteRecoveryControlPortV1, RemoteRecoveryInterruptionV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_domain::{ManifestDigest, ProjectId, RemoteWriterFenceV1, canonical_sha256};
use tracedecay_runtime_core::storage::PrivateStoreIo;
use tracedecay_rusqlite_runtime::remote::{
    RemoteRecoveryPhysicalCommitV1, RemoteRecoveryPhysicalEffectErrorV1,
    RemoteRecoveryPhysicalEffectsV1, RemoteSqliteStorageV1,
};
use tracedecay_store::{
    RemoteWriterFenceInstallV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestProbeV1,
    StoreRuntimeBindingV1, StoreShardIdV1,
};

use super::{
    DatabaseAuthority, DestructiveMaintenanceTarget, LocalProfileIdentityAuthorityV1,
    LocalStoreRuntimeResolverV1, ProfileAuthorityPin, RegisteredGlobalDb, Result,
    StoreRuntimeRegistry, open_runtime, registry_open_error, session_registry_error,
};

const BACKUP_MANIFEST_VERSION: &str = "tracedecay.remote-backup.v1";
const CONTROL_POLL: Duration = Duration::from_millis(10);
const INTERRUPTION_NONE: u8 = 0;
const INTERRUPTION_CANCELLED: u8 = 1;
const INTERRUPTION_DEADLINE: u8 = 2;

mod artifacts;

use artifacts::{
    BackupSnapshotV1, RemoteBackupManifestV1, classify_runtime_error, converge_interrupted_restore,
    digest_bytes, digest_from_bytes, read_json_manifest, replay_current_authority_state,
    safe_digest_suffix, sha256_bytes, sha256_file, validate_isolated_restore,
};

#[derive(Clone)]
pub(super) struct RemoteRecoveryPublicationContextV1 {
    identity: LocalProfileIdentityAuthorityV1,
    incarnation: tracedecay_store::StoreIncarnationV1,
    resolver: Arc<LocalStoreRuntimeResolverV1>,
    registry: StoreRuntimeRegistry,
    profile_pin: ProfileAuthorityPin,
    project_sessions: Arc<tokio::sync::Mutex<BTreeMap<ProjectId, Arc<RegisteredGlobalDb>>>>,
    replay: Arc<crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1>,
}

impl RemoteRecoveryPublicationContextV1 {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        identity: LocalProfileIdentityAuthorityV1,
        incarnation: tracedecay_store::StoreIncarnationV1,
        resolver: Arc<LocalStoreRuntimeResolverV1>,
        registry: StoreRuntimeRegistry,
        profile_pin: ProfileAuthorityPin,
        project_sessions: Arc<tokio::sync::Mutex<BTreeMap<ProjectId, Arc<RegisteredGlobalDb>>>>,
        replay: Arc<
            crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1,
        >,
    ) -> Self {
        Self {
            identity,
            incarnation,
            resolver,
            registry,
            profile_pin,
            project_sessions,
            replay,
        }
    }

    async fn publish_restore(
        &self,
        project_id: ProjectId,
        staging: PathBuf,
        rollback: PathBuf,
        expected_binding: StoreRuntimeBindingV1,
        expected_staging_identity: u64,
        interruption: Arc<AtomicU8>,
    ) -> Result<RestorePublicationV1> {
        let mut mounted = Arc::clone(&self.project_sessions).lock_owned().await;
        let Some(database) = mounted.get(&project_id) else {
            return Ok(RestorePublicationV1::RolledBack);
        };
        if database.binding() != &expected_binding {
            return Ok(RestorePublicationV1::RolledBack);
        }
        if Arc::strong_count(database) != 1 {
            return Ok(RestorePublicationV1::RolledBack);
        }
        let destination = database.db_path().to_path_buf();
        let Some(root) = destination.parent() else {
            return Ok(RestorePublicationV1::RolledBack);
        };
        let target = match DestructiveMaintenanceTarget::new(root, [destination.clone()]) {
            Ok(target) => target,
            Err(_) => return Ok(RestorePublicationV1::RolledBack),
        };
        if self
            .replay
            .unregister_target(&project_id, &expected_binding)
            .is_err()
        {
            return Ok(RestorePublicationV1::RolledBack);
        }
        let database = mounted.remove(&project_id).ok_or_else(|| {
            session_registry_error(
                "publish remote restore",
                "ProjectSessions target disappeared during recovery".to_owned(),
            )
        })?;
        drop(database);

        let reservation = match self.registry.begin_destructive_maintenance(target).await {
            Ok(reservation) => reservation,
            Err(error) => {
                let restored = self
                    .mount_project_sessions(project_id.clone())
                    .await
                    .map_err(|remount| {
                        session_registry_error(
                            "recover failed remote restore quiesce",
                            format!("{error:?}; remount failed: {remount}"),
                        )
                    })?;
                self.publish_mounted(&mut mounted, project_id, restored)?;
                return Ok(RestorePublicationV1::RolledBack);
            }
        };
        let Some(closed) = reservation
            .closed()
            .iter()
            .find(|closed| closed.binding() == &expected_binding)
            .cloned()
        else {
            reservation.abort_preserved().map_err(|error| {
                session_registry_error(
                    "release incomplete remote restore quiesce",
                    format!("{error:?}"),
                )
            })?;
            let restored = self.mount_project_sessions(project_id.clone()).await?;
            self.publish_mounted(&mut mounted, project_id, restored)?;
            return Ok(RestorePublicationV1::RolledBack);
        };
        if let Err(error) = replay_current_authority_state(&destination, &staging) {
            reservation.abort_preserved().map_err(|release| {
                session_registry_error(
                    "release rejected remote restore",
                    format!("{error:?}; release failed: {release:?}"),
                )
            })?;
            let restored = self.mount_project_sessions(project_id.clone()).await?;
            self.publish_mounted(&mut mounted, project_id, restored)?;
            return Ok(RestorePublicationV1::RolledBack);
        }
        if interruption_value(&interruption).is_some() {
            reservation.abort_preserved().map_err(|error| {
                session_registry_error("cancel remote restore publication", format!("{error:?}"))
            })?;
            let restored = self.mount_project_sessions(project_id.clone()).await?;
            self.publish_mounted(&mut mounted, project_id, restored)?;
            return Ok(RestorePublicationV1::RolledBack);
        }

        let publication = DatabaseAuthority::replace_sqlite_with_rollback_atomically(
            &staging,
            &destination,
            &rollback,
            closed.opened_file_identity(),
            expected_staging_identity,
        );
        match publication {
            Ok(()) => {
                PrivateStoreIo::sync_sqlite_family(&destination).map_err(|error| {
                    session_registry_error("sync published remote restore", error.to_string())
                })?;
                PrivateStoreIo::sync_sqlite_family(&rollback).map_err(|error| {
                    session_registry_error("sync remote restore rollback", error.to_string())
                })?;
                reservation.finish_deleted().map_err(|error| {
                    session_registry_error("release published remote restore", format!("{error:?}"))
                })?;
            }
            Err(error) => {
                let destination_identity =
                    tracedecay_runtime_core::db::sqlite_generation_identity(&destination).ok();
                if destination_identity == Some(closed.opened_file_identity()) {
                    reservation.abort_preserved().map_err(|release| {
                        session_registry_error(
                            "release rolled-back remote restore",
                            format!("{release:?}"),
                        )
                    })?;
                    let restored = self.mount_project_sessions(project_id.clone()).await?;
                    self.publish_mounted(&mut mounted, project_id, restored)?;
                    return Ok(RestorePublicationV1::RolledBack);
                }
                return Err(session_registry_error(
                    "publish remote restore",
                    format!("atomic publication requires forward recovery: {error}"),
                ));
            }
        }

        match self.mount_project_sessions(project_id.clone()).await {
            Ok(restored) => {
                self.publish_mounted(&mut mounted, project_id, restored)?;
                Ok(RestorePublicationV1::Published)
            }
            Err(publication_error) => {
                self.rollback_published_restore(
                    &project_id,
                    &destination,
                    &rollback,
                    expected_staging_identity,
                    closed.opened_file_identity(),
                )
                .await?;
                let restored = self.mount_project_sessions(project_id.clone()).await?;
                self.publish_mounted(&mut mounted, project_id, restored)?;
                tracing::warn!(
                    error = %publication_error,
                    "restored database failed registered reattach and was rolled back"
                );
                Ok(RestorePublicationV1::RolledBack)
            }
        }
    }

    async fn rollback_published_restore(
        &self,
        project_id: &ProjectId,
        destination: &Path,
        rollback: &Path,
        expected_published_identity: u64,
        expected_rollback_identity: u64,
    ) -> Result<()> {
        let retained_new = destination.with_extension(format!(
            "remote-restore-rejected-{expected_published_identity:016x}.sqlite3"
        ));
        let target = DestructiveMaintenanceTarget::new(
            destination.parent().ok_or_else(|| {
                session_registry_error(
                    "rollback remote restore",
                    "published target has no parent directory".to_owned(),
                )
            })?,
            [destination.to_path_buf()],
        )
        .map_err(|error| {
            session_registry_error("reserve remote restore rollback", format!("{error:?}"))
        })?;
        let reservation = self
            .registry
            .begin_destructive_maintenance(target)
            .await
            .map_err(|error| {
                session_registry_error("quiesce remote restore rollback", format!("{error:?}"))
            })?;
        DatabaseAuthority::replace_sqlite_with_rollback_atomically(
            rollback,
            destination,
            &retained_new,
            expected_published_identity,
            expected_rollback_identity,
        )?;
        PrivateStoreIo::sync_sqlite_family(destination).map_err(|error| {
            session_registry_error("sync rolled-back remote restore", error.to_string())
        })?;
        reservation.finish_deleted().map_err(|error| {
            session_registry_error("release remote restore rollback", format!("{error:?}"))
        })?;
        tracing::info!(
            project_id = %project_id,
            retained_rejected_store = %retained_new.display(),
            "remote restore rolled back after registered validation failed"
        );
        Ok(())
    }

    async fn mount_project_sessions(
        &self,
        project_id: ProjectId,
    ) -> Result<Arc<RegisteredGlobalDb>> {
        let shard_id = StoreShardIdV1::project_sessions(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
        );
        let runtime = open_runtime(
            &self.registry,
            self.resolver.as_ref(),
            shard_id,
            self.incarnation,
            Some(self.profile_pin.clone()),
            None,
            false,
            "reattach restored project session store",
        )
        .await?;
        let expected_binding = runtime.binding().clone();
        let expected_locator = runtime.locator().verified().clone();
        let authority = runtime
            .database_authority("reattach restored project session store")
            .map_err(|error| {
                registry_open_error("reattach restored project session store", error)
            })?;
        let database = RegisteredGlobalDb::migrate_and_attach(
            runtime,
            expected_binding,
            expected_locator,
            authority,
        )
        .await?;
        Ok(Arc::new(database))
    }

    fn publish_mounted(
        &self,
        mounted: &mut BTreeMap<ProjectId, Arc<RegisteredGlobalDb>>,
        project_id: ProjectId,
        database: Arc<RegisteredGlobalDb>,
    ) -> Result<()> {
        let runtime = database.runtime().clone();
        let authority = runtime
            .database_authority("register restored remote replay target")
            .map_err(|error| {
                registry_open_error("register restored remote replay target", error)
            })?;
        self.replay
            .register_target(project_id.clone(), runtime, authority)
            .map_err(|error| session_registry_error("register restored replay target", error))?;
        mounted.insert(project_id, database);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestorePublicationV1 {
    Published,
    RolledBack,
}

#[derive(Clone)]
pub(super) struct DaemonRemoteRecoveryPhysicalEffectsV1 {
    storage: RemoteSqliteStorageV1,
    backup_root: PathBuf,
    replay: Arc<crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1>,
    publication: RemoteRecoveryPublicationContextV1,
    runtime: tokio::runtime::Handle,
}

impl DaemonRemoteRecoveryPhysicalEffectsV1 {
    pub(super) fn new(
        storage: RemoteSqliteStorageV1,
        backup_root: PathBuf,
        replay: Arc<
            crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1,
        >,
        publication: RemoteRecoveryPublicationContextV1,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            storage,
            backup_root,
            replay,
            publication,
            runtime,
        }
    }

    fn resolve_project(
        &self,
        expected: &RecoveryAuthorityExpectationV1,
        caller: &RemoteRecoveryCallerV1,
    ) -> std::result::Result<ProjectId, RemoteRecoveryPhysicalEffectErrorV1> {
        let writer = self
            .storage
            .recovery_writer(expected)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        if writer.scope != caller.scope {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
        }
        writer
            .target_project_id()
            .cloned()
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)
    }
}

impl RemoteRecoveryPhysicalEffectsV1 for DaemonRemoteRecoveryPhysicalEffectsV1 {
    fn current_authority(
        &self,
        expected: &RecoveryAuthorityExpectationV1,
        caller: &RemoteRecoveryCallerV1,
    ) -> std::result::Result<
        (tracedecay_domain::CurrentRemoteAuthorityV1, u64),
        RemoteRecoveryPhysicalEffectErrorV1,
    > {
        let writer = self
            .storage
            .recovery_writer(expected)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        if writer.scope != caller.scope {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
        }
        let project_id = writer
            .target_project_id()
            .cloned()
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        let authority_key = authority_key(expected)?;
        match self
            .replay
            .current_writer_fence(project_id, authority_key)
            .map_err(classify_runtime_error)?
        {
            Some((fence, frontier)) if fence == writer.authority.fence => {
                Ok((writer.authority, frontier))
            }
            _ => Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption),
        }
    }

    fn required_promotion_sink_ids(
        &self,
        expected: &RecoveryAuthorityExpectationV1,
    ) -> std::result::Result<Vec<String>, RemoteRecoveryPhysicalEffectErrorV1> {
        let authority_key = authority_key(expected)?;
        Ok(vec![
            format!("remote-node.recovery-journal.{}", authority_key.as_str()),
            format!("remote-node.writer-authority.{}", authority_key.as_str()),
            format!("project-sessions.writer-fence.{}", authority_key.as_str()),
        ])
    }

    fn create_backup(
        &self,
        operation_id: &str,
        expected: &RecoveryAuthorityExpectationV1,
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
        request_id: &RequestId,
    ) -> std::result::Result<
        RemoteRecoveryPhysicalCommitV1<BackupOperationStateV1>,
        RemoteRecoveryPhysicalEffectErrorV1,
    > {
        let project_id = self.resolve_project(expected, caller)?;
        let policy_digest = self
            .storage
            .recovery_policy_digest(&caller.scope)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        let (binding, _) = self
            .replay
            .target_descriptor(&project_id)
            .map_err(classify_runtime_error)?;
        let backup_id = backup_id(operation_id, expected)?;
        let database_path = self.backup_root.join(format!("{backup_id}.sqlite3"));
        let manifest_path = self.backup_root.join(format!("{backup_id}.manifest.json"));
        if manifest_path.exists() {
            return load_existing_backup(
                &manifest_path,
                &database_path,
                &backup_id,
                expected,
                &policy_digest,
                &project_id,
                &binding.shard_id,
            );
        }
        PrivateStoreIo::create_dir_all(&self.backup_root)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        let interruption = Arc::new(AtomicU8::new(INTERRUPTION_NONE));
        if database_path.exists() {
            let identity = tracedecay_runtime_core::db::sqlite_generation_identity(&database_path)
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
            let interrupted =
                database_path.with_extension(format!("interrupted-{identity:016x}.sqlite3"));
            if interrupted.exists() {
                return Err(RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired);
            }
            DatabaseAuthority::replace_file_atomically(
                &database_path,
                &interrupted,
                "interrupted remote backup",
            )
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired)?;
            PrivateStoreIo::sync_sqlite_family(&interrupted)
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired)?;
        }
        let probe = Arc::new(RecoveryRuntimeProbeV1::new(
            request_id,
            Arc::clone(&interruption),
        )?);
        let replay = Arc::clone(&self.replay);
        let snapshot_path = database_path.clone();
        let snapshot_project_id = project_id.clone();
        let receipt = run_controlled(control, request_id, &interruption, move || {
            replay.snapshot_target(snapshot_project_id, snapshot_path, probe)
        })?
        .map_err(classify_runtime_error)?;
        let snapshot = BackupSnapshotV1 {
            source_watermark: receipt.source_watermark,
            destination_bytes: receipt.destination_bytes,
            destination_sha256: receipt.destination_sha256.0,
        };
        let committed_at = tracedecay_application::clock::now_micros();
        let manifest = RemoteBackupManifestV1 {
            version: BACKUP_MANIFEST_VERSION.to_owned(),
            backup_id: backup_id.clone(),
            expected: expected.clone(),
            policy_digest: policy_digest.clone(),
            project_id,
            source_shard: binding.shard_id,
            destination_bytes: snapshot.destination_bytes,
            destination_sha256: snapshot.destination_sha256,
            source_watermark: snapshot.source_watermark,
            committed_at,
        };
        let manifest_bytes = serde_json::to_vec(&manifest)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        let manifest_digest = sha256_bytes(&manifest_bytes);
        let manifest_temp = self
            .backup_root
            .join(format!(".{backup_id}.manifest.staging"));
        PrivateStoreIo::write_file_atomically_durable(
            &manifest_path,
            &manifest_temp,
            &manifest_bytes,
        )
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        let committed_state_digest = digest_from_bytes(manifest_digest)?;
        Ok(RemoteRecoveryPhysicalCommitV1 {
            output: BackupOperationStateV1::Available {
                backup_id,
                manifest_digest,
            },
            policy_digest,
            committed_state_digest,
            committed_at,
            units_consumed: 1,
            bytes_consumed: snapshot
                .destination_bytes
                .saturating_add(manifest_bytes.len() as u64),
            interruption_observed_after_commit: interruption_value(&interruption),
        })
    }

    fn publish_staged_restore(
        &self,
        request: &StagedRestoreConfirmationV1,
        expected: &RecoveryAuthorityExpectationV1,
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
        request_id: &RequestId,
    ) -> std::result::Result<
        RemoteRecoveryPhysicalCommitV1<StagedRestoreProgressV1>,
        RemoteRecoveryPhysicalEffectErrorV1,
    > {
        let project_id = self.resolve_project(expected, caller)?;
        let policy_digest = self
            .storage
            .recovery_policy_digest(&caller.scope)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        if digest_bytes(&policy_digest)? != request.expected_policy_digest {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
        }
        let manifest_path = self
            .backup_root
            .join(format!("{}.manifest.json", request.backup_id));
        let backup_path = self
            .backup_root
            .join(format!("{}.sqlite3", request.backup_id));
        let (binding, destination) = self
            .replay
            .target_descriptor(&project_id)
            .map_err(classify_runtime_error)?;
        let manifest = read_json_manifest(&manifest_path)?;
        validate_manifest(
            &manifest,
            &backup_path,
            &request.backup_id,
            expected,
            &policy_digest,
            &project_id,
            &binding.shard_id,
        )?;
        if sha256_file(&manifest_path)? != request.manifest_digest {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
        }
        validate_isolated_restore(&backup_path)?;

        let suffix = safe_suffix(&request.preview_id)?;
        let staging = destination.with_extension(format!("remote-restore-{suffix}.staging"));
        let rollback = destination.with_extension(format!("remote-restore-{suffix}.rollback"));
        if converge_interrupted_restore(
            &destination,
            &staging,
            &rollback,
            manifest.destination_sha256,
        )? {
            return committed_restore(request, policy_digest, manifest.destination_bytes, None);
        }
        if rollback.exists() {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired);
        }
        if !staging.exists() {
            PrivateStoreIo::copy_artifact(&backup_path, &staging)
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
            PrivateStoreIo::sync_sqlite_family(&staging)
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        }
        validate_isolated_restore(&staging)?;
        let staging_identity = tracedecay_runtime_core::db::sqlite_generation_identity(&staging)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        let interruption = Arc::new(AtomicU8::new(INTERRUPTION_NONE));
        let publication = self.publication.clone();
        let runtime = self.runtime.clone();
        let project_for_publish = project_id.clone();
        let binding_for_publish = binding.clone();
        let staging_for_publish = staging.clone();
        let rollback_for_publish = rollback.clone();
        let publication_interruption = Arc::clone(&interruption);
        let result = run_controlled(control, request_id, &interruption, move || {
            runtime.block_on(publication.publish_restore(
                project_for_publish,
                staging_for_publish,
                rollback_for_publish,
                binding_for_publish,
                staging_identity,
                publication_interruption,
            ))
        })?;
        match result {
            Ok(RestorePublicationV1::Published) => committed_restore(
                request,
                policy_digest,
                manifest.destination_bytes,
                interruption_value(&interruption),
            ),
            Ok(RestorePublicationV1::RolledBack) => {
                Err(RemoteRecoveryPhysicalEffectErrorV1::RolledBack)
            }
            Err(_) => Err(RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired),
        }
    }

    fn promote(
        &self,
        operation_id: &str,
        expected: &RecoveryAuthorityExpectationV1,
        replacement: &RemoteWriterFenceV1,
        required_sink_ids: &[String],
        caller: &RemoteRecoveryCallerV1,
        control: &dyn RemoteRecoveryControlPortV1,
        request_id: &RequestId,
    ) -> std::result::Result<
        RemoteRecoveryPhysicalCommitV1<PromotionCasReceiptV1>,
        RemoteRecoveryPhysicalEffectErrorV1,
    > {
        let writer = self
            .storage
            .recovery_writer_for_lineage(expected)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        if writer.scope != caller.scope {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
        }
        let project_id = writer
            .target_project_id()
            .cloned()
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        let expected_sinks = self.required_promotion_sink_ids(expected)?;
        if expected_sinks != required_sink_ids {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
        }
        let policy_digest = self
            .storage
            .recovery_policy_digest(&caller.scope)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        let authority_key = authority_key(expected)?;
        let current = remote_fence(expected)?;
        let installed_at = tracedecay_application::clock::now_micros();
        let (binding, _) = self
            .replay
            .target_descriptor(&project_id)
            .map_err(classify_runtime_error)?;
        let install = RemoteWriterFenceInstallV1 {
            project_id: project_id.clone(),
            target_binding: binding,
            authority_key: authority_key.clone(),
            expected: current,
            replacement: replacement.clone(),
            installed_at,
        };
        let interruption = Arc::new(AtomicU8::new(INTERRUPTION_NONE));
        let probe = Arc::new(RecoveryRuntimeProbeV1::new(
            request_id,
            Arc::clone(&interruption),
        )?);
        let replay = Arc::clone(&self.replay);
        let project_for_install = project_id.clone();
        let receipt = run_controlled(control, request_id, &interruption, move || {
            replay.install_writer_fence(project_for_install, install, probe)
        })?
        .map_err(classify_runtime_error)?;
        let (_, published_frontier_sequence) = self
            .replay
            .current_writer_fence(project_id, authority_key)
            .map_err(classify_runtime_error)?
            .filter(|(fence, _)| fence == replacement)
            .ok_or(RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        let receipt_id = format!("remote.promotion.{}", safe_suffix(operation_id)?);
        let output = PromotionCasReceiptV1 {
            receipt_id,
            preview_id: operation_id.to_owned(),
            previous_epoch: expected.authority_epoch,
            installed_epoch: replacement.authority_epoch.0,
            installed_placement_revision: replacement.placement_revision.get(),
            installed_sink_ids: required_sink_ids.to_vec(),
            published_frontier_sequence,
            old_authority_fenced: true,
        };
        let bytes_consumed = u64::try_from(
            serde_json::to_vec(&(&output, &receipt))
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?
                .len(),
        )
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?
        .max(1);
        Ok(RemoteRecoveryPhysicalCommitV1 {
            committed_state_digest: canonical_sha256(&(&output, &receipt))
                .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
            output,
            policy_digest,
            committed_at: receipt.committed_at,
            units_consumed: 1,
            bytes_consumed,
            interruption_observed_after_commit: interruption_value(&interruption),
        })
    }
}

fn load_existing_backup(
    manifest_path: &Path,
    database_path: &Path,
    backup_id: &str,
    expected: &RecoveryAuthorityExpectationV1,
    policy_digest: &ManifestDigest,
    project_id: &ProjectId,
    source_shard: &StoreShardIdV1,
) -> std::result::Result<
    RemoteRecoveryPhysicalCommitV1<BackupOperationStateV1>,
    RemoteRecoveryPhysicalEffectErrorV1,
> {
    let manifest = read_json_manifest(manifest_path)?;
    validate_manifest(
        &manifest,
        database_path,
        backup_id,
        expected,
        policy_digest,
        project_id,
        source_shard,
    )?;
    let manifest_digest = sha256_file(manifest_path)?;
    Ok(RemoteRecoveryPhysicalCommitV1 {
        output: BackupOperationStateV1::Available {
            backup_id: backup_id.to_owned(),
            manifest_digest,
        },
        policy_digest: policy_digest.clone(),
        committed_state_digest: digest_from_bytes(manifest_digest)?,
        committed_at: manifest.committed_at,
        units_consumed: 1,
        bytes_consumed: manifest.destination_bytes,
        interruption_observed_after_commit: None,
    })
}

fn validate_manifest(
    manifest: &RemoteBackupManifestV1,
    database_path: &Path,
    backup_id: &str,
    expected: &RecoveryAuthorityExpectationV1,
    policy_digest: &ManifestDigest,
    project_id: &ProjectId,
    source_shard: &StoreShardIdV1,
) -> std::result::Result<(), RemoteRecoveryPhysicalEffectErrorV1> {
    if manifest.version != BACKUP_MANIFEST_VERSION
        || manifest.backup_id != backup_id
        || &manifest.expected != expected
        || &manifest.policy_digest != policy_digest
        || &manifest.project_id != project_id
        || &manifest.source_shard != source_shard
        || manifest.destination_bytes == 0
        || sha256_file(database_path)? != manifest.destination_sha256
    {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    Ok(())
}

fn committed_restore(
    request: &StagedRestoreConfirmationV1,
    policy_digest: ManifestDigest,
    bytes_consumed: u64,
    interruption: Option<RemoteRecoveryInterruptionV1>,
) -> std::result::Result<
    RemoteRecoveryPhysicalCommitV1<StagedRestoreProgressV1>,
    RemoteRecoveryPhysicalEffectErrorV1,
> {
    let receipt_id = format!("remote.restore.{}", safe_suffix(&request.preview_id)?);
    let output = StagedRestoreProgressV1::Published { receipt_id };
    Ok(RemoteRecoveryPhysicalCommitV1 {
        committed_state_digest: canonical_sha256(&output)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
        output,
        policy_digest,
        committed_at: tracedecay_application::clock::now_micros(),
        units_consumed: 1,
        bytes_consumed,
        interruption_observed_after_commit: interruption,
    })
}

fn run_controlled<T: Send>(
    control: &dyn RemoteRecoveryControlPortV1,
    request_id: &RequestId,
    interruption: &Arc<AtomicU8>,
    operation: impl FnOnce() -> T + Send,
) -> std::result::Result<T, RemoteRecoveryPhysicalEffectErrorV1> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            if sender.send(operation()).is_err() {
                tracing::debug!("remote recovery caller ended before physical effect reply");
            }
        });
        loop {
            match receiver.recv_timeout(CONTROL_POLL) {
                Ok(result) => {
                    observe_control(control, request_id, interruption);
                    return Ok(result);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    observe_control(control, request_id, interruption);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(RemoteRecoveryPhysicalEffectErrorV1::Unavailable);
                }
            }
        }
    })
}

fn observe_control(
    control: &dyn RemoteRecoveryControlPortV1,
    request_id: &RequestId,
    interruption: &Arc<AtomicU8>,
) {
    let value = match control.interruption(request_id) {
        Some(RemoteRecoveryInterruptionV1::Cancelled) => INTERRUPTION_CANCELLED,
        Some(RemoteRecoveryInterruptionV1::DeadlineExceeded) => INTERRUPTION_DEADLINE,
        None => INTERRUPTION_NONE,
    };
    if value != INTERRUPTION_NONE {
        match interruption.compare_exchange(
            INTERRUPTION_NONE,
            value,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) | Err(INTERRUPTION_CANCELLED) | Err(INTERRUPTION_DEADLINE) => {}
            Err(_) => tracing::warn!("remote recovery interruption state is invalid"),
        }
    }
}

fn interruption_value(interruption: &Arc<AtomicU8>) -> Option<RemoteRecoveryInterruptionV1> {
    match interruption.load(Ordering::Acquire) {
        INTERRUPTION_CANCELLED => Some(RemoteRecoveryInterruptionV1::Cancelled),
        INTERRUPTION_DEADLINE => Some(RemoteRecoveryInterruptionV1::DeadlineExceeded),
        _ => None,
    }
}

struct RecoveryRuntimeProbeV1 {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: Arc<AtomicU8>,
    commit_started: AtomicBool,
}

impl RecoveryRuntimeProbeV1 {
    fn new(
        request_id: &RequestId,
        interruption: Arc<AtomicU8>,
    ) -> std::result::Result<Self, RemoteRecoveryPhysicalEffectErrorV1> {
        let digest = canonical_sha256(&("tracedecay.remote-recovery-control.v1", request_id))
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        let suffix = digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        Ok(Self {
            cancellation: RuntimeCancellationIdentityV1 {
                cancellation_id: RuntimeCancellationIdV1::new(format!("cancellation.{suffix}"))
                    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
                generation: 1,
            },
            deadline: RuntimeDeadlineV1 {
                deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{suffix}"))
                    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
            },
            interruption,
            commit_started: AtomicBool::new(false),
        })
    }
}

impl RuntimeRequestProbeV1 for RecoveryRuntimeProbeV1 {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        match interruption_value(&self.interruption) {
            Some(RemoteRecoveryInterruptionV1::Cancelled) => Some(RuntimeInterruptionV1::Cancelled),
            Some(RemoteRecoveryInterruptionV1::DeadlineExceeded) => {
                Some(RuntimeInterruptionV1::DeadlineExceeded)
            }
            None => None,
        }
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
            && self
                .commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}

fn authority_key(
    expected: &RecoveryAuthorityExpectationV1,
) -> std::result::Result<ManifestDigest, RemoteRecoveryPhysicalEffectErrorV1> {
    canonical_sha256(&(
        "tracedecay.remote-recovery-authority.v1",
        &expected.brain_id,
        &expected.shard_id,
        &expected.generation_id,
    ))
    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)
}

fn backup_id(
    operation_id: &str,
    expected: &RecoveryAuthorityExpectationV1,
) -> std::result::Result<String, RemoteRecoveryPhysicalEffectErrorV1> {
    let digest = canonical_sha256(&("tracedecay.remote-backup.v1", operation_id, expected))
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    Ok(format!("remote.backup.{}", safe_digest_suffix(&digest)?))
}

fn remote_fence(
    expected: &RecoveryAuthorityExpectationV1,
) -> std::result::Result<RemoteWriterFenceV1, RemoteRecoveryPhysicalEffectErrorV1> {
    Ok(RemoteWriterFenceV1 {
        brain_id: tracedecay_domain::BrainId::new(expected.brain_id.clone())
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
        shard_id: tracedecay_domain::ShardId::new(expected.shard_id.clone())
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
        generation_id: tracedecay_domain::ProjectionGenerationId::new(
            expected.generation_id.clone(),
        )
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
        placement_revision: tracedecay_domain::RemotePlacementRevisionV1::new(
            expected.placement_revision,
        )
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
        authority_epoch: tracedecay_domain::AuthorityEpoch(expected.authority_epoch),
        authority_node_id: tracedecay_domain::BrainNodeId::new(expected.authority_node_id.clone())
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?,
    })
}

fn safe_suffix(value: &str) -> std::result::Result<&str, RemoteRecoveryPhysicalEffectErrorV1> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    Ok(value)
}
