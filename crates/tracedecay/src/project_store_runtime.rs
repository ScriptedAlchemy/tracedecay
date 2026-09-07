//! Root composition for the aggregate's project-store runtime port.
//!
//! Standalone init/open joins one daemon session registry per profile, then
//! hands the aggregate a handle that is consumed only as
//! [`ProjectStoreRuntimeV1`]. The concrete registry stays here so existing
//! daemon and MCP callers of [`TraceDecay::store_runtime_registry`] keep
//! compiling without this slice retargeting them.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use tokio::sync::Mutex as AsyncMutex;
use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;
use tracedecay_domain::errors::Result;
use tracedecay_runtime_core::weak_registry::WeakRegistry;
use tracedecay_usecases::tracedecay::ProjectStoreRuntimeV1;

use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

/// One standalone session runtime registry per profile, process-wide.
///
/// Direct init/open still has a single writer for the profile session-relation
/// graph (an exclusive Grafeo file lock). A second independent registry on the
/// same profile cannot open that store. Concurrent opens in one process join
/// the live registry; entries are weak so close-then-reopen constructs a
/// fresh mount after the last holder drops.
static STANDALONE_SESSION_REGISTRIES: LazyLock<
    AsyncMutex<WeakRegistry<PathBuf, DaemonSessionRuntimeRegistryV1>>,
> = LazyLock::new(|| AsyncMutex::new(WeakRegistry::new()));

/// Root-owned handle around the sole [`ProjectStoreRuntimeV1`] implementor.
///
/// The aggregate stores this and calls [`Self::port`]. Out-of-tree daemon
/// consumers reach the concrete registry through
/// [`TraceDecay::store_runtime_registry`](crate::tracedecay::TraceDecay::store_runtime_registry).
#[derive(Clone)]
pub(crate) struct ProjectStoreRuntimeHandle {
    registry: Arc<DaemonSessionRuntimeRegistryV1>,
}

impl ProjectStoreRuntimeHandle {
    pub(crate) fn from_registry(registry: Arc<DaemonSessionRuntimeRegistryV1>) -> Self {
        Self { registry }
    }

    pub(crate) fn port(&self) -> &dyn ProjectStoreRuntimeV1 {
        self.registry.as_ref()
    }
}

impl From<Arc<DaemonSessionRuntimeRegistryV1>> for ProjectStoreRuntimeHandle {
    fn from(registry: Arc<DaemonSessionRuntimeRegistryV1>) -> Self {
        Self::from_registry(registry)
    }
}

#[hotpath::measure(label = "lifecycle.join_session_registry", future = true)]
pub(crate) async fn join_standalone_session_registry(
    identity: LocalProfileIdentityAuthorityV1,
) -> Result<ProjectStoreRuntimeHandle> {
    crate::register_runtime_ports()?;
    let profile_key =
        tracedecay_runtime_core::lifecycle_lease::canonical_or_original(identity.profile_root());
    let registries = STANDALONE_SESSION_REGISTRIES.lock().await;
    if let Some(registry) = registries.get_live(&profile_key) {
        return Ok(ProjectStoreRuntimeHandle::from_registry(registry));
    }
    let registry = Arc::new(DaemonSessionRuntimeRegistryV1::open(identity).await?);
    registries.insert(profile_key, &registry);
    Ok(ProjectStoreRuntimeHandle::from_registry(registry))
}

#[cfg(test)]
pub(crate) async fn open_project_store_runtime(
    identity: LocalProfileIdentityAuthorityV1,
) -> Result<ProjectStoreRuntimeHandle> {
    crate::register_runtime_ports()?;
    Ok(ProjectStoreRuntimeHandle::from_registry(Arc::new(
        DaemonSessionRuntimeRegistryV1::open(identity).await?,
    )))
}

impl crate::tracedecay::TraceDecay {
    pub(crate) fn store_runtime_registry(&self) -> &Arc<DaemonSessionRuntimeRegistryV1> {
        &self.store_runtime_registry.registry
    }

    pub(crate) fn retained_store_runtime_registry(&self) -> Arc<DaemonSessionRuntimeRegistryV1> {
        Arc::clone(&self.store_runtime_registry.registry)
    }
}
