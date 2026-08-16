//! Daemon-owned registry assembly for profile and project session shards.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracedecay_agent_hosts::ports::project_runtime::{ProfileRuntime, RuntimeFuture};
use tracedecay_domain::BrainNodeId;
use tracedecay_store::{
    AdmissionConfigV1, ProjectId, StoreIncarnationV1, StoreShardIdV1, StoreShardScopeV1,
};

use super::register_registered_schema_installer;
use super::registry::{
    DestructiveMaintenanceReservation, DestructiveMaintenanceTarget,
    LifecycleShardRuntimePublisher, ProfileAuthorityPin, ProfileAuthorityPinResult,
    StoreRuntimeHandle, StoreRuntimeKey, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
    StoreRuntimeRegistry, StoreRuntimeRegistryFailure, StoreRuntimeResolver,
};
use super::resolver::{
    LocalProfileStoreAuthorityV1, LocalProjectEnrollmentAuthorityV1, LocalStoreLocatorResolutionV1,
    LocalStoreRuntimeResolverV1,
};
use crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1;
use crate::db::{Database, DatabaseAccessMode, DatabaseAuthority};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;

mod code_graph;
mod code_graph_manifest;
mod code_reads;
mod maintenance;
mod memory_graph_reconciliation_tasks;
mod mounts;
mod profile_memory;
mod remote_recovery;
mod retained_hook_tasks;

use maintenance::RegisteredSchemaConvergenceMaintenance;
use memory_graph_reconciliation_tasks::RetainedMemoryGraphReconciliationTasksV1;
use retained_hook_tasks::RetainedHookTasks;

pub(crate) use code_graph::RetainedCodeGraphRuntimeV1;
pub(crate) use profile_memory::open_user_memory_db;

static LONG_LIVED_SESSION_MAINTENANCE: AtomicBool = AtomicBool::new(false);

/// Every session relation store resolves through this one registry, whose
/// capacity reserves one slot for the profile session store.
pub(crate) const RETAINED_SESSION_GRAPH_RUNTIME_CAPACITY: usize = 8;
/// One relation-graph slot is reserved for the profile session store; project
/// runtimes must stay below the remaining registered graph capacity.
pub(crate) const DEFAULT_RETAINED_PROJECT_RUNTIME_CAPACITY: usize =
    RETAINED_SESSION_GRAPH_RUNTIME_CAPACITY - 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionRuntimeRetentionTelemetryV1 {
    pub(crate) project_runtime_capacity: u64,
    pub(crate) profile_memory_runtimes: u64,
    pub(crate) profile_session_runtimes: u64,
    pub(crate) project_memory_runtimes: u64,
    pub(crate) project_session_runtimes: u64,
    pub(crate) retained_memory_graph_reconciliation_tasks: u64,
    pub(crate) retired_project_memory_runtimes: u64,
    pub(crate) retired_project_session_runtimes: u64,
    pub(crate) retirement_refusals: u64,
}

fn remote_restore_quarantine_fence_path(database: &Path) -> std::path::PathBuf {
    database.with_extension("remote-restore-quarantine.json")
}

pub(crate) fn mark_process_long_lived_for_session_maintenance() {
    LONG_LIVED_SESSION_MAINTENANCE.store(true, Ordering::Relaxed);
}

