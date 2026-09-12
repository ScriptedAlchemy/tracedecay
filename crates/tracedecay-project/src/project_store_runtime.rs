//! Standalone project-store runtime ownership.
//!
//! Standalone init/open joins the process-global session-registry owner and
//! stores the returned lease on [`TraceDecay`]. It refuses first when the
//! composition root has not registered the runtime ports: the hook bindings
//! every open publishes need the registered daemon client, so an unwired
//! process fails typed before it mounts anything.

use std::sync::Arc;

use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;
use tracedecay_domain::errors::Result;
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

use crate::project::TraceDecay;
use crate::runtime_ports::require_runtime_ports;

/// Join the process-wide standalone session registry once runtime ports are registered.
#[hotpath::measure(label = "lifecycle.join_session_registry", future = true)]
pub(crate) async fn join_standalone_session_registry(
    identity: LocalProfileIdentityAuthorityV1,
) -> Result<Arc<DaemonSessionRuntimeRegistryV1>> {
    require_runtime_ports()?;
    tracedecay_store_runtime::join_standalone_session_registry(identity).await
}

impl TraceDecay {
    pub fn store_runtime_registry(&self) -> &Arc<DaemonSessionRuntimeRegistryV1> {
        &self.store_runtime_registry
    }

    pub fn retained_store_runtime_registry(&self) -> Arc<DaemonSessionRuntimeRegistryV1> {
        Arc::clone(&self.store_runtime_registry)
    }
}
