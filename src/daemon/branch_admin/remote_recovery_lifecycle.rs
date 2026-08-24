use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
use tracedecay_store::StoreShardScopeV1;

use super::{
    DatabaseOwnerRegistry, StoreAdministration, StoreWriterClass, StoreWriterGates, WriterScope,
};
use crate::daemon::store_writer_gate::WriterAdmissionGuard;
use crate::errors::{Result, TraceDecayError};

pub(in crate::daemon) struct RemoteRecoveryProjectLifecycleV1 {
    brain_id: BrainId,
    profile_id: UserProfileId,
    profile_root: PathBuf,
    gate: Arc<StoreWriterGates>,
    project_servers: Arc<tokio::sync::Mutex<DatabaseOwnerRegistry>>,
    session_runtime_registries: super::SharedSessionRuntimeRegistries,
    invocation: super::super::DaemonInvocationState,
    lsp_session_registry: Arc<tokio::sync::Mutex<tracedecay_lsp::LspSessionRegistry>>,
    project_open_gates: Arc<tokio::sync::Mutex<super::super::ProjectOpenGates>>,
    session_temporal_refresh_schedulers: Arc<
        super::super::session_temporal_refresh_scheduler::SessionTemporalRefreshSchedulerRegistry,
    >,
    git_index_transaction_services:
        Arc<super::super::git_transactions::DaemonGitIndexTransactionServiceRegistry>,
    native_integration_services:
        Arc<super::super::native_integration::DaemonNativeIntegrationServiceRegistry>,
    session_sync_service: Arc<super::super::session_sync::DaemonSessionSyncService>,
    project_server_retirements:
        Arc<tokio::sync::Mutex<Vec<super::project_retirement::ProjectServerRetirement>>>,
    #[cfg(unix)]
    automation_schedulers: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<
                super::super::ProjectServerKey,
                super::super::scheduler::AutomationSchedulerHandle,
            >,
        >,
    >,
}

#[derive(Clone)]
struct RemoteRecoveryProjectLifecycleFactoryV1 {
    invocation: super::super::DaemonInvocationState,
    project_open_gates: Arc<tokio::sync::Mutex<super::super::ProjectOpenGates>>,
}

#[derive(Default)]
pub(super) struct RemoteRecoveryProjectLifecyclesV1 {
    factory: Option<RemoteRecoveryProjectLifecycleFactoryV1>,
    profiles: HashMap<PathBuf, Arc<RemoteRecoveryProjectLifecycleV1>>,
}

pub(super) type SharedRemoteRecoveryProjectLifecyclesV1 =
    Arc<std::sync::RwLock<RemoteRecoveryProjectLifecyclesV1>>;

pub(in crate::daemon) type RemoteRecoveryProjectQuiescenceV1 =
    Arc<super::project_retirement::ProjectRetirementFenceV1>;

impl StoreAdministration {
    pub(in crate::daemon) fn install_remote_recovery_project_lifecycle(
        &self,
        invocation: super::super::DaemonInvocationState,
        project_open_gates: Arc<tokio::sync::Mutex<super::super::ProjectOpenGates>>,
    ) -> Result<()> {
        let mut lifecycles = self
            .remote_recovery_project_lifecycles
            .write()
            .map_err(|_| lifecycle_registry_unavailable())?;
        lifecycles
            .factory
            .get_or_insert(RemoteRecoveryProjectLifecycleFactoryV1 {
                invocation,
                project_open_gates,
            });
        ensure_profile_lifecycle(self, &mut lifecycles).map(|_| ())
    }

    pub(in crate::daemon) fn remote_recovery_project_lifecycle(
        &self,
    ) -> Result<Option<Arc<RemoteRecoveryProjectLifecycleV1>>> {
        let mut lifecycles = self
            .remote_recovery_project_lifecycles
            .write()
            .map_err(|_| lifecycle_registry_unavailable())?;
        if lifecycles.factory.is_none() {
            return Ok(None);
        }
        ensure_profile_lifecycle(self, &mut lifecycles).map(Some)
    }
}

fn ensure_profile_lifecycle(
    administration: &StoreAdministration,
    lifecycles: &mut RemoteRecoveryProjectLifecyclesV1,
) -> Result<Arc<RemoteRecoveryProjectLifecycleV1>> {
    let profile_root = super::authority::canonical_identity_path(
        administration.profile_identity()?.profile_root(),
    )?;
    if let Some(lifecycle) = lifecycles.profiles.get(&profile_root) {
        return Ok(Arc::clone(lifecycle));
    }
    let factory = lifecycles
        .factory
        .as_ref()
        .ok_or_else(lifecycle_registry_unavailable)?
        .clone();
    let lifecycle = Arc::new(RemoteRecoveryProjectLifecycleV1::new(
        administration,
        factory.invocation,
        factory.project_open_gates,
    )?);
    lifecycles
        .profiles
        .insert(profile_root, Arc::clone(&lifecycle));
    Ok(lifecycle)
}

