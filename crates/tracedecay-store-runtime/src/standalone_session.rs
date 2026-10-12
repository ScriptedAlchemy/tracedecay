//! Process-global owner for standalone session-runtime registries.
//!
//! Direct init/open still has a single writer for the profile session-relation
//! graph (an exclusive Grafeo file lock). A second independent registry on the
//! same profile cannot open that store. Concurrent opens of one profile join
//! a single in-flight open; opens of other profiles proceed independently.
//! Entries are weak so close-then-reopen constructs a fresh mount after the
//! last holder drops.
//!
//! The composition root stores the returned lease and registers runtime ports
//! before joining.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;
use tracedecay_domain::errors::Result;
use tracedecay_runtime_core::weak_registry::SingleFlightWeakRegistry;

use crate::DaemonSessionRuntimeRegistryV1;

static STANDALONE_SESSION_REGISTRIES: LazyLock<
    SingleFlightWeakRegistry<PathBuf, DaemonSessionRuntimeRegistryV1>,
> = LazyLock::new(SingleFlightWeakRegistry::new);

/// Join the process-wide standalone session registry for `identity`.
///
/// Returns a live lease the caller stores. Port registration stays in the
/// composition root so this crate never names root wiring.
#[tracing::instrument(name = "lifecycle.join_session_registry", level = "trace", skip_all)]
pub async fn join_standalone_session_registry(
    identity: LocalProfileIdentityAuthorityV1,
) -> Result<Arc<DaemonSessionRuntimeRegistryV1>> {
    let profile_key =
        tracedecay_runtime_core::lifecycle_lease::canonical_or_original(identity.profile_root());
    STANDALONE_SESSION_REGISTRIES
        .get_or_open(profile_key, || {
            DaemonSessionRuntimeRegistryV1::open(identity)
        })
        .await
}
