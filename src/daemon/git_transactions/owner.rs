//! One retained Git index transaction service per daemon-owned project store.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::{GitIndexApplyRequestV1, GitIndexTransactionPortError};
use tracedecay_domain::{
    GitHeadStateV1, GitIndexPreviewV1, ManifestDigest, ProjectId, UtcMicros, canonical_sha256,
};
use tracedecay_policy::{GitConflictRiskV1, GitEffectAuthorizationV1, GitEffectClassifierV1};

use crate::global_db::GlobalDb;

use super::{
    CurrentGitIndexPolicyStateV1, DaemonGitIndexTransactionService,
    DaemonProjectGitIndexPreviewAssembler, FixedDaemonGitIndexExecutor, GitIndexPolicyRecheckPort,
    GitIndexTransactionStoreRegistry, SharedDaemonGitIndexTransactionStore,
};

const GIT_POLICY_REVISION: u64 = 1;
const GIT_POLICY_DIGEST_DOMAIN: &str = "tracedecay.daemon.git-index-policy.v1";
const GIT_CONFIGURATION_DIGEST_DOMAIN: &str = "tracedecay.daemon.git-index-configuration.v1";
const GIT_CATALOG_DIGEST_DOMAIN: &str = "tracedecay.application.catalog.v1";
const GIT_PRIVACY_DIGEST_DOMAIN: &str = "tracedecay.application.privacy.v1";

/// Rechecks the daemon-minted capability and exact preview scope immediately
/// before the native mutation boundary.
pub(crate) struct DaemonGitIndexPolicyRecheck;

impl GitIndexPolicyRecheckPort for DaemonGitIndexPolicyRecheck {
    fn recheck(
        &self,
        request: &GitIndexApplyRequestV1,
        preview: &GitIndexPreviewV1,
    ) -> Result<CurrentGitIndexPolicyStateV1, GitIndexTransactionPortError> {
        let (policy_digest, configuration_digest) = daemon_git_policy_evidence()?;
        let catalog_digest = canonical_sha256(&GIT_CATALOG_DIGEST_DOMAIN)
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let privacy_digest = canonical_sha256(&GIT_PRIVACY_DIGEST_DOMAIN)
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        if request.proof.catalog_digest != catalog_digest
            || request.proof.privacy_digest != privacy_digest
            || request.proof.external_proof.is_some()
        {
            return Err(GitIndexTransactionPortError::PolicyDenied);
        }
        let scope = request.context.scope();
        Ok(CurrentGitIndexPolicyStateV1 {
            authorization: GitEffectAuthorizationV1 {
                capability_granted: request
                    .context
                    .allows(&request.binding.capability_id, &request.binding.use_case_id),
                owner_scope_matches: scope.project_id == preview.repository_snapshot.project_id
                    && scope.repository_id == preview.repository_snapshot.repository_id
                    && preview.repository_snapshot.worktree_id.as_ref() == Some(&scope.worktree_id)
                    && match (&scope.reference, &preview.repository_snapshot.head) {
                        (
                            Some(reference),
                            GitHeadStateV1::Attached { branch, .. }
                            | GitHeadStateV1::Unborn { branch },
                        ) => reference.as_str() == branch,
                        (None, GitHeadStateV1::Detached { .. }) => true,
                        (None, _) | (Some(_), GitHeadStateV1::Detached { .. }) => false,
                    },
            },
            conflict_risk: GitConflictRiskV1::NoneKnown,
            policy_revision: GIT_POLICY_REVISION,
            policy_digest,
            configuration_digest,
            evaluated_at: request.observed_at,
        })
    }
}

pub(crate) fn daemon_git_policy_evidence()
-> Result<(ManifestDigest, ManifestDigest), GitIndexTransactionPortError> {
    let policy = canonical_sha256(&(GIT_POLICY_DIGEST_DOMAIN, GIT_POLICY_REVISION))
        .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
    let configuration = canonical_sha256(&(GIT_CONFIGURATION_DIGEST_DOMAIN, GIT_POLICY_REVISION))
        .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
    Ok((policy, configuration))
}

pub(crate) type DaemonProjectGitIndexTransactionService = DaemonGitIndexTransactionService<
    SharedDaemonGitIndexTransactionStore,
    FixedDaemonGitIndexExecutor<DaemonProjectGitIndexPreviewAssembler>,
    GitEffectClassifierV1,
    DaemonGitIndexPolicyRecheck,
>;

#[derive(Clone)]
pub(crate) struct DaemonGitInvocationOwner {
    pub(crate) project_id: ProjectId,
    pub(crate) service: Arc<DaemonProjectGitIndexTransactionService>,
}

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
                DaemonGitIndexPolicyRecheck,
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

    /// Resolve only an owner already mounted by project-open admission.
    /// Missing and ambiguous roots deliberately share the same outcome.
    pub(crate) async fn for_repository_root(
        &self,
        repository_root: &std::path::Path,
    ) -> Result<Option<DaemonGitInvocationOwner>, GitIndexTransactionPortError> {
        let repository_root = repository_root
            .canonicalize()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        let services = self.services.lock().await;
        let mut matches = services
            .values()
            .filter(|entry| entry.repository_root == repository_root);
        let Some(entry) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Ok(None);
        }
        Ok(Some(DaemonGitInvocationOwner {
            project_id: entry.project_id.clone(),
            service: Arc::clone(&entry.service),
        }))
    }

    #[cfg(test)]
    pub(crate) async fn quarantine_preview_for_test(
        &self,
        repository_root: &std::path::Path,
        preview: &GitIndexPreviewV1,
        observed_at: UtcMicros,
    ) -> Result<(), GitIndexTransactionPortError> {
        let owner = self
            .for_repository_root(repository_root)
            .await?
            .ok_or(GitIndexTransactionPortError::DaemonUnavailable)?;
        owner
            .service
            .quarantine_preview_for_test(preview, observed_at)
    }
}
