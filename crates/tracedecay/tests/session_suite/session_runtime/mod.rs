//! Composition-root integration tests for the session-runtime crate:
//! temporal refresh scheduling, retained history ingest, and session sync
//! exercised through the root host-admission fixtures, which compose the
//! `TraceDecay` aggregate and therefore cannot live in
//! `tracedecay-session-runtime` itself.

use std::sync::Arc;

use tracedecay::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_session_runtime::StoreOwnerKey;
use tracedecay_session_runtime::session_sync::test_harness::{
    SessionTemporalRefreshPassReport, SessionTemporalRefreshWakeState,
    run_session_temporal_refresh_pass,
};
use tracedecay_session_runtime::session_temporal_refresh_scheduler::{projector, registry, wake};
use tracedecay_sessions::admission::HostAdmissionScope;

mod project_lifecycle;
mod retained_history;
mod session_sync;
mod temporal_refresh;
mod worker_persistence;

pub(crate) struct SessionTemporalRefreshTestAuthority {
    _runtime: HostAdmissionTestRuntimeV1,
    database: RegisteredGlobalDbLeaseV1,
}

impl SessionTemporalRefreshTestAuthority {
    /// Binds the registered session database of `scope` from `runtime`, keeping
    /// the runtime alive for as long as the authority is.
    pub(crate) fn bind(
        runtime: HostAdmissionTestRuntimeV1,
        scope: HostAdmissionScope,
    ) -> Result<Self> {
        let database =
            runtime
                .registered_database_arc(scope)
                .ok_or_else(|| TraceDecayError::Database {
                    operation: "bind session temporal refresh test authority".to_owned(),
                    message: "registered session database mount is unavailable".to_owned(),
                })?;
        Ok(Self {
            _runtime: runtime,
            database,
        })
    }

    fn database(&self) -> &tracedecay_global_db::RegisteredGlobalDb {
        self.database.as_ref()
    }

    fn project<'a>(
        &'a self,
        projector: &'a dyn projector::SessionTemporalRefreshProjector,
        recovery: tracedecay_session_temporal_store::SessionRefreshRecoveryV1,
    ) -> projector::SessionTemporalRefreshProjectionFuture<'a> {
        projector.project(&self.database, recovery)
    }

    async fn run_pass(
        &self,
        state: &Arc<SessionTemporalRefreshWakeState>,
        projector: &dyn projector::SessionTemporalRefreshProjector,
        policy: projector::SessionTemporalRefreshPolicy,
    ) -> SessionTemporalRefreshPassReport {
        run_session_temporal_refresh_pass(&self.database, state, projector, policy).await
    }

    async fn ensure_profile(
        &self,
        registry: &registry::SessionTemporalRefreshSchedulerRegistry,
    ) -> wake::SessionTemporalRefreshWake {
        registry
            .ensure_profile(self.database.db_path().to_path_buf(), self.database.clone())
            .await
    }

    async fn ensure_project(
        &self,
        registry: &registry::SessionTemporalRefreshSchedulerRegistry,
        owner: StoreOwnerKey,
    ) -> wake::SessionTemporalRefreshWake {
        registry.ensure_project(owner, self.database.clone()).await
    }

    async fn rekey_project(
        &self,
        registry: &registry::SessionTemporalRefreshSchedulerRegistry,
        old_owner: &StoreOwnerKey,
        new_owner: StoreOwnerKey,
    ) {
        registry
            .rekey_project(old_owner, new_owner, self.database.clone())
            .await;
    }
}
