//! One retained Git index transaction service per daemon-owned project store.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::{GitIndexApplyRequestV1, GitIndexTransactionPortError};
use tracedecay_domain::{GitIndexPreviewV1, ProjectId, UtcMicros};
use tracedecay_policy::GitEffectClassifierV1;

use crate::global_db::GlobalDb;

use super::{
    CurrentGitIndexPolicyStateV1, DaemonGitIndexTransactionService,
    DaemonProjectGitIndexPreviewAssembler, FixedDaemonGitIndexExecutor, GitIndexPolicyRecheckPort,
    GitIndexTransactionStoreRegistry, SharedDaemonGitIndexTransactionStore,
};

/// PR11 deliberately has no public mutation binding. Until PR12 supplies the
/// daemon's live policy adapter, any accidentally reached apply path fails
/// closed and is terminalized as `AbortedNoChange` by the service.
pub(crate) struct FailClosedGitIndexPolicyRecheck;

impl GitIndexPolicyRecheckPort for FailClosedGitIndexPolicyRecheck {
    fn recheck(
        &self,
        _request: &GitIndexApplyRequestV1,
        _preview: &GitIndexPreviewV1,
    ) -> Result<CurrentGitIndexPolicyStateV1, GitIndexTransactionPortError> {
        Err(GitIndexTransactionPortError::PolicyDenied)
    }
}

pub(crate) type DaemonProjectGitIndexTransactionService = DaemonGitIndexTransactionService<
    SharedDaemonGitIndexTransactionStore,
    FixedDaemonGitIndexExecutor<DaemonProjectGitIndexPreviewAssembler>,
    GitEffectClassifierV1,
    FailClosedGitIndexPolicyRecheck,
>;

struct ServiceEntry {
    project_id: ProjectId,
    repository_root: PathBuf,
    service: Arc<DaemonProjectGitIndexTransactionService>,
}

/// Owns the store actor, native executor, classifier, policy recheck, and
/// repository queue for each canonical project `GlobalDb`.
#[derive(Default)]
pub(crate) struct DaemonGitIndexTransactionServiceRegistry {
    stores: GitIndexTransactionStoreRegistry,
    services: tokio::sync::Mutex<HashMap<PathBuf, ServiceEntry>>,
    creation_gate: tokio::sync::Mutex<()>,
}

impl DaemonGitIndexTransactionServiceRegistry {
    pub(crate) async fn ensure(
        &self,
        database: Arc<GlobalDb>,
        repository_root: PathBuf,
        project_id: ProjectId,
        observed_at: UtcMicros,
    ) -> Result<Arc<DaemonProjectGitIndexTransactionService>, GitIndexTransactionPortError> {
        let database_path = database
            .db_path()
            .canonicalize()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let repository_root = repository_root
            .canonicalize()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        if let Some(service) = self
            .existing(&database_path, &repository_root, &project_id)
            .await?
        {
            return Ok(service);
        }

        let _creation = self.creation_gate.lock().await;
        if let Some(service) = self
            .existing(&database_path, &repository_root, &project_id)
            .await?
        {
            return Ok(service);
        }

        // Open/retain the store actor under the creation gate before native
        // recovery runs on a blocking thread. Later ensures reuse this actor.
        let store = self
            .stores
            .ensure(database)
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let native_root = repository_root.clone();
        let (project_id, service) = tokio::task::spawn_blocking(move || {
            let native = FixedDaemonGitIndexExecutor::new(
                DaemonProjectGitIndexPreviewAssembler::new(native_root, project_id.clone()),
            );
            DaemonGitIndexTransactionService::start(
                store,
                native,
                GitEffectClassifierV1::default(),
                FailClosedGitIndexPolicyRecheck,
                observed_at,
            )
            .map(|service| (project_id, service))
        })
        .await
        .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)??;
        let service = Arc::new(service);
        self.services.lock().await.insert(
            database_path,
            ServiceEntry {
                project_id,
                repository_root,
                service: Arc::clone(&service),
            },
        );
        Ok(service)
    }

    async fn existing(
        &self,
        database_path: &PathBuf,
        repository_root: &PathBuf,
        project_id: &ProjectId,
    ) -> Result<Option<Arc<DaemonProjectGitIndexTransactionService>>, GitIndexTransactionPortError>
    {
        let services = self.services.lock().await;
        let Some(entry) = services.get(database_path) else {
            return Ok(None);
        };
        if entry.project_id != *project_id || entry.repository_root != *repository_root {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        }
        Ok(Some(Arc::clone(&entry.service)))
    }
}