fn lifecycle_registry_unavailable() -> TraceDecayError {
    TraceDecayError::Config {
        message: "remote recovery project lifecycle registry is unavailable".to_owned(),
    }
}

impl RemoteRecoveryProjectLifecycleV1 {
    pub(super) fn new(
        administration: &super::StoreAdministration,
        invocation: super::super::DaemonInvocationState,
        project_open_gates: Arc<tokio::sync::Mutex<super::super::ProjectOpenGates>>,
    ) -> Result<Self> {
        let identity = administration.profile_identity()?.clone();
        Ok(Self {
            brain_id: identity.brain_id().clone(),
            profile_id: identity.profile_id().clone(),
            profile_root: super::authority::canonical_identity_path(identity.profile_root())?,
            gate: Arc::clone(&administration.gate),
            project_servers: Arc::clone(&administration.project_servers),
            session_runtime_registries: Arc::clone(&administration.session_runtime_registries),
            lsp_session_registry: Arc::clone(&invocation.lsp_session_registry),
            invocation,
            project_open_gates,
            session_temporal_refresh_schedulers: Arc::clone(
                &administration.session_temporal_refresh_schedulers,
            ),
            git_index_transaction_services: Arc::clone(
                &administration.git_index_transaction_services,
            ),
            native_integration_services: Arc::clone(&administration.native_integration_services),
            session_sync_service: Arc::clone(&administration.session_sync_service),
            project_server_retirements: Arc::clone(&administration.project_server_retirements),
            #[cfg(unix)]
            automation_schedulers: Arc::clone(&administration.automation_schedulers),
        })
    }

    pub(in crate::daemon) async fn quiesce(
        &self,
        project_id: &ProjectId,
        database: &crate::global_db::RegisteredGlobalDbLeaseV1,
    ) -> Result<RemoteRecoveryProjectQuiescenceV1> {
        let shard = &database.binding().shard_id;
        if shard.brain_id != self.brain_id
            || shard.profile_id != self.profile_id
            || !matches!(
                &shard.scope,
                StoreShardScopeV1::ProjectSessions { project_id: bound }
                    if bound == project_id
            )
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "remote recovery project '{}' does not match the mounted ProjectSessions owner",
                    project_id.as_str()
                ),
            });
        }
        let data_root = database
            .db_path()
            .parent()
            .ok_or_else(|| TraceDecayError::Config {
                message: "remote recovery ProjectSessions database has no store root".to_owned(),
            })?;
        self.settle_retained_runtime_retirement(project_id).await?;
        let writer = self
            .gate
            .acquire(&WriterScope::store(data_root, StoreWriterClass::Owner))
            .await;
        self.ensure_project_recovery_active(project_id).await?;
        let roots = project_roots(
            database,
            &self.project_servers,
            &self.profile_root,
            project_id.as_str(),
        )
        .await?;
        let open_tasks = super::super::project_open_tasks(self.project_open_gates.as_ref()).await;
        let project_open = open_tasks
            .quiesce_project_identity(&self.profile_root, project_id.as_str(), &roots)
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "remote recovery project '{}' open admission did not quiesce",
                    project_id.as_str()
                ),
            })?;
        let invocation = self
            .invocation
            .service
            .quiesce_project(
                &self.lsp_session_registry,
                &self.profile_id,
                project_id,
                &roots,
            )
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "remote recovery project '{}' invocation owners did not drain",
                    project_id.as_str()
                ),
            })?;
        let fence = Arc::new(super::project_retirement::ProjectRetirementFenceV1::new(
            invocation,
            project_open,
            writer,
        ));
        retire_runtime_work(
            &self.project_servers,
            &self.session_temporal_refresh_schedulers,
            #[cfg(unix)]
            &self.automation_schedulers,
            &self.project_server_retirements,
            &self.profile_root,
            project_id.as_str(),
            Some(Arc::clone(&fence)),
        )
        .await?;
        self.git_index_transaction_services
            .retire_project_database(project_id, database.db_path())
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "could not retire recovery project Git transaction actors: {error}"
                ),
            })?;
        self.native_integration_services
            .retire_project_database(project_id, database.db_path())
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "could not retire recovery project native integration actors: {error}"
                ),
            })?;
        self.session_sync_service
            .retire_project(&self.profile_id, project_id)
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("could not retire recovery project session sync: {error}"),
            })?;
        Ok(fence)
    }

    pub(in crate::daemon) async fn authorize_project_recovery(
        &self,
        project_id: &ProjectId,
    ) -> Result<WriterAdmissionGuard> {
        let data_root =
            crate::storage::profile_sharded_data_root(&self.profile_root, project_id.as_str());
        self.settle_retained_runtime_retirement(project_id).await?;
        let writer = self
            .gate
            .acquire(&WriterScope::store(data_root, StoreWriterClass::Content))
            .await;
        self.ensure_project_recovery_active(project_id).await?;
        Ok(writer)
    }

    async fn settle_retained_runtime_retirement(&self, project_id: &ProjectId) -> Result<()> {
        let deadline = tokio::time::Instant::now() + super::super::DAEMON_TASK_ABORT_DEADLINE;
        let receipt = super::project_retirement::settle_project_retirements(
            &self.project_server_retirements,
            &self.profile_root,
            project_id.as_str(),
            deadline,
        )
        .await;
        if receipt.is_clean() {
            Ok(())
        } else {
            Err(TraceDecayError::Config {
                message: format!(
                    "project '{}' retained runtime retirement is incomplete: failed={}, timed_out={}",
                    project_id.as_str(),
                    receipt.failed_count(),
                    receipt.timed_out_count()
                ),
            })
        }
    }

    async fn ensure_project_recovery_active(&self, project_id: &ProjectId) -> Result<()> {
        let registry = {
            let registries = self.session_runtime_registries.lock().await;
            registries
                .get(&self.profile_root)
                .and_then(|entry| entry.registry.get())
                .cloned()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "remote recovery profile registry is unavailable".to_owned(),
                })?
        };
        let profile_database = registry.profile_database().await?;
        if profile_database
            .remote_account_deletion_tombstone(self.profile_id.as_str())
            .await?
            .is_some()
        {
            return Err(TraceDecayError::project_route(
                "remote_deleted",
                false,
                "authenticated profile was remotely deleted",
            ));
        }
        if profile_database
            .remote_deletion_tombstone_for_project(self.profile_id.as_str(), project_id.as_str())
            .await?
            .is_some()
        {
            return Err(TraceDecayError::project_route(
                "remote_deleted",
                false,
                format!("project '{}' was remotely deleted", project_id.as_str()),
            ));
        }
        Ok(())
    }
}

