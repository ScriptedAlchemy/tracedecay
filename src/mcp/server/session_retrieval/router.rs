use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_domain::ProjectId;

use super::{
    DaemonSessionRetrievalRoot, DaemonSessionRetrievalService, SessionRetrievalCommand,
    SessionRetrievalServiceOutcome, SessionRetrievalServicePort, SessionRetrievalUnavailable,
    SessionRetrievalUnavailableReason,
};
use crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{ProjectRegistryContext, RegisteredGlobalDb};
use crate::mcp::server::RetainedProjectGraphResolver;
use crate::mcp::tools::SessionRetrievalProjectSelector;

pub(crate) struct DaemonProjectSessionRetrievalRouter {
    pub(super) active: DaemonSessionRetrievalService,
    registry: Arc<RegisteredGlobalDb>,
    resolver: RetainedProjectGraphResolver,
    profile_identity: crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
}

enum RoutedProjectService {
    Ready(Box<DaemonSessionRetrievalService>),
    Unavailable {
        code: &'static str,
        message: &'static str,
        retryable: bool,
    },
}

pub(crate) fn into_project_session_retrieval_service(
    active: DaemonSessionRetrievalService,
    registry: Option<&Arc<RegisteredGlobalDb>>,
    resolver: Option<&RetainedProjectGraphResolver>,
    profile_identity: Option<&crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
) -> Arc<dyn SessionRetrievalServicePort> {
    match (registry, resolver, profile_identity) {
        (Some(registry), Some(resolver), Some(profile_identity)) => {
            Arc::new(DaemonProjectSessionRetrievalRouter::new(
                active,
                Arc::clone(registry),
                Arc::clone(resolver),
                profile_identity.clone(),
            ))
        }
        _ => Arc::new(active),
    }
}

pub(crate) struct ProjectSessionRetrievalServiceInputs<'a> {
    pub(crate) database: Option<&'a Arc<RegisteredGlobalDb>>,
    pub(crate) root: Option<DaemonSessionRetrievalRoot>,
    pub(crate) registered_database: Option<&'a Arc<RegisteredGlobalDb>>,
    pub(crate) refresh_status: Option<SessionTemporalRefreshWake>,
    pub(crate) registry: Option<&'a Arc<RegisteredGlobalDb>>,
    pub(crate) resolver: Option<&'a RetainedProjectGraphResolver>,
    pub(crate) profile_identity:
        Option<&'a crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
}

pub(crate) fn build_project_session_retrieval_service(
    inputs: ProjectSessionRetrievalServiceInputs<'_>,
) -> Option<Arc<dyn SessionRetrievalServicePort>> {
    let ProjectSessionRetrievalServiceInputs {
        database,
        root,
        registered_database,
        refresh_status,
        registry,
        resolver,
        profile_identity,
    } = inputs;
    let service = database.zip(root).and_then(|(database, root)| {
        let refresh_status = refresh_status.clone();
        match registered_database {
            Some(registered_database) => DaemonSessionRetrievalService::new_registered(
                Arc::clone(database),
                Arc::clone(registered_database),
                root,
                refresh_status,
            ),
            None => DaemonSessionRetrievalService::new(Arc::clone(database), root, refresh_status),
        }
    });
    service.map(|service| {
        into_project_session_retrieval_service(service, registry, resolver, profile_identity)
    })
}

impl DaemonProjectSessionRetrievalRouter {
    pub(crate) fn new(
        active: DaemonSessionRetrievalService,
        registry: Arc<RegisteredGlobalDb>,
        resolver: RetainedProjectGraphResolver,
        profile_identity: crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    ) -> Self {
        Self {
            active,
            registry,
            resolver,
            profile_identity,
        }
    }

