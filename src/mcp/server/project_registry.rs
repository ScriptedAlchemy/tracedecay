//! Daemon-backed project-registry reads: registered-project listing, search,
//! and single-project context resolution for the
//! [`ProjectRegistryReadPort`] implementation.
//!
//! This module owns the registry database handle so MCP handlers do not. A
//! mounted-but-empty registry answers with an empty listing, an absent
//! registry never reaches here (the boundary reports that state), and an
//! unreadable registry still fails.

use serde_json::Value;
use std::path::Path;

use crate::errors::Result;
use crate::global_db::{CodeProjectRecord, ProjectRegistryContext, RegisteredGlobalDbLeaseV1};
use crate::mcp::tools::{
    ProjectRegistryContextCommand, ProjectRegistryContextFuture, ProjectRegistryContextOutcome,
    ProjectRegistryContextView, ProjectRegistryListingCommand, ProjectRegistryListingFuture,
    ProjectRegistryListingOutcome, ProjectRegistryListingScope, ProjectRegistryListingView,
    ProjectRegistryReadPort, ProjectRegistrySelector,
};
use crate::project_registry::{PublicCodeProject, build_project_registry_view};

#[derive(Clone)]
pub(crate) struct DaemonProjectRegistryReadService {
    registry: RegisteredGlobalDbLeaseV1,
}

impl DaemonProjectRegistryReadService {
    pub(crate) const fn new(registry: RegisteredGlobalDbLeaseV1) -> Self {
        Self { registry }
    }

    /// Resolves the served root's registered `project_id`, the same identity
    /// lookup the `tracedecay projects` CLI performs for its own active
    /// project. An unregistered root simply has no active id.
    async fn active_project_id(&self, active_project_root: &Path) -> Result<Option<String>> {
        let git_common_dir = crate::worktree::git_common_dir(active_project_root);
        Ok(self
            .registry
            .project_registry_context_by_identity(active_project_root, git_common_dir.as_deref())
            .await?
            .map(|context| context.project.project_id))
    }

    async fn listing(
        &self,
        active_project_root: &Path,
        mut projects: Vec<CodeProjectRecord>,
        limit: usize,
    ) -> Result<ProjectRegistryListingView> {
        let active_id = self.active_project_id(active_project_root).await?;
        let truncated = projects.len() > limit;
        projects.truncate(limit);
        let contexts = self
            .registry
            .project_registry_contexts_for_projects(&projects)
            .await?;
        let view = build_project_registry_view(&contexts, active_id.as_deref(), truncated);
        let projects = projects
            .iter()
            .map(|project| PublicCodeProject::from_record(project, active_id.as_deref()))
            .collect::<Vec<_>>();
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
    ) -> Result<ProjectRegistryListingOutcome> {
        let ProjectRegistryListingCommand {
            active_project_root,
            scope,
            limit,
        } = command;
        // One extra row detects truncation without a second count query.
        let page = limit.saturating_add(1);
        let projects = match &scope {
            ProjectRegistryListingScope::All => self.registry.list_code_projects(page).await?,
            ProjectRegistryListingScope::Matching { query } => {
                self.registry.try_search_code_projects(query, page).await?
            }
        };
        Ok(ProjectRegistryListingOutcome::Listing(
            self.listing(&active_project_root, projects, limit).await?,
        ))
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
                let git_common_dir = crate::worktree::git_common_dir(path);
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
        let active_id = self.active_project_id(&active_project_root).await?;
        let is_active = active_id.as_deref() == Some(context.project.project_id.as_str());
        Ok(ProjectRegistryContextOutcome::Context(Box::new(
            ProjectRegistryContextView {
                registry_path,
                is_active,
                project: PublicCodeProject::from_record(&context.project, active_id.as_deref()),
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
