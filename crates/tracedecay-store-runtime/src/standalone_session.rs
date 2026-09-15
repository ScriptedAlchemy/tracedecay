//! Process-global owner for standalone session-runtime registries.
//!
//! Direct init/open still has a single writer for the profile session-relation
//! graph (an exclusive Grafeo file lock). A second independent registry on the
//! same profile cannot open that store. Concurrent opens in one process join
//! the live registry; entries are weak so close-then-reopen constructs a
//! fresh mount after the last holder drops.
//!
//! The composition root stores the returned lease and registers runtime ports
//! before joining.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use tokio::sync::Mutex as AsyncMutex;
use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;
use tracedecay_domain::errors::Result;
use tracedecay_runtime_core::weak_registry::WeakRegistry;

use crate::DaemonSessionRuntimeRegistryV1;

static STANDALONE_SESSION_REGISTRIES: LazyLock<
    AsyncMutex<WeakRegistry<PathBuf, DaemonSessionRuntimeRegistryV1>>,
> = LazyLock::new(|| AsyncMutex::new(WeakRegistry::new()));

/// Join the process-wide standalone session registry for `identity`.
///
/// Returns a live lease the caller stores. Port registration stays in the
/// composition root so this crate never names root wiring.
#[hotpath::measure(label = "lifecycle.join_session_registry", future = true)]
pub async fn join_standalone_session_registry(
    identity: LocalProfileIdentityAuthorityV1,
) -> Result<Arc<DaemonSessionRuntimeRegistryV1>> {
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