    async fn context_for_path(
        &self,
        project_path: &Path,
    ) -> Result<Option<ProjectRegistryContext>> {
        if let Some(store) = self
            .registry
            .try_resolve_project_store_record_by_alias(project_path)
            .await?
        {
            return self
                .registry
                .project_registry_context_by_id(&store.project_id)
                .await;
        }
        if let Some(context) = self
            .registry
            .project_registry_context_by_alias(project_path)
            .await?
        {
            return Ok(Some(context));
        }
        let git_common_dir = crate::worktree::git_common_dir(project_path);
        self.registry
            .project_registry_context_by_identity(project_path, git_common_dir.as_deref())
            .await
    }

    async fn context_for_selector(
        &self,
        selector: &SessionRetrievalProjectSelector,
    ) -> Result<Option<(ProjectRegistryContext, PathBuf)>> {
        let id_context = match selector.project_id.as_deref() {
            Some(project_id) => {
                self.registry
                    .project_registry_context_by_id(project_id)
                    .await?
            }
            None => None,
        };
        let path_context = match selector.project_path.as_deref() {
            Some(project_path) => {
                let project_path = Path::new(project_path);
                let requested_root = crate::worktree::git_worktree_root(project_path)
                    .unwrap_or_else(|| project_path.to_path_buf());
                self.context_for_path(project_path)
                    .await?
                    .map(|context| (context, requested_root))
            }
            None => None,
        };

        match (id_context, path_context) {
            (Some(id_context), Some((path_context, requested_root))) => {
                if id_context.project.project_id != path_context.project.project_id {
                    return Err(TraceDecayError::project_route(
                        "project_selector_mismatch",
                        false,
                        format!(
                            "project_id '{}' does not own project_path '{}'",
                            id_context.project.project_id,
                            selector.project_path.as_deref().unwrap_or_default(),
                        ),
                    ));
                }
                Ok(Some((id_context, requested_root)))
            }
            (Some(context), None) => Ok(Some((
                context.clone(),
                PathBuf::from(&context.project.canonical_root),
            ))),
            (None, Some((context, requested_root))) => Ok(Some((context, requested_root))),
            (None, None) => Ok(None),
        }
    }

    async fn service_for_context(
        &self,
        context: ProjectRegistryContext,
        requested_root: PathBuf,
    ) -> Result<RoutedProjectService> {
        let request = crate::mcp::server::RetainedProjectGraphRequest::for_registered_project(
            context.clone(),
            requested_root,
        );
        let Some(graph) = (self.resolver)(request).await? else {
            return Ok(RoutedProjectService::Unavailable {
                code: "registered_project_graph_unavailable",
                message: "the selected project's graph is not mounted by the daemon",
                retryable: true,
            });
        };
        let Some(root) = DaemonSessionRetrievalRoot::from_project_context(
            graph.as_ref(),
            self.registry.as_ref(),
            context,
        )
        .and_then(|root| root.with_project_runtime_shard(&self.profile_identity)) else {
            return Ok(RoutedProjectService::Unavailable {
                code: "registered_project_root_unavailable",
                message: "the selected project's retrieval root could not be resolved",
                retryable: false,
            });
        };
        let Some(project_id) = root
            .project_id
            .as_deref()
            .and_then(|project_id| ProjectId::new(project_id.to_owned()).ok())
        else {
            return Ok(RoutedProjectService::Unavailable {
                code: "registered_project_identity_invalid",
                message: "the selected project's registered identity is invalid",
                retryable: false,
            });
        };
        let Some(database) = graph
            .store_runtime_registry()
            .mounted_project_sessions(&project_id)
            .await
        else {
            return Ok(RoutedProjectService::Unavailable {
                code: "registered_project_session_store_unavailable",
                message: "the selected project's session retrieval store is not mounted",
                retryable: true,
            });
        };
        let Some(service) = DaemonSessionRetrievalService::new_registered(
            Arc::clone(&database),
            database,
            root,
            None,
        ) else {
            return Ok(RoutedProjectService::Unavailable {
                code: "registered_project_session_store_binding_invalid",
                message: "the selected project's mounted session store did not match its registered binding",
                retryable: false,
            });
        };
        Ok(RoutedProjectService::Ready(Box::new(service)))
    }

