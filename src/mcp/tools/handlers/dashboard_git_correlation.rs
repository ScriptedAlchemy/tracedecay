//! Daemon-side implementation of the dashboard git-correlation read port.
//!
//! [`DashboardGitCorrelationReadAdapter`] recovers the verified
//! session-git-evidence graph projection through the registered
//! project-sessions authority's mounted graph runtime — the same store the
//! `sessions_for` and correlation-health reads consult — and hands Loom's
//! routes complete typed span and commit rows. A projection that has never
//! published a verified head is the typed empty start, never an error, and a
//! store without its graph runtime mount stays a typed failed read.

use crate::dashboard::{
    DashboardGitCorrelationReadErrorV1, DashboardGitCorrelationReadFutureV1,
    DashboardGitCorrelationReadPortV1, DashboardGitCorrelationReadV1,
};
use crate::global_db::RegisteredGlobalDbLeaseV1;
use crate::store::GlobalDbGitCorrelationStore;

pub struct DashboardGitCorrelationReadAdapter {
    store: GlobalDbGitCorrelationStore<RegisteredGlobalDbLeaseV1>,
}

impl DashboardGitCorrelationReadAdapter {
    pub fn new(project_database: RegisteredGlobalDbLeaseV1) -> Self {
        Self {
            store: GlobalDbGitCorrelationStore::new(project_database),
        }
    }

    fn read_inner(
        &self,
    ) -> Result<DashboardGitCorrelationReadV1, DashboardGitCorrelationReadErrorV1> {
        let Some(projection) = self.store.git_evidence_projection().map_err(|error| {
            DashboardGitCorrelationReadErrorV1 {
                detail: error.to_string(),
            }
        })?
        else {
            return Ok(DashboardGitCorrelationReadV1::Unpublished);
        };
        Ok(DashboardGitCorrelationReadV1::Published {
            generation: projection
                .verified_snapshot()
                .generation()
                .as_str()
                .to_owned(),
            spans: projection.projection().spans().to_vec(),
            commits: projection.projection().commit_sessions().to_vec(),
        })
    }
}

impl DashboardGitCorrelationReadPortV1 for DashboardGitCorrelationReadAdapter {
    fn read(&self) -> DashboardGitCorrelationReadFutureV1<'_> {
        Box::pin(std::future::ready(self.read_inner()))
    }
}