pub(super) async fn project_roots(
    database: &crate::global_db::RegisteredGlobalDbLeaseV1,
    project_servers: &tokio::sync::Mutex<DatabaseOwnerRegistry>,
    profile_root: &Path,
    project_id: &str,
) -> Result<BTreeSet<PathBuf>> {
    let mut roots = BTreeSet::new();
    if let Some(context) = database.project_registry_context_by_id(project_id).await? {
        roots.insert(PathBuf::from(context.project.canonical_root));
        roots.insert(PathBuf::from(context.project.display_root));
        if let Some(git_common_dir) = context.project.git_common_dir {
            roots.insert(PathBuf::from(git_common_dir));
        }
        roots.extend(
            context
                .aliases
                .into_iter()
                .map(|alias| PathBuf::from(alias.alias_path)),
        );
    }
    let registry = project_servers.lock().await;
    roots.extend(
        registry
            .servers
            .keys()
            .filter(|key| {
                key.owner.profile_root == profile_root
                    && key.owner.project_id.as_deref() == Some(project_id)
            })
            .map(|key| key.project_root.clone()),
    );
    roots.retain(|root| root.is_absolute());
    Ok(roots)
}

pub(super) async fn retire_runtime_work(
    project_servers: &tokio::sync::Mutex<DatabaseOwnerRegistry>,
    temporal: &super::super::session_temporal_refresh_scheduler::SessionTemporalRefreshSchedulerRegistry,
    #[cfg(unix)] automation: &tokio::sync::Mutex<
        std::collections::HashMap<
            super::super::ProjectServerKey,
            super::super::scheduler::AutomationSchedulerHandle,
        >,
    >,
    tracked_retirements: &tokio::sync::Mutex<
        Vec<super::project_retirement::ProjectServerRetirement>,
    >,
    profile_root: &Path,
    project_id: &str,
    failure_fence: Option<Arc<super::project_retirement::ProjectRetirementFenceV1>>,
) -> Result<()> {
    let server_retirements = {
        let mut registry = project_servers.lock().await;
        let owners = registry
            .servers
            .keys()
            .filter(|key| {
                key.owner.profile_root == profile_root
                    && key.owner.project_id.as_deref() == Some(project_id)
            })
            .map(|key| key.owner.clone())
            .collect::<Vec<_>>();
        owners
            .into_iter()
            .map(|owner| {
                let servers = registry.remove_owner(&owner);
                (owner, servers)
            })
            .collect::<Vec<_>>()
    };
    for server in server_retirements.iter().flat_map(|(_, servers)| servers) {
        server.revoke_project_server_responses();
        server.abort_project_server_requests();
    }
    for (owner, _) in &server_retirements {
        temporal.retire_project(owner).await;
    }
    #[cfg(unix)]
    retire_maintenance_tasks(automation, tracked_retirements, profile_root, project_id).await;
    for (owner, servers) in server_retirements {
        let task = tokio::spawn(async move {
            super::super::project_server_lifecycle::retire_project_servers_now(servers).await;
        });
        super::project_retirement::track_retirement_task(tracked_retirements, owner, task).await;
    }
    if let Some(fence) = failure_fence {
        super::project_retirement::attach_project_retirement_fence(
            tracked_retirements,
            profile_root,
            project_id,
            fence,
        )
        .await;
    }
    let deadline = tokio::time::Instant::now() + super::super::DAEMON_TASK_ABORT_DEADLINE;
    let receipt = super::project_retirement::settle_project_retirements(
        tracked_retirements,
        profile_root,
        project_id,
        deadline,
    )
    .await;
    if receipt.is_clean() {
        Ok(())
    } else {
        Err(TraceDecayError::Config {
            message: format!(
                "project '{project_id}' runtime retirement is incomplete: failed={}, timed_out={}",
                receipt.failed_count(),
                receipt.timed_out_count()
            ),
        })
    }
}

