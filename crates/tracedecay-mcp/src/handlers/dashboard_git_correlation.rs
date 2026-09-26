//! Daemon-side implementation of the dashboard git-correlation read port.
//!
//! [`DashboardGitCorrelationReadAdapter`] reads the verified
//! session-git-evidence graph projection through the registered
//! project-sessions authority's mounted graph runtime, the same store the
//! `sessions_for` and correlation-health reads consult, and hands Loom's
//! routes complete typed span and commit rows for the requested sessions. A
//! projection that has never published a verified head is the typed empty
//! start, never an error, and a store without its graph runtime mount stays a
//! typed failed read.

use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_dashboard_api::{
    DashboardGitCorrelationReadErrorV1, DashboardGitCorrelationReadFutureV1,
    DashboardGitCorrelationReadPortV1, DashboardGitCorrelationReadV1,
};
use tracedecay_global_db::GlobalDbGitCorrelationStore;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;

pub struct DashboardGitCorrelationReadAdapter {
    store: Arc<GlobalDbGitCorrelationStore<RegisteredGlobalDbLeaseV1>>,
}

impl DashboardGitCorrelationReadAdapter {
    pub fn new(project_database: RegisteredGlobalDbLeaseV1) -> Self {
        Self {
            store: Arc::new(GlobalDbGitCorrelationStore::new(project_database)),
        }
    }
}

#[hotpath::measure(label = "mcp.dashboard.git_correlation.read")]
fn read_sessions(
    store: &GlobalDbGitCorrelationStore<RegisteredGlobalDbLeaseV1>,
    session_ids: &BTreeSet<String>,
) -> Result<DashboardGitCorrelationReadV1, DashboardGitCorrelationReadErrorV1> {
    let read_error = |detail: String| DashboardGitCorrelationReadErrorV1 { detail };
    let Some(evidence) = store
        .git_evidence_for_sessions(session_ids)
        .map_err(|error| read_error(error.to_string()))?
    else {
        return Ok(DashboardGitCorrelationReadV1::Unpublished);
    };
    Ok(DashboardGitCorrelationReadV1::Published {
        generation: evidence.generation,
        spans: evidence.spans,
        commits: evidence.commits,
    })
}

impl DashboardGitCorrelationReadPortV1 for DashboardGitCorrelationReadAdapter {
    fn read(&self, session_ids: BTreeSet<String>) -> DashboardGitCorrelationReadFutureV1<'_> {
        let store = Arc::clone(&self.store);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || read_sessions(&store, &session_ids))
                .await
                .map_err(|error| DashboardGitCorrelationReadErrorV1 {
                    detail: format!("Git evidence read task failed: {error}"),
                })?
        })
    }
}
