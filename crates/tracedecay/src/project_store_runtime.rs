//! Root composition for standalone project-store runtime ownership.
//!
//! Standalone init/open joins one daemon session registry per profile, then
//! hands the aggregate the concrete registry it and daemon/MCP callers use.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use tokio::sync::Mutex as AsyncMutex;
use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;
use tracedecay_domain::errors::Result;
use tracedecay_runtime_core::weak_registry::WeakRegistry;

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

#[hotpath::measure(label = "lifecycle.join_session_registry", future = true)]
pub(crate) async fn join_standalone_session_registry(
    identity: LocalProfileIdentityAuthorityV1,
) -> Result<Arc<DaemonSessionRuntimeRegistryV1>> {
    crate::register_runtime_ports()?;
    let profile_key =
        tracedecay_runtime_core::lifecycle_lease::canonical_or_original(identity.profile_root());
    let registries = STANDALONE_SESSION_REGISTRIES.lock().await;
    if let Some(registry) = registries.get_live(&profile_key) {
        return Ok(registry);
    }
    let registry = Arc::new(DaemonSessionRuntimeRegistryV1::open(identity).await?);
    registries.insert(profile_key, &registry);
    Ok(registry)
}

#[cfg(test)]
pub(crate) async fn open_project_store_runtime(
    identity: LocalProfileIdentityAuthorityV1,
) -> Result<Arc<DaemonSessionRuntimeRegistryV1>> {
    crate::register_runtime_ports()?;
    Ok(Arc::new(DaemonSessionRuntimeRegistryV1::open(identity).await?))
}

impl crate::tracedecay::TraceDecay {
    pub(crate) fn store_runtime_registry(&self) -> &Arc<DaemonSessionRuntimeRegistryV1> {
        &self.store_runtime_registry
    }

    pub(crate) fn retained_store_runtime_registry(&self) -> Arc<DaemonSessionRuntimeRegistryV1> {
        Arc::clone(&self.store_runtime_registry)
    }
}
