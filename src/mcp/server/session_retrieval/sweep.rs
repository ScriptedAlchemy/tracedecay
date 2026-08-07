//! Registered-project session-retrieval sweep.
//!
//! Serves `project_scope=all_registered` message search: enumerates the
//! project registry, opens each selected project's durable session store
//! through the daemon session registry with exact project/profile/store
//! identity, executes the same authorized retrieval command per root, and
//! returns every per-root outcome with its provenance. The sweep never
//! aliases the active project's session store onto another project and
//! reports every unserved project with a typed reason.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};

use super::{DaemonSessionRetrievalRoot, DaemonSessionRetrievalService, MESSAGE_SEARCH_PROFILE_ID};
use crate::application::context::{
    BranchId, ProfileId, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
};
use crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1;
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::global_db::{ProjectRegistryContext, RegisteredGlobalDb};
use crate::mcp::tools::{
    SessionRetrievalCommand, SessionRetrievalServicePort, SessionRetrievalStoreScope,
    SessionRetrievalSweepFuture, SessionRetrievalSweepOutcome, SessionRetrievalSweepPort,
    SessionRetrievalSweepRootView, SessionRetrievalSweepSkipReason, SessionRetrievalSweepSkipView,
};

/// Bounded number of registered projects one sweep serves. Projects beyond
/// the bound are reported as truncation, never silently dropped.
const SWEEP_MAX_PROJECTS: usize = 64;

pub(crate) struct DaemonSessionRetrievalSweep {
    registry: Arc<RegisteredGlobalDb>,
    session_stores: Arc<DaemonSessionRuntimeRegistryV1>,
    profile_identity: LocalProfileIdentityAuthorityV1,
}

impl DaemonSessionRetrievalSweep {
    pub(crate) fn new(
        registry: Arc<RegisteredGlobalDb>,
        session_stores: Arc<DaemonSessionRuntimeRegistryV1>,
        profile_identity: LocalProfileIdentityAuthorityV1,
    ) -> Self {
        Self {
            registry,
            session_stores,
            profile_identity,
        }
    }

    /// Binds one registered project's retrieval root from its registry
    /// context. The graph-scope selection mirrors the active-project binding
    /// but cannot disambiguate by serving database, so a project with more
    /// than one writable scope is served only when its default branch names
    /// exactly one of them.
    fn registered_root(
        context: &ProjectRegistryContext,
    ) -> Result<DaemonSessionRetrievalRoot, SessionRetrievalSweepSkipReason> {
        let project = &context.project;
        let mut candidates = Vec::new();
        for store in &context.stores {
            for scope in &store.graph_scopes {
                if scope.writable
                    && scope.project_id == project.project_id
                    && scope.store_id == store.store.store_id
                {
                    candidates.push(scope);
                }
            }
        }
        let selected = match candidates.as_slice() {
            [] => return Err(SessionRetrievalSweepSkipReason::StoreIdentityMissing),
            [scope] => scope,
            scopes => {
                let mut default_branch_scopes = scopes.iter().filter(|scope| {
                    project
                        .default_branch
                        .as_deref()
                        .is_some_and(|branch| branch == scope.branch_name)
                });
                match (default_branch_scopes.next(), default_branch_scopes.next()) {
                    (Some(scope), None) => scope,
                    _ => return Err(SessionRetrievalSweepSkipReason::StoreIdentityAmbiguous),
                }
            }
        };
        let mismatch = SessionRetrievalSweepSkipReason::StoreIdentityMismatch;
        let project_key = ProjectId::new(project.project_id.clone()).map_err(|_| mismatch)?;
        let repository_id = project
            .git_common_dir
            .clone()
            .unwrap_or_else(|| format!("repository.project.{}", project.project_id));
        let identity = ResolvedSessionIdentity::for_project(
            ProfileId::new(MESSAGE_SEARCH_PROFILE_ID).map_err(|_| mismatch)?,
            project_key,
            SessionStoreId::new(selected.store_id.clone()).map_err(|_| mismatch)?,
            SessionRootId::new(selected.graph_scope_id.clone()).map_err(|_| mismatch)?,
            ResolvedGitRoute::new(
                RepositoryId::new(repository_id).map_err(|_| mismatch)?,
                WorktreeId::new(project.canonical_root.clone()).map_err(|_| mismatch)?,
                BranchId::new(selected.graph_scope_id.clone()).map_err(|_| mismatch)?,
            ),
        );
        let mut project_paths = context
            .aliases
            .iter()
            .map(|alias| PathBuf::from(&alias.alias_path))
            .collect::<HashSet<_>>();
        project_paths.insert(PathBuf::from(&project.canonical_root));
        project_paths.insert(PathBuf::from(&project.display_root));
        Ok(DaemonSessionRetrievalRoot {
            store_scope: SessionRetrievalStoreScope::Project,
            identity,
            project_id: Some(project.project_id.clone()),
            project_paths,
            authorized_root: Some(project.display_root.clone()),
            expected_runtime_shard: None,
        })
    }