    fn routing_unavailable(
        error: TraceDecayError,
        fallback_code: &'static str,
    ) -> SessionRetrievalServiceOutcome {
        let (code, retryable, message) = error.project_route_context().map_or_else(
            || {
                (
                    fallback_code.to_owned(),
                    error.is_database_error(),
                    error.to_string(),
                )
            },
            |(code, retryable, detail)| (code.to_owned(), retryable, detail.to_owned()),
        );
        SessionRetrievalServiceOutcome::Unavailable(SessionRetrievalUnavailable::routing_failure(
            SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
            code,
            message,
            retryable,
        ))
    }

    pub(super) async fn execute_command(
        &self,
        command: SessionRetrievalCommand,
    ) -> SessionRetrievalServiceOutcome {
        let Some(selector) = command.project_selector() else {
            return self.active.execute_command(command).await;
        };
        let (context, requested_root) = match self.context_for_selector(selector).await {
            Ok(Some(selection)) => selection,
            Ok(None) => return SessionRetrievalServiceOutcome::WrongScope,
            Err(error) => {
                return Self::routing_unavailable(error, "project_registry_lookup_failed");
            }
        };
        if self.active.root.project_id.as_deref() == Some(context.project.project_id.as_str())
            && self.active.root.project_root.as_deref() == Some(requested_root.as_path())
        {
            return self.active.execute_command(command).await;
        }
        match self.service_for_context(context, requested_root).await {
            Ok(RoutedProjectService::Ready(service)) => service.execute_command(command).await,
            Ok(RoutedProjectService::Unavailable {
                code,
                message,
                retryable,
            }) => SessionRetrievalServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::routing_failure(
                    SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
                    code,
                    message,
                    retryable,
                ),
            ),
            Err(error) => Self::routing_unavailable(error, "retained_project_graph_lookup_failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::{
        BranchId, ProfileId, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId,
        SessionStoreId,
    };
    use crate::mcp::tools::SessionRetrievalStoreScope;
    use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};
    use tracedecay_store::StoreShardIdV1;

    #[test]
    fn registered_project_binding_uses_one_durable_profile_and_typed_project() {
        let brain_id = tracedecay_domain::BrainId::try_from("brain.session-retrieval".to_owned())
            .expect("brain identity");
        let profile_id = tracedecay_domain::UserProfileId::try_from(
            "profile.durable-session-retrieval".to_owned(),
        )
        .expect("profile identity");
        let project_id = ProjectId::new("project.session-retrieval").expect("project identity");
        let identity = ResolvedSessionIdentity::for_project(
            ProfileId::new(super::super::MESSAGE_SEARCH_PROFILE_ID).expect("legacy profile"),
            project_id.clone(),
            SessionStoreId::new("store.project.test").expect("store identity"),
            SessionRootId::new("root.project.test").expect("root identity"),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.project.test").expect("repository identity"),
                WorktreeId::new("/project/test").expect("worktree identity"),
                BranchId::new("branch.project.test").expect("branch identity"),
            ),
        );
        let root = DaemonSessionRetrievalRoot {
            store_scope: SessionRetrievalStoreScope::Project,
            identity,
            project_id: Some(project_id.as_str().to_owned()),
            project_root: None,
            authorized_root: None,
            expected_runtime_shard: None,
        }
        .with_project_runtime_identity(brain_id.clone(), profile_id.clone())
        .expect("durable project binding");

        assert_eq!(root.identity.profile_id().as_str(), profile_id.as_str());
        assert_eq!(root.identity.project_id(), Some(&project_id));
        assert_eq!(
            root.expected_runtime_shard,
            Some(StoreShardIdV1::project_sessions(
                brain_id, profile_id, project_id,
            ))
        );
    }
}