#[cfg(unix)]
async fn retire_maintenance_tasks(
    automation: &tokio::sync::Mutex<
        std::collections::HashMap<
            super::super::ProjectServerKey,
            super::super::scheduler::AutomationSchedulerHandle,
        >,
    >,
    retirements: &tokio::sync::Mutex<Vec<super::project_retirement::ProjectServerRetirement>>,
    profile_root: &Path,
    project_id: &str,
) {
    let mut tasks = Vec::new();
    {
        let mut schedulers = automation.lock().await;
        let keys = matching_scheduler_keys(&schedulers, profile_root, project_id);
        for key in keys {
            if let Some(mut scheduler) = schedulers.remove(&key)
                && let Some(task) = scheduler.task.take()
            {
                tasks.push((key.owner, task));
            }
        }
    }
    for (_, task) in &tasks {
        task.abort();
    }
    for (owner, task) in tasks {
        super::project_retirement::track_aborted_retirement_task(retirements, owner, task).await;
    }
}

#[cfg(unix)]
fn matching_scheduler_keys<T>(
    schedulers: &std::collections::HashMap<super::super::ProjectServerKey, T>,
    profile_root: &Path,
    project_id: &str,
) -> Vec<super::super::ProjectServerKey> {
    schedulers
        .keys()
        .filter(|key| {
            key.owner.profile_root == profile_root
                && key.owner.project_id.as_deref() == Some(project_id)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::StoreAdministration;
    use crate::daemon::{DaemonInvocationState, ProjectOpenGates};

    fn profile_identity(
        root: &Path,
    ) -> crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1 {
        std::fs::create_dir_all(root).expect("create profile root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
                .expect("secure profile root");
        }
        crate::daemon::profile_identity::load_or_create(root).expect("profile identity")
    }

    #[test]
    fn authenticated_clones_select_profile_keyed_recovery_lifecycles() {
        let first_root = tempfile::tempdir().expect("first profile");
        let second_root = tempfile::tempdir().expect("second profile");
        let first_identity = profile_identity(first_root.path());
        let second_identity = profile_identity(second_root.path());
        let administration = StoreAdministration::default();
        let first = administration
            .clone()
            .with_profile_identity(first_identity.clone());
        let second = administration.with_profile_identity(second_identity.clone());
        first
            .install_remote_recovery_project_lifecycle(
                DaemonInvocationState::default(),
                Arc::new(tokio::sync::Mutex::new(ProjectOpenGates::default())),
            )
            .expect("install lifecycle factory");

        let first_lifecycle = first
            .remote_recovery_project_lifecycle()
            .expect("first lookup")
            .expect("first lifecycle");
        let second_lifecycle = second
            .remote_recovery_project_lifecycle()
            .expect("second lookup")
            .expect("second lifecycle");

        assert_eq!(&first_lifecycle.profile_id, first_identity.profile_id());
        assert_eq!(&second_lifecycle.profile_id, second_identity.profile_id());
        assert!(!Arc::ptr_eq(&first_lifecycle, &second_lifecycle));
        assert!(Arc::ptr_eq(
            &first_lifecycle,
            &first
                .remote_recovery_project_lifecycle()
                .expect("repeat lookup")
                .expect("retained first lifecycle")
        ));
    }
}
