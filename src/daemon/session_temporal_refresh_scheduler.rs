mod history;
mod projector;
mod registry;
#[cfg(test)]
mod retained_history_tests;
#[cfg(test)]
mod tests;
mod wake;
mod worker;

pub(super) use history::{ProfileSessionHistoricalIngestor, ProjectSessionHistoricalIngestor};
pub(super) use registry::SessionTemporalRefreshSchedulerRegistry;
pub(crate) use wake::SessionTemporalRefreshWake;

#[cfg(test)]
pub(crate) struct SessionTemporalRefreshTestAuthority {
    _runtime: crate::host_admission::HostAdmissionTestRuntimeV1,
    database: crate::global_db::RegisteredGlobalDbLeaseV1,
}

#[cfg(test)]
impl SessionTemporalRefreshTestAuthority {
    pub(crate) fn new(
        runtime: crate::host_admission::HostAdmissionTestRuntimeV1,
        database: crate::global_db::RegisteredGlobalDbLeaseV1,
    ) -> Self {
        Self {
            _runtime: runtime,
            database,
        }
    }

    fn database(&self) -> &crate::global_db::RegisteredGlobalDb {
        self.database.as_ref()
    }

    fn database_identity(&self) -> usize {
        std::sync::Arc::as_ptr(&self.database) as usize
    }

    fn project<'a>(
        &'a self,
        projector: &'a dyn projector::SessionTemporalRefreshProjector,
        recovery: crate::store::SessionRefreshRecoveryV1,
    ) -> projector::SessionTemporalRefreshProjectionFuture<'a> {
        projector.project(&self.database, recovery)
    }

    async fn run_pass(
        &self,
        state: &std::sync::Arc<wake::SessionTemporalRefreshWakeState>,
        projector: &dyn projector::SessionTemporalRefreshProjector,
        policy: projector::SessionTemporalRefreshPolicy,
    ) -> registry::SessionTemporalRefreshPassReport {
        worker::run_session_temporal_refresh_pass(&self.database, state, projector, policy).await
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
        owner: super::StoreOwnerKey,
    ) -> wake::SessionTemporalRefreshWake {
        registry.ensure_project(owner, self.database.clone()).await
    }

    async fn rekey_project(
        &self,
        registry: &registry::SessionTemporalRefreshSchedulerRegistry,
        old_owner: &super::StoreOwnerKey,
        new_owner: super::StoreOwnerKey,
    ) {
        registry
            .rekey_project(old_owner, new_owner, self.database.clone())
            .await;
    }
}
