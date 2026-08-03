//! Daemon-owned registry assembly for profile and project session shards.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use tokio::sync::Mutex;
use tracedecay_agent_hosts::ports::project_runtime::{
    MemoryCurateOptions as AgentMemoryCurateOptions, ProfileRuntime, RuntimeFuture,
};
use tracedecay_domain::RefId;
use tracedecay_store::{ProjectId, StoreIncarnationV1, StoreShardIdV1};

use super::register_registered_schema_installer;
use super::registry::{
    DestructiveMaintenanceReservation, DestructiveMaintenanceTarget,
    LifecycleShardRuntimePublisher, ProfileAuthorityPin, ProfileAuthorityPinResult,
    StoreRuntimeHandle, StoreRuntimeKey, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
    StoreRuntimeRegistry, StoreRuntimeRegistryFailure, StoreRuntimeResolver,
};
use super::resolver::{
    LocalCodeStoreAuthorityRegistrationOutcomeV1, LocalCodeStoreAuthorityV1,
    LocalProfileStoreAuthorityV1, LocalProjectEnrollmentAuthorityV1, LocalStoreLocatorResolutionV1,
    LocalStoreRuntimeResolverV1,
};
use crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1;
use crate::db::{Database, DatabaseAccessMode, DatabaseAuthority};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;

mod code_reads;
mod maintenance;
mod mounts;

use maintenance::RegisteredSchemaConvergenceMaintenance;

static LONG_LIVED_SESSION_MAINTENANCE: AtomicBool = AtomicBool::new(false);

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
/// One canonical registry and profile pin shared by every daemon session shard.
pub(crate) struct DaemonSessionRuntimeRegistryV1 {
    identity: LocalProfileIdentityAuthorityV1,
    incarnation: StoreIncarnationV1,
    resolver: Arc<LocalStoreRuntimeResolverV1>,
    registry: StoreRuntimeRegistry,
    profile_pin: ProfileAuthorityPin,
    profile_runtime: StoreRuntimeHandle,
    profile_database: Mutex<Option<Arc<RegisteredGlobalDb>>>,
    profile_memory: Mutex<Option<Arc<Database>>>,
    profile_sessions: Mutex<Option<Arc<RegisteredGlobalDb>>>,
    project_memory: Mutex<BTreeMap<ProjectId, Arc<Database>>>,
    project_sessions: Mutex<BTreeMap<ProjectId, Arc<RegisteredGlobalDb>>>,
    code_graph_open_gates: Mutex<BTreeMap<StoreShardIdV1, Weak<Mutex<()>>>>,
    registered_schema_convergence: RegisteredSchemaConvergenceMaintenance,
    #[cfg(test)]
    long_lived_session_maintenance_for_test: AtomicBool,
}

impl ProfileRuntime for DaemonSessionRuntimeRegistryV1 {
    fn profile_sessions(&self) -> RuntimeFuture<'_, Arc<RegisteredGlobalDb>> {
        Box::pin(DaemonSessionRuntimeRegistryV1::profile_sessions(self))
    }

    fn open_user_memory_db(&self) -> RuntimeFuture<'_, Database> {
        Box::pin(crate::memory::user::open_user_memory_db(self))
    }

    fn curate_user_memory<'a>(
        &'a self,
        profile_root: &'a Path,
        automation_root: &'a Path,
        options: &'a AgentMemoryCurateOptions,
    ) -> RuntimeFuture<'a, serde_json::Value> {
        Box::pin(async move {
            let memory_db = crate::memory::user::open_user_memory_db(self).await?;
            let options = crate::dashboard::memory_curate::MemoryCurateOptions {
                apply: options.apply,
                llm: options.llm,
                llm_ops: options.llm_ops.clone(),
                max_clusters: options.max_clusters,
                min_confidence: options.min_confidence,
            };
            crate::dashboard::memory_curate::run_user_memory_curate(
                &memory_db,
                memory_db.database_path(),
                profile_root,
                automation_root,
                &options,
            )
            .await
        })
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
    match registry.open(request).await {
        StoreRuntimeOpenResult::Published(runtime) => Ok(runtime),
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
    session_registry_error(operation, format!("{failure:?}"))
}

fn session_registry_error(operation: &'static str, message: String) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message,
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests;
