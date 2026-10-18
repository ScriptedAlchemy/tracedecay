//! Daemon-backed project-registry reads: registered-project listing, search,
//! and single-project context resolution for the
//! [`ProjectRegistryReadPort`] implementation.
//!
//! This module owns the registry database handle so MCP handlers do not. A
//! mounted-but-empty registry answers with an empty listing, and an
//! unreadable registry still fails.

use serde_json::Value;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use tracedecay_contracts::{
    ProjectRegistryContextCommand, ProjectRegistryContextFuture, ProjectRegistryContextOutcome,
    ProjectRegistryContextView, ProjectRegistryListingCommand, ProjectRegistryListingFuture,
    ProjectRegistryListingScope, ProjectRegistryListingView, ProjectRegistryReadPort,
    ProjectRegistrySelector,
};
use tracedecay_dashboard_api::project_registry::{
    align_public_checkout_branches, build_project_registry_view, public_code_project_for_checkout,
    public_code_project_from_record,
};
use tracedecay_domain::errors::Result;
use tracedecay_global_db::{CodeProjectRecord, ProjectRegistryContext, RegisteredGlobalDbLeaseV1};

#[derive(Clone)]
pub struct DaemonProjectRegistryReadService {
    registry: RegisteredGlobalDbLeaseV1,
}

impl DaemonProjectRegistryReadService {
    pub const fn new(registry: RegisteredGlobalDbLeaseV1) -> Self {
        Self { registry }
    }

    /// Resolves the served root's registered `project_id`, the same identity
    /// lookup the `tracedecay projects` CLI performs for its own active
    /// project. An unregistered root, or a projectless read with no served
    /// root, simply has no active id.
    async fn active_project_id(
        &self,
        active_project_root: Option<&Path>,
    ) -> Result<Option<String>> {
        let Some(active_project_root) = active_project_root else {
            return Ok(None);
        };
        let git_common_dir = tracedecay_runtime_core::worktree::git_common_dir(active_project_root);
        let resolved: Pin<
            Box<dyn Future<Output = Result<Option<ProjectRegistryContext>>> + Send + '_>,
        > =
            Box::pin(self.registry.project_registry_context_by_identity(
                active_project_root,
                git_common_dir.as_deref(),
            ));
        Ok(resolved.await?.map(|context| context.project.project_id))
    }

    async fn listing(
        &self,
        active_project_root: Option<&Path>,
        mut projects: Vec<CodeProjectRecord>,
        limit: usize,
    ) -> Result<ProjectRegistryListingView> {
        let active: Pin<Box<dyn Future<Output = Result<Option<String>>> + Send + '_>> =
            Box::pin(self.active_project_id(active_project_root));
        let active_id = active.await?;
        let truncated = projects.len() > limit;
        projects.truncate(limit);
        let contexts_future: Pin<
            Box<dyn Future<Output = Result<Vec<ProjectRegistryContext>>> + Send + '_>,
        > = Box::pin(
            self.registry
                .project_registry_contexts_for_projects(&projects),
        );
        let contexts = contexts_future.await?;
        let view = build_project_registry_view(
            &contexts,
            active_id.as_deref(),
            active_project_root,
            truncated,
        );
        let mut projects = projects
            .iter()
            .map(|project| public_code_project_from_record(project, active_id.as_deref()))
            .collect::<Vec<_>>();
        align_public_checkout_branches(&mut projects, &view);
        Ok(ProjectRegistryListingView {
            registry_path: self.registry.db_path().to_path_buf(),
            truncated,
            view,
            projects,
        })
    }

    async fn execute_list(
        &self,
        command: ProjectRegistryListingCommand,
    ) -> Result<ProjectRegistryListingView> {
        let ProjectRegistryListingCommand {
            active_project_root,
            scope,
            limit,
        } = command;
        // One extra row detects truncation without a second count query.
        let page = limit.saturating_add(1);
        let projects = match &scope {
            ProjectRegistryListingScope::All => {
                let listed: Pin<
                    Box<dyn Future<Output = Result<Vec<CodeProjectRecord>>> + Send + '_>,
                > = Box::pin(self.registry.list_code_projects(page));
                listed.await?
            }
            ProjectRegistryListingScope::Matching { query } => {
                let searched: Pin<
                    Box<dyn Future<Output = Result<Vec<CodeProjectRecord>>> + Send + '_>,
                > = Box::pin(self.registry.try_search_code_projects(query, page));
                searched.await?
            }
        };
        let listed_view: Pin<
            Box<dyn Future<Output = Result<ProjectRegistryListingView>> + Send + '_>,
        > = Box::pin(self.listing(active_project_root.as_deref(), projects, limit));
        listed_view.await
    }

    async fn resolve_context(
        &self,
        selector: &ProjectRegistrySelector,
    ) -> Result<Option<ProjectRegistryContext>> {
        match selector {
            ProjectRegistrySelector::ProjectId(project_id) => {
                self.registry
                    .project_registry_context_by_id(project_id)
                    .await
            }
            ProjectRegistrySelector::Path {
                path,
                allow_git_identity,
            } => {
                if let Some(context) = self
                    .registry
                    .project_registry_context_by_alias(path)
                    .await?
                {
                    return Ok(Some(context));
                }
                if !allow_git_identity {
                    return Ok(None);
                }
                let git_common_dir = tracedecay_runtime_core::worktree::git_common_dir(path);
                self.registry
                    .project_registry_context_by_identity(path, git_common_dir.as_deref())
                    .await
            }
        }
    }

    async fn execute_context(
        &self,
        command: ProjectRegistryContextCommand,
    ) -> Result<ProjectRegistryContextOutcome> {
        let ProjectRegistryContextCommand {
            active_project_root,
            selector,
        } = command;
        let registry_path = self.registry.db_path().to_path_buf();
        let Some(context) = self.resolve_context(&selector).await? else {
            return Ok(ProjectRegistryContextOutcome::NotFound { registry_path });
        };
        let active_id = self
            .active_project_id(active_project_root.as_deref())
            .await?;
        let is_active = active_id.as_deref() == Some(context.project.project_id.as_str());
        let preferred = match &selector {
            ProjectRegistrySelector::Path { path, .. } => Some(path.as_path()),
            ProjectRegistrySelector::ProjectId(_) if is_active => active_project_root.as_deref(),
            ProjectRegistrySelector::ProjectId(_) => None,
        };
        Ok(ProjectRegistryContextOutcome::Context(Box::new(
            ProjectRegistryContextView {
                registry_path,
                is_active,
                project: public_code_project_for_checkout(
                    &context.project,
                    &context.aliases,
                    active_id.as_deref(),
                    preferred,
                ),
                aliases: serialize_records(&context.aliases)?,
                stores: serialize_records(&context.stores)?,
            },
        )))
    }
}

fn serialize_records<T: serde::Serialize>(records: &[T]) -> Result<Vec<Value>> {
    records
        .iter()
        .map(|record| serde_json::to_value(record).map_err(Into::into))
        .collect()
}

impl ProjectRegistryReadPort for DaemonProjectRegistryReadService {
    fn list(&self, command: ProjectRegistryListingCommand) -> ProjectRegistryListingFuture<'_> {
        Box::pin(async move { self.execute_list(command).await })
    }

    fn context(&self, command: ProjectRegistryContextCommand) -> ProjectRegistryContextFuture<'_> {
        Box::pin(async move { self.execute_context(command).await })
    }
}
