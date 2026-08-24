use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock, Weak, mpsc};
use std::time::Duration;

use tracedecay_application::RequestId;
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, PromotionCasReceiptV1, RecoveryAuthorityExpectationV1,
    RemoteRecoveryCallerV1, RemoteRecoveryControlPortV1, RemoteRecoveryInterruptionV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_domain::{ManifestDigest, ProjectId, RemoteWriterFenceV1, canonical_sha256};
use tracedecay_graph_db::GraphDbRegistry;
use tracedecay_runtime_core::storage::PrivateStoreIo;
use tracedecay_rusqlite_runtime::remote::{
    RemoteRecoveryPhysicalCommitV1, RemoteRecoveryPhysicalEffectErrorV1,
    RemoteRecoveryPhysicalEffectsV1, RemoteSqliteStorageV1,
};
use tracedecay_store::{RemoteWriterFenceInstallV1, StoreShardIdV1};

use super::{
    LocalProfileIdentityAuthorityV1, LocalStoreLocatorResolutionV1, LocalStoreRuntimeResolverV1,
    ProfileAuthorityPin, ProjectRuntimeOwnerRegistryV1, Result, StoreRuntimeKey,
    StoreRuntimeRegistry, session_registry_error,
};
use crate::db::DatabaseAuthority;
use crate::global_db::RegisteredGlobalDbLeaseV1;

const BACKUP_MANIFEST_VERSION: &str = "tracedecay.remote-backup.v1";
const CONTROL_POLL: Duration = Duration::from_millis(10);
const INTERRUPTION_NONE: u8 = 0;
const INTERRUPTION_CANCELLED: u8 = 1;
const INTERRUPTION_DEADLINE: u8 = 2;

mod artifacts;
mod publication;
mod support;

use publication::RestorePublicationV1;
pub(in crate::daemon::store_runtime::session_registry) use publication::remote_restore_activated_open_identity;

use artifacts::{
    BackupSnapshotV1, RemoteBackupManifestV1, classify_runtime_error, converge_interrupted_restore,
    digest_bytes, digest_from_bytes, read_json_manifest, sha256_bytes, sha256_file,
    sqlite_identity, validate_isolated_restore,
};
use support::{
    RecoveryRuntimeProbeV1, authority_key, backup_id, committed_restore,
    validate_recovery_artifact_file,
};

#[derive(Clone)]
pub(super) struct RemoteRecoveryPublicationContextV1 {
    identity: LocalProfileIdentityAuthorityV1,
    incarnation: tracedecay_store::StoreIncarnationV1,
    resolver: Arc<LocalStoreRuntimeResolverV1>,
    registry: StoreRuntimeRegistry,
    graph_registry: GraphDbRegistry,
    graph_lifecycle_cancelled: Arc<AtomicBool>,
    profile_pin: ProfileAuthorityPin,
    project_owners: ProjectRuntimeOwnerRegistryV1,
    replay: Arc<crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1>,
    session_sync_service:
        Arc<OnceLock<Weak<crate::daemon::session_sync::DaemonSessionSyncService>>>,
    project_lifecycle: Arc<
        OnceLock<
            Weak<
                crate::daemon::branch_admin::remote_recovery_lifecycle::RemoteRecoveryProjectLifecycleV1,
            >,
        >,
    >,
}