pub(crate) fn release_process_allocator_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `malloc_trim` is a process-wide, thread-safe glibc allocator
        // maintenance operation. It does not invalidate live allocations.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    pub(crate) fn profile_id(&self) -> &tracedecay_domain::configuration::UserProfileId {
        self.identity.profile_id()
    }

    pub(crate) fn runtime_telemetry(
        &self,
    ) -> crate::daemon::store_runtime::telemetry::RuntimeTelemetryProjection {
        let inventory = self.registry.inventory(AdmissionConfigV1::default(), None);
        crate::daemon::store_runtime::telemetry::project_runtime_telemetry(&inventory)
    }

    pub(crate) async fn session_runtime_retention_telemetry(
        &self,
    ) -> Result<SessionRuntimeRetentionTelemetryV1> {
        let project_memory_runtimes = self.project_memory.lock().await.len();
        let project_session_runtimes = self.project_sessions.lock().await.len();
        let profile_memory_runtimes = u64::from(self.profile_memory.lock().await.is_some());
        let profile_session_runtimes = u64::from(self.profile_sessions.lock().await.is_some());
        let retained_memory_graph_reconciliation_tasks =
            self.memory_graph_reconciliation_tasks.retained_count()?;
        let project_runtime_capacity =
            u64::try_from(self.project_runtime_capacity.get()).map_err(|_| {
                session_registry_error(
                    "observe retained project runtime capacity",
                    "project runtime capacity exceeds telemetry range".to_owned(),
                )
            })?;
        let project_memory_runtimes = u64::try_from(project_memory_runtimes).map_err(|_| {
            session_registry_error(
                "observe retained project memory runtimes",
                "project memory runtime cardinality exceeds telemetry range".to_owned(),
            )
        })?;
        let project_session_runtimes = u64::try_from(project_session_runtimes).map_err(|_| {
            session_registry_error(
                "observe retained project session runtimes",
                "project session runtime cardinality exceeds telemetry range".to_owned(),
            )
        })?;
        let retained_memory_graph_reconciliation_tasks =
            u64::try_from(retained_memory_graph_reconciliation_tasks).map_err(|_| {
                session_registry_error(
                    "observe retained memory graph reconciliation tasks",
                    "memory graph task cardinality exceeds telemetry range".to_owned(),
                )
            })?;
        Ok(SessionRuntimeRetentionTelemetryV1 {
            project_runtime_capacity,
            profile_memory_runtimes,
            profile_session_runtimes,
            project_memory_runtimes,
            project_session_runtimes,
            retained_memory_graph_reconciliation_tasks,
            retired_project_memory_runtimes: self
                .retired_project_memory_runtimes
                .load(Ordering::Acquire),
            retired_project_session_runtimes: self
                .retired_project_session_runtimes
                .load(Ordering::Acquire),
            retirement_refusals: self.retirement_refusals.load(Ordering::Acquire),
        })
    }
}

/// One canonical registry and profile pin shared by every daemon session shard.
pub(crate) struct DaemonSessionRuntimeRegistryV1 {
    identity: LocalProfileIdentityAuthorityV1,
    incarnation: StoreIncarnationV1,
    resolver: Arc<LocalStoreRuntimeResolverV1>,
    registry: StoreRuntimeRegistry,
    graph_registry: tracedecay_graph_db::GraphDbRegistry,
    graph_manifest_provider: Arc<code_graph_manifest::DaemonCodeGraphManifestProviderV1>,
    graph_lifecycle_cancelled: Arc<AtomicBool>,
    profile_pin: ProfileAuthorityPin,
    profile_runtime: StoreRuntimeHandle,
    profile_database: Mutex<Option<Arc<RegisteredGlobalDb>>>,
    profile_memory: Mutex<Option<Arc<Database>>>,
    profile_sessions: Mutex<Option<Arc<RegisteredGlobalDb>>>,
    remote_nodes: Mutex<BTreeMap<BrainNodeId, Arc<Database>>>,
    remote_credential_authority:
        Arc<crate::daemon::remote_protocol::DaemonRemoteCredentialAuthorityV1>,
    remote_replay_transaction:
        Arc<crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1>,
    remote_recovery_authorities: Mutex<
        BTreeMap<
            BrainNodeId,
            Arc<tracedecay_rusqlite_runtime::remote::RemoteRecoverySqliteAuthorityV1>,
        >,
    >,
    project_memory: Arc<Mutex<BTreeMap<ProjectId, Arc<Database>>>>,
    project_sessions: Arc<Mutex<BTreeMap<ProjectId, Arc<RegisteredGlobalDb>>>>,
    project_runtime_capacity: NonZeroUsize,
    retired_project_memory_runtimes: AtomicU64,
    retired_project_session_runtimes: AtomicU64,
    retirement_refusals: AtomicU64,
    registered_schema_convergence: RegisteredSchemaConvergenceMaintenance,
    retained_hook_tasks: RetainedHookTasks,
    memory_graph_reconciliation_tasks: Arc<RetainedMemoryGraphReconciliationTasksV1>,
    session_sync_service:
        Arc<OnceLock<Weak<crate::daemon::session_sync::DaemonSessionSyncService>>>,
    remote_recovery_project_lifecycle: Arc<
        OnceLock<Weak<crate::daemon::branch_admin::remote_recovery_lifecycle::RemoteRecoveryProjectLifecycleV1>>,
    >,
    /// Fixed at construction: whether this registry's process runs long-lived
    /// session maintenance (background historical schema convergence) for the
    /// shards it attaches. Short-lived CLI/hook processes stay `false`.
    long_lived_session_maintenance: bool,
}

