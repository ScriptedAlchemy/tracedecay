//! Root composition for standalone project-store runtime ownership.
//!
//! Standalone init/open registers runtime ports, then joins the process-global
//! session-registry owner and stores the returned lease on [`TraceDecay`].

use std::sync::Arc;

use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;
use tracedecay_domain::errors::Result;
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

/// Join the process-wide standalone session registry after root port registration.
#[hotpath::measure(label = "lifecycle.join_session_registry", future = true)]
pub(crate) async fn join_standalone_session_registry(
    identity: LocalProfileIdentityAuthorityV1,
) -> Result<Arc<DaemonSessionRuntimeRegistryV1>> {
    crate::register_runtime_ports()?;
    tracedecay_store_runtime::join_standalone_session_registry(identity).await
}

impl crate::tracedecay::TraceDecay {
    pub(crate) fn store_runtime_registry(&self) -> &Arc<DaemonSessionRuntimeRegistryV1> {
        &self.store_runtime_registry
    }

    pub(crate) fn retained_store_runtime_registry(&self) -> Arc<DaemonSessionRuntimeRegistryV1> {
        Arc::clone(&self.store_runtime_registry)
    }
}