    async fn root_view(
        &self,
        context: ProjectRegistryContext,
        command: &SessionRetrievalCommand,
    ) -> Result<SessionRetrievalSweepRootView, SessionRetrievalSweepSkipView> {
        let project_id = context.project.project_id.clone();
        let display_root = context.project.display_root.clone();
        let canonical_root = PathBuf::from(&context.project.canonical_root);
        let skip = |reason| SessionRetrievalSweepSkipView {
            project_id: project_id.clone(),
            reason,
        };
        let root = Self::registered_root(&context).map_err(&skip)?;
        let root = root
            .with_project_runtime_shard(&self.profile_identity)
            .ok_or_else(|| skip(SessionRetrievalSweepSkipReason::StoreIdentityMismatch))?;
        let typed_project_id = ProjectId::new(project_id.clone())
            .map_err(|_| skip(SessionRetrievalSweepSkipReason::StoreIdentityMismatch))?;
        let database = self
            .session_stores
            .project_sessions(typed_project_id, [canonical_root])
            .await
            .map_err(|_| skip(SessionRetrievalSweepSkipReason::StoreMountFailed))?;
        let service = DaemonSessionRetrievalService::new_registered(
            Arc::clone(&database),
            database,
            root,
            None,
        )
        .ok_or_else(|| skip(SessionRetrievalSweepSkipReason::StoreIdentityMismatch))?;
        let outcome = service.execute(command.clone()).await;
        Ok(SessionRetrievalSweepRootView {
            project_id,
            root: display_root,
            outcome,
        })
    }
}

impl SessionRetrievalSweepPort for DaemonSessionRetrievalSweep {
    fn execute_registered<'a>(
        &'a self,
        command: SessionRetrievalCommand,
    ) -> SessionRetrievalSweepFuture<'a> {
        Box::pin(async move {
            if command.store_scope() != SessionRetrievalStoreScope::Project
                || command.project_selector().is_some()
            {
                return SessionRetrievalSweepOutcome::WrongScope;
            }
            let Ok(records) = self
                .registry
                .list_code_projects(SWEEP_MAX_PROJECTS + 1)
                .await
            else {
                return SessionRetrievalSweepOutcome::RegistryUnavailable;
            };
            let registry_truncated = records.len() > SWEEP_MAX_PROJECTS;
            let mut roots = Vec::new();
            let mut skipped = Vec::new();
            for record in records.into_iter().take(SWEEP_MAX_PROJECTS) {
                let context = match self
                    .registry
                    .project_registry_context_by_id(&record.project_id)
                    .await
                {
                    Ok(Some(context)) => context,
                    Ok(None) | Err(_) => {
                        skipped.push(SessionRetrievalSweepSkipView {
                            project_id: record.project_id,
                            reason: SessionRetrievalSweepSkipReason::RegistryContextMissing,
                        });
                        continue;
                    }
                };
                match self.root_view(context, &command).await {
                    Ok(view) => roots.push(view),
                    Err(skip) => skipped.push(skip),
                }
            }
            SessionRetrievalSweepOutcome::Complete {
                roots,
                skipped,
                registry_truncated,
            }
        })
    }
}