impl DaemonSessionRuntimeRegistryV1 {
    pub(crate) fn install_session_sync_service(
        &self,
        service: &Arc<crate::daemon::session_sync::DaemonSessionSyncService>,
    ) -> Result<()> {
        let service = Arc::downgrade(service);
        if let Some(retained) = self.session_sync_service.get() {
            return if Weak::ptr_eq(retained, &service) {
                Ok(())
            } else {
                Err(TraceDecayError::Config {
                    message:
                        "session runtime registry already has a different session sync service"
                            .to_owned(),
                })
            };
        }
        match self.session_sync_service.set(service) {
            Ok(()) => Ok(()),
            Err(service)
                if self
                    .session_sync_service
                    .get()
                    .is_some_and(|retained| Weak::ptr_eq(retained, &service)) =>
            {
                Ok(())
            }
            Err(_) => Err(TraceDecayError::Config {
                message: "session runtime registry session sync installation raced".to_owned(),
            }),
        }
    }

    pub(in crate::daemon) fn install_remote_recovery_project_lifecycle(
        &self,
        lifecycle: &Arc<
            crate::daemon::branch_admin::remote_recovery_lifecycle::RemoteRecoveryProjectLifecycleV1,
        >,
    ) -> Result<()> {
        let lifecycle = Arc::downgrade(lifecycle);
        if let Some(retained) = self.remote_recovery_project_lifecycle.get() {
            return if Weak::ptr_eq(retained, &lifecycle) {
                Ok(())
            } else {
                Err(TraceDecayError::Config {
                    message: "session runtime registry already has a different remote recovery project lifecycle".to_owned(),
                })
            };
        }
        match self.remote_recovery_project_lifecycle.set(lifecycle) {
            Ok(()) => Ok(()),
            Err(lifecycle)
                if self
                    .remote_recovery_project_lifecycle
                    .get()
                    .is_some_and(|retained| Weak::ptr_eq(retained, &lifecycle)) =>
            {
                Ok(())
            }
            Err(_) => Err(TraceDecayError::Config {
                message: "session runtime registry remote recovery lifecycle installation raced"
                    .to_owned(),
            }),
        }
    }

    fn session_sync_service(
        &self,
    ) -> Arc<OnceLock<Weak<crate::daemon::session_sync::DaemonSessionSyncService>>> {
        Arc::clone(&self.session_sync_service)
    }

    fn remote_recovery_project_lifecycle(
        &self,
    ) -> Arc<
        OnceLock<Weak<crate::daemon::branch_admin::remote_recovery_lifecycle::RemoteRecoveryProjectLifecycleV1>>,
    >{
        Arc::clone(&self.remote_recovery_project_lifecycle)
    }

    pub(crate) fn retain_hook_task<F, Fut>(
        &self,
        provider: &str,
        session_id: &str,
        operation: F,
    ) -> bool
    where
        F: FnOnce(tracedecay_usecases::observation::ObservationCancellation) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.retained_hook_tasks
            .retain(provider, session_id, operation)
    }
}

impl ProfileRuntime for DaemonSessionRuntimeRegistryV1 {
    fn profile_id(&self) -> &tracedecay_domain::configuration::UserProfileId {
        self.identity.profile_id()
    }

    fn profile_sessions(&self) -> RuntimeFuture<'_, Arc<RegisteredGlobalDb>> {
        Box::pin(DaemonSessionRuntimeRegistryV1::profile_sessions(self))
    }

    fn open_user_memory_db(&self) -> RuntimeFuture<'_, Database> {
        Box::pin(open_user_memory_db(self))
    }
}

fn runtime_incarnation(identity: &LocalProfileIdentityAuthorityV1) -> Result<StoreIncarnationV1> {
    let process_run_id = crate::runtime_identity::process_run_id();
    let daemon_generation = crate::daemon::authority::current_record(identity.profile_root())?
        .filter(|record| {
            record.process_run_id == process_run_id
                && record.profile_root == identity.profile_root()
                && record.brain_id.as_ref() == Some(identity.brain_id())
                && record.profile_id.as_ref() == Some(identity.profile_id())
        })
        .map(|record| record.epoch);
    let generation = match daemon_generation {
        Some(generation) => generation,
        None => process_runtime_generation(process_run_id).ok_or_else(|| {
            session_registry_error(
                "create store incarnation",
                "process runtime generation has an unsupported format".to_owned(),
            )
        })?,
    };
    StoreIncarnationV1::new(generation)
        .map_err(|error| session_registry_error("create store incarnation", error.to_string()))
}

fn process_runtime_generation(process_run_id: &str) -> Option<u64> {
    let raw = process_run_id
        .get(..16)
        .and_then(|prefix| u64::from_str_radix(prefix, 16).ok())
        .or_else(|| {
            process_run_id
                .strip_prefix("mcp-")
                .and_then(|value| value.parse::<u64>().ok())
                .map(|timestamp| timestamp ^ u64::from(std::process::id()))
        })?;
    Some((raw & i64::MAX as u64).max(1))
}

