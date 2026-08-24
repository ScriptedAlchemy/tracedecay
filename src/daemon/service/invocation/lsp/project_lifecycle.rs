use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracedecay_domain::{ManifestDigest, ProjectId, UserProfileId};
use tracedecay_lsp::LspSessionRegistry;

use super::DaemonInvocationService;
use crate::daemon::service::project_runtime::ProjectRuntimeRootQuiescenceV1;

impl DaemonInvocationService {
    pub(crate) async fn expire_project(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        profile_id: &UserProfileId,
        project_id: &ProjectId,
        project_roots: &BTreeSet<PathBuf>,
    ) -> bool {
        let runtime_owners_retired = self.project_runtimes.retire_roots(project_roots).await;
        let protocol_owners_retired = self
            .retire_project_protocol_owners(lsp_registry, profile_id, project_id, project_roots)
            .await;
        protocol_owners_retired && runtime_owners_retired
    }

    pub(crate) async fn quiesce_project(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        profile_id: &UserProfileId,
        project_id: &ProjectId,
        project_roots: &BTreeSet<PathBuf>,
    ) -> Option<ProjectRuntimeRootQuiescenceV1> {
        let runtime_quiescence = self.project_runtimes.quiesce_roots(project_roots).await?;
        let protocol_owners_retired = self
            .retire_project_protocol_owners(lsp_registry, profile_id, project_id, project_roots)
            .await;
        protocol_owners_retired.then_some(runtime_quiescence)
    }

    async fn retire_project_protocol_owners(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        profile_id: &UserProfileId,
        project_id: &ProjectId,
        project_roots: &BTreeSet<PathBuf>,
    ) -> bool {
        let _lsp_admission = self.lsp_admission_open.lock().await;
        self.context_scout_registries
            .lock()
            .await
            .retain(|key, _| !key.belongs_to(profile_id, project_id, project_roots));
        let retired_workspace_digests = {
            let mut workspaces = self.authorized_lsp_workspaces.lock().await;
            let retired = workspaces
                .iter()
                .filter(|(_, workspace)| {
                    workspace.scope_set.roots().iter().any(|root| {
                        root.locator().is_some_and(|locator| {
                            &locator.profile.profile_id == profile_id
                                && &locator.project_id == project_id
                                && project_roots.contains(&locator.canonical_root)
                        })
                    })
                })
                .map(|(digest, _)| digest.clone())
                .collect::<HashSet<_>>();
            workspaces.retain(|digest, _| !retired.contains(digest));
            retired
        };
        let retired_sessions = {
            let mut sessions = self.lsp_sessions.lock().await;
            let retired = sessions
                .iter()
                .filter(|(_, session)| {
                    session
                        .project_identity
                        .belongs_to(profile_id, project_id, project_roots)
                        || workspace_belongs_to_project(
                            session.actor.workspace(),
                            &retired_workspace_digests,
                        )
                })
                .map(|(session_id, _)| session_id.clone())
                .collect::<Vec<_>>();
            for session_id in &retired {
                sessions.remove(session_id);
            }
            retired
        };
        let mut clean = true;
        {
            let mut registry = lsp_registry.lock().await;
            for session_id in &retired_sessions {
                registry.reclaim(session_id);
            }
        }
        for session_id in retired_sessions {
            clean &= self.lsp_lease_tasks.cancel(&session_id).await.is_ok();
        }
        clean
    }
}

fn workspace_belongs_to_project(
    workspace: &tracedecay_lsp::AuthorizedLspWorkspace,
    retired_workspace_digests: &HashSet<ManifestDigest>,
) -> bool {
    workspace
        .scope_set_digest()
        .is_some_and(|digest| retired_workspace_digests.contains(digest))
}