impl RemoteRecoveryPublicationContextV1 {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        identity: LocalProfileIdentityAuthorityV1,
        incarnation: tracedecay_store::StoreIncarnationV1,
        resolver: Arc<LocalStoreRuntimeResolverV1>,
        registry: StoreRuntimeRegistry,
        graph_registry: GraphDbRegistry,
        graph_lifecycle_cancelled: Arc<AtomicBool>,
        profile_pin: ProfileAuthorityPin,
        project_owners: ProjectRuntimeOwnerRegistryV1,
        replay: Arc<
            crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1,
        >,
        session_sync_service: Arc<
            OnceLock<Weak<crate::daemon::session_sync::DaemonSessionSyncService>>,
        >,
        project_lifecycle: Arc<
            OnceLock<
                Weak<
                    crate::daemon::branch_admin::remote_recovery_lifecycle::RemoteRecoveryProjectLifecycleV1,
                >,
            >,
        >,
    ) -> Self {
        Self {
            identity,
            incarnation,
            resolver,
            registry,
            graph_registry,
            graph_lifecycle_cancelled,
            profile_pin,
            project_owners,
            replay,
            session_sync_service,
            project_lifecycle,
        }
    }

    fn session_sync_service(
        &self,
        operation: &'static str,
    ) -> Result<Arc<crate::daemon::session_sync::DaemonSessionSyncService>> {
        self.session_sync_service
            .get()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| {
                session_registry_error(
                    operation,
                    "session sync lifecycle authority is unavailable".to_owned(),
                )
            })
    }

    fn project_lifecycle(
        &self,
    ) -> Result<Arc<crate::daemon::branch_admin::remote_recovery_lifecycle::RemoteRecoveryProjectLifecycleV1>>
    {
        self.project_lifecycle
            .get()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| {
                session_registry_error(
                    "authorize remote project recovery",
                    "remote recovery project lifecycle is unavailable".to_owned(),
                )
            })
    }

    async fn retire_project_session_sync(&self, project_id: &ProjectId) -> Result<()> {
        self.session_sync_service("retire remote recovery project session sync")?
            .retire_project(self.identity.profile_id(), project_id)
            .await
            .map(|_| ())
            .map_err(|error| {
                session_registry_error("retire remote recovery project session sync", error)
            })
    }

    async fn rebind_project_session_sync(
        &self,
        project_id: &ProjectId,
        database: &RegisteredGlobalDbLeaseV1,
    ) -> Result<()> {
        self.session_sync_service("rebind remote recovery project session sync")?
            .rebind_project(self.identity.profile_id(), project_id, database)
            .await
            .map(|_| ())
            .map_err(|error| {
                session_registry_error("rebind remote recovery project session sync", error)
            })
    }

    async fn authorize_project_recovery(
        &self,
        project_id: &ProjectId,
    ) -> Result<crate::daemon::store_writer_gate::WriterAdmissionGuard> {
        self.project_lifecycle()?
            .authorize_project_recovery(project_id)
            .await
    }
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
        let _recovery_admission = self
            .runtime
            .block_on(self.publication.authorize_project_recovery(&project_id))
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
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
        let _recovery_admission = self
            .runtime
            .block_on(self.publication.authorize_project_recovery(&project_id))
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
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
            validate_recovery_artifact_file(&self.backup_root, &manifest_path)?;
            validate_recovery_artifact_file(&self.backup_root, &database_path)?;
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
            let identity = sqlite_identity(&database_path)?;
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
        let _recovery_admission = self
            .runtime
            .block_on(self.publication.authorize_project_recovery(&project_id))
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        let policy_digest = self
            .storage
            .recovery_policy_digest(&caller.scope)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        if digest_bytes(&policy_digest)? != request.expected_policy_digest {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
        }
        let backup_id = safe_suffix(&request.backup_id)?;
        let manifest_path = self.backup_root.join(format!("{backup_id}.manifest.json"));
        let backup_path = self.backup_root.join(format!("{backup_id}.sqlite3"));
        validate_recovery_artifact_file(&self.backup_root, &manifest_path)?;
        validate_recovery_artifact_file(&self.backup_root, &backup_path)?;
        let manifest = read_json_manifest(&manifest_path)?;
        let expected_shard = StoreShardIdV1::project_sessions(
            self.publication.identity.brain_id().clone(),
            self.publication.identity.profile_id().clone(),
            project_id.clone(),
        );
        let destination = match self.publication.resolver.resolve_key(&StoreRuntimeKey::new(
            expected_shard.clone(),
            self.publication.incarnation,
        )) {
            LocalStoreLocatorResolutionV1::Resolved(locator) => {
                locator.locator().path().to_path_buf()
            }
            LocalStoreLocatorResolutionV1::Unavailable(_) => {
                return Err(RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired);
            }
        };
        validate_manifest(
            &manifest,
            &backup_path,
            &request.backup_id,
            expected,
            &policy_digest,
            &project_id,
            &expected_shard,
        )?;
        if sha256_file(&manifest_path)? != request.manifest_digest {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
        }
        validate_isolated_restore(&backup_path)?;

        let suffix = safe_suffix(&request.preview_id)?;
        let staging = destination.with_extension(format!("remote-restore-{suffix}.staging"));
        let rollback = destination.with_extension(format!("remote-restore-{suffix}.rollback"));
        if let Some(outcome) = self
            .runtime
            .block_on(self.publication.resume_quarantined_restore_while_admitted(
                project_id.clone(),
                &destination,
                &rollback,
            ))
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired)?
        {
            return match outcome {
                RestorePublicationV1::Published => {
                    committed_restore(request, policy_digest, manifest.destination_bytes, None)
                }
                RestorePublicationV1::RolledBack => {
                    Err(RemoteRecoveryPhysicalEffectErrorV1::RolledBack)
                }
            };
        }
        let destination_matches_restore =
            sha256_file(&destination).is_ok_and(|digest| digest == manifest.destination_sha256);
        if self
            .runtime
            .block_on(self.publication.resume_retained_rollback(
                project_id.clone(),
                &destination,
                &rollback,
                destination_matches_restore,
            ))
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired)?
        {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::RolledBack);
        }
        if converge_interrupted_restore(
            &destination,
            &staging,
            &rollback,
            manifest.destination_sha256,
        )? {
            let converged_identity = sqlite_identity(&destination)?;
            if self
                .runtime
                .block_on(
                    self.publication
                        .ensure_project_sessions_target_while_admitted(
                            project_id.clone(),
                            converged_identity,
                            &destination,
                        ),
                )
                .is_err()
            {
                let published_identity = sqlite_identity(&destination)?;
                let rollback_identity = sqlite_identity(&rollback)?;
                self.runtime
                    .block_on(self.publication.rollback_published_restore(
                        &project_id,
                        &destination,
                        &rollback,
                        published_identity,
                        rollback_identity,
                    ))
                    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired)?;
                self.runtime
                    .block_on(
                        self.publication
                            .ensure_project_sessions_target_while_admitted(
                                project_id.clone(),
                                rollback_identity,
                                &destination,
                            ),
                    )
                    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired)?;
                return Err(RemoteRecoveryPhysicalEffectErrorV1::RolledBack);
            }
            return committed_restore(request, policy_digest, manifest.destination_bytes, None);
        }
        let current_destination_identity = sqlite_identity(&destination)?;
        self.runtime
            .block_on(
                self.publication
                    .ensure_project_sessions_target_while_admitted(
                        project_id.clone(),
                        current_destination_identity,
                        &destination,
                    ),
            )
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired)?;
        let (binding, mounted_destination) = self
            .replay
            .target_descriptor(&project_id)
            .map_err(classify_runtime_error)?;
        if binding.shard_id != expected_shard || mounted_destination != destination {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
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
        let staging_identity = sqlite_identity(&staging)?;
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
        let _recovery_admission = self
            .runtime
            .block_on(self.publication.authorize_project_recovery(&project_id))
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
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
            Ok(_) | Err(INTERRUPTION_CANCELLED | INTERRUPTION_DEADLINE) => {}
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