async fn open_runtime(
    registry: &StoreRuntimeRegistry,
    resolver: &LocalStoreRuntimeResolverV1,
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    profile_pin: Option<ProfileAuthorityPin>,
    database_authority: Option<DatabaseAuthority>,
    initialize_if_missing: bool,
    operation: &'static str,
) -> Result<StoreRuntimeHandle> {
    open_runtime_with_presence(
        registry,
        resolver,
        shard_id,
        incarnation,
        profile_pin,
        database_authority,
        initialize_if_missing,
        false,
        None,
        operation,
    )
    .await
    .map(|(runtime, _)| runtime)
}

async fn open_runtime_during_remote_restore(
    registry: &StoreRuntimeRegistry,
    resolver: &LocalStoreRuntimeResolverV1,
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    profile_pin: Option<ProfileAuthorityPin>,
    expected_opened_file_identity: u64,
    operation: &'static str,
) -> Result<StoreRuntimeHandle> {
    open_runtime_with_presence(
        registry,
        resolver,
        shard_id,
        incarnation,
        profile_pin,
        None,
        false,
        true,
        Some(expected_opened_file_identity),
        operation,
    )
    .await
    .map(|(runtime, _)| runtime)
}

#[allow(clippy::too_many_arguments)]
async fn open_runtime_with_presence(
    registry: &StoreRuntimeRegistry,
    resolver: &LocalStoreRuntimeResolverV1,
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    profile_pin: Option<ProfileAuthorityPin>,
    database_authority: Option<DatabaseAuthority>,
    initialize_if_missing: bool,
    allow_remote_restore_fence: bool,
    required_opened_file_identity: Option<u64>,
    operation: &'static str,
) -> Result<(StoreRuntimeHandle, bool)> {
    let key = StoreRuntimeKey::new(shard_id.clone(), incarnation);
    let locator = match resolver.resolve_key(&key) {
        LocalStoreLocatorResolutionV1::Resolved(locator) => locator,
        LocalStoreLocatorResolutionV1::Unavailable(unavailable) => {
            return Err(session_registry_error(
                operation,
                format!(
                    "registered store locator unavailable: {:?}",
                    unavailable.reason
                ),
            ));
        }
    };
    let authority = match database_authority {
        Some(authority) => authority,
        None => DatabaseAuthority::for_runtime(locator.locator().path(), operation)?,
    };
    if authority.canonical_database_path() != locator.locator().path() {
        return Err(session_registry_error(
            operation,
            format!(
                "registered locator {} does not match originating database authority {}",
                locator.locator().path().display(),
                authority.canonical_database_path().display()
            ),
        ));
    }
    let expected_opened_file_identity = if let Some(expected) = required_opened_file_identity {
        Some(expected)
    } else if !allow_remote_restore_fence
        && matches!(&shard_id.scope, StoreShardScopeV1::ProjectSessions { .. })
    {
        remote_recovery::remote_restore_activated_open_identity(locator.locator().path())?
    } else {
        None
    };
    let exists = locator
        .locator()
        .path()
        .try_exists()
        .map_err(|error| session_registry_error(operation, error.to_string()))?;
    let request = if initialize_if_missing && !exists {
        StoreRuntimeOpenRequest::new_initialize_authorized(
            shard_id,
            incarnation,
            profile_pin,
            authority,
        )
    } else {
        StoreRuntimeOpenRequest::new_authorized(shard_id, incarnation, profile_pin, authority)
    };
    let request = match expected_opened_file_identity {
        Some(expected) => request.require_opened_file_identity(expected),
        None => request,
    };
    match registry.open(request).await {
        StoreRuntimeOpenResult::Published(runtime) => Ok((runtime, exists)),
        StoreRuntimeOpenResult::Failed(failure) => Err(registry_open_error(
            "open registered session runtime",
            failure,
        )),
    }
}

fn registry_open_error(
    operation: &'static str,
    failure: StoreRuntimeRegistryFailure,
) -> TraceDecayError {
    match failure {
        StoreRuntimeRegistryFailure::ResetRequired { authority, reason } => {
            TraceDecayError::reset_required(authority, reason)
        }
        failure => session_registry_error(operation, format!("{failure:?}")),
    }
}

fn session_registry_error(operation: &'static str, message: String) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message,
    }
}

#[cfg(test)]
mod verified_graph_runtime_port_contract_tests;

#[cfg(test)]
mod project_memory_relation_graph_contract_tests;

#[cfg(test)]
mod tests;
