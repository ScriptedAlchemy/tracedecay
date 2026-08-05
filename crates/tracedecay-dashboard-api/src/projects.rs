use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::response::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    scope_from_state,
};
use super::{DashboardState, build_selected_project_state, config_error};
use crate::project_registry::{PublicCodeProject, build_project_registry_view};
use tracedecay_global_db::ProjectRegistryContext;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

#[derive(Clone)]
pub struct DashboardRuntime {
    active: DashboardState,
    project_api: Router<DashboardState>,
    project_states: Arc<RwLock<HashMap<String, CachedProjectState>>>,
}

#[derive(Clone)]
struct CachedProjectState {
    registry_context: ProjectRegistryContext,
    state: DashboardState,
}

impl DashboardRuntime {
    pub fn new(active: DashboardState, project_api: Router<DashboardState>) -> Self {
        Self {
            active,
            project_api,
            project_states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn active_state(&self) -> DashboardState {
        self.active.clone()
    }

    pub fn active_project_id(&self) -> Option<&str> {
        self.active.project_id.as_deref()
    }

    pub fn project_api_router(&self) -> Router<DashboardState> {
        self.project_api.clone()
    }

    fn active_project_root(&self) -> String {
        self.active.project_root.display().to_string()
    }

    pub async fn selected_project_state(&self, project_id: &str) -> Result<SelectedProjectState> {
        if self.active.project_id.as_deref() == Some(project_id) {
            return Ok(SelectedProjectState {
                state: self.active.clone(),
            });
        }

        let db = self
            .active
            .savings_db
            .as_ref()
            .ok_or_else(|| config_error("tracedecay project registry is unavailable"))?;
        let context = db
            .project_registry_context_by_id(project_id)
            .await?
            .ok_or_else(|| config_error(format!("registered project not found: {project_id}")))?;
        if let Some(cached) = self.project_states.read().await.get(project_id).cloned()
            && cached.registry_context == context
        {
            return Ok(SelectedProjectState {
                state: cached.state,
            });
        }
        let project_root = std::path::PathBuf::from(&context.project.canonical_root);
        let resolver = self.active.project_graph_resolver.as_ref().ok_or_else(|| {
            config_error(format!(
                "registered project graph is not mounted: {project_id}"
            ))
        })?;
        let request = crate::project_graph::RetainedProjectGraphRequest::for_registered_project(
            context.clone(),
            project_root.clone(),
        );
        let cg = resolver(request).await?.ok_or_else(|| {
            config_error(format!(
                "registered project graph is not mounted: {project_id}"
            ))
        })?;
        if cg.store_layout().identity.project_id.as_deref() != Some(project_id) {
            return Err(config_error(format!(
                "registered project id mismatch for {project_id}: {}",
                project_root.display()
            )));
        }
        let state = build_selected_project_state(cg, &self.active).await?;
        let mut project_states = self.project_states.write().await;
        if let Some(cached) = project_states.get(project_id).cloned()
            && cached.registry_context == context
        {
            return Ok(SelectedProjectState {
                state: cached.state,
            });
        }
        project_states.insert(
            project_id.to_string(),
            CachedProjectState {
                registry_context: context,
                state: state.clone(),
            },
        );
        Ok(SelectedProjectState { state })
    }
}

pub struct SelectedProjectState {
    pub state: DashboardState,
}

#[derive(Debug, Deserialize)]
pub struct ProjectsParams {
    limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ProjectsPayloadV1 {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    limit: usize,
    truncated: Option<bool>,
    projects: Option<Vec<PublicCodeProject>>,
    active_project_id: Option<String>,
    active_project_root: String,
    summary: Option<crate::project_registry::ProjectRegistrySummary>,
    project_tree: Option<Vec<crate::project_registry::ProjectRepoGroup>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct ProjectContextPayloadV1 {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_active: Option<bool>,
    project: Option<PublicCodeProject>,
    aliases: Vec<tracedecay_global_db::ProjectAliasRecord>,
    stores: Vec<tracedecay_global_db::ProjectStoreContext>,
}

pub async fn list(
    State(runtime): State<DashboardRuntime>,
    Query(params): Query<ProjectsParams>,
) -> Json<DashboardEnvelopeV1<ProjectsPayloadV1>> {
    let limit = params.limit.unwrap_or(100).clamp(1, 250);
    let Some(db) = runtime.active.savings_db.as_ref() else {
        return registry_list_unavailable(
            &runtime,
            ProjectsPayloadV1 {
                status: "missing_registry".to_owned(),
                error: None,
                limit,
                truncated: None,
                projects: None,
                active_project_id: runtime.active_project_id().map(str::to_owned),
                active_project_root: runtime.active_project_root(),
                summary: None,
                project_tree: None,
            },
            "project_registry_not_mounted",
        );
    };

    let mut projects = match db.list_code_projects(limit + 1).await {
        Ok(projects) => projects,
        Err(error) => {
            return registry_list_unavailable(
                &runtime,
                ProjectsPayloadV1 {
                    status: "registry_unavailable".to_owned(),
                    error: Some(error.to_string()),
                    limit,
                    truncated: None,
                    projects: None,
                    active_project_id: runtime.active_project_id().map(str::to_owned),
                    active_project_root: runtime.active_project_root(),
                    summary: None,
                    project_tree: None,
                },
                error.to_string(),
            );
        }
    };
    let truncated = projects.len() > limit;
    projects.truncate(limit);
    let active_project_id = runtime.active_project_id().map(str::to_string);
    let contexts = match db.project_registry_contexts_for_projects(&projects).await {
        Ok(contexts) => contexts,
        Err(error) => {
            return registry_list_unavailable(
                &runtime,
                ProjectsPayloadV1 {
                    status: "registry_unavailable".to_owned(),
                    error: Some(error.to_string()),
                    limit,
                    truncated: None,
                    projects: None,
                    active_project_id,
                    active_project_root: runtime.active_project_root(),
                    summary: None,
                    project_tree: None,
                },
                error.to_string(),
            );
        }
    };
    let view = build_project_registry_view(&contexts, runtime.active_project_id(), truncated);
    let rows = projects
        .iter()
        .map(|project| PublicCodeProject::from_record(project, runtime.active_project_id()))
        .collect::<Vec<_>>();
    let row_count = rows.len() as u64;

    let payload = ProjectsPayloadV1 {
        status: "ok".to_owned(),
        error: None,
        limit,
        truncated: Some(truncated),
        projects: Some(rows),
        active_project_id,
        active_project_root: runtime.active_project_root(),
        summary: Some(view.summary),
        project_tree: Some(view.project_tree),
    };
    let envelope = if truncated {
        DashboardEnvelopeV1::new(
            scope_from_state(&runtime.active),
            DashboardDomainStateV1::Partial,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::fresh_now(),
            payload,
        )
    } else {
        DashboardEnvelopeV1::ready(
            scope_from_state(&runtime.active),
            DashboardCoverageV1::complete(row_count, "projects"),
            payload,
        )
    };
    Json(envelope)
}

fn registry_list_unavailable(
    runtime: &DashboardRuntime,
    payload: ProjectsPayloadV1,
    reason: impl Into<String>,
) -> Json<DashboardEnvelopeV1<ProjectsPayloadV1>> {
    Json(DashboardEnvelopeV1::unavailable(
        scope_from_state(&runtime.active),
        payload,
        reason,
    ))
}

pub fn is_registry_unavailable_error(error: &TraceDecayError) -> bool {
    matches!(
        error,
        TraceDecayError::Database { .. } | TraceDecayError::Sqlite(_)
    ) || matches!(
        error,
        TraceDecayError::Config { message }
            if message == "tracedecay project registry is unavailable"
    )
}

pub fn registry_unavailable_response(
    state: &DashboardState,
    error: &TraceDecayError,
) -> Json<DashboardEnvelopeV1<ProjectContextPayloadV1>> {
    Json(DashboardEnvelopeV1::unavailable(
        scope_from_state(state),
        ProjectContextPayloadV1 {
            status: "registry_unavailable".to_owned(),
            error: Some(error.to_string()),
            is_active: None,
            project: None,
            aliases: Vec::new(),
            stores: Vec::new(),
        },
        error.to_string(),
    ))
}

pub async fn context(
    State(runtime): State<DashboardRuntime>,
    AxumPath(project_id): AxumPath<String>,
) -> Json<DashboardEnvelopeV1<ProjectContextPayloadV1>> {
    let Some(db) = runtime.active.savings_db.as_ref() else {
        return Json(DashboardEnvelopeV1::unavailable(
            scope_from_state(&runtime.active),
            ProjectContextPayloadV1 {
                status: "missing_registry".to_owned(),
                error: None,
                is_active: None,
                project: None,
                aliases: Vec::new(),
                stores: Vec::new(),
            },
            "project_registry_not_mounted",
        ));
    };
    let context = match db.project_registry_context_by_id(&project_id).await {
        Ok(context) => context,
        Err(error) => return registry_unavailable_response(&runtime.active, &error),
    };
    let Some(context) = context else {
        return Json(DashboardEnvelopeV1::complete_zero_findings(
            scope_from_state(&runtime.active),
            DashboardCoverageV1::complete(1, "projects"),
            ProjectContextPayloadV1 {
                status: "not_found".to_owned(),
                error: None,
                is_active: None,
                project: None,
                aliases: Vec::new(),
                stores: Vec::new(),
            },
        ));
    };
    let is_active = Some(project_id.as_str()) == runtime.active_project_id();
    Json(DashboardEnvelopeV1::ready(
        scope_from_state(&runtime.active),
        DashboardCoverageV1::complete(1, "projects"),
        ProjectContextPayloadV1 {
            status: "ok".to_owned(),
            error: None,
            is_active: Some(is_active),
            project: Some(PublicCodeProject::from_record(
                &context.project,
                runtime.active_project_id(),
            )),
            aliases: context.aliases,
            stores: context.stores,
        },
    ))
}

#[cfg(test)]
mod tests {
    use tracedecay_global_db::{
        CodeProjectRecord, GraphScopeRecord, ProjectRegistryContext, ProjectStoreContext,
        StoreArtifactRecord, StoreInstanceRecord,
    };

    fn code_project() -> CodeProjectRecord {
        CodeProjectRecord {
            project_id: "proj_test".to_string(),
            canonical_root: "/repo".to_string(),
            display_root: "/repo".to_string(),
            git_common_dir: Some("/repo/.git".to_string()),
            git_remote_url: Some("https://example.com/repo.git".to_string()),
            default_branch: Some("main".to_string()),
            created_at: 100,
            last_seen_at: 200,
        }
    }

    fn store_context() -> ProjectStoreContext {
        ProjectStoreContext {
            store: StoreInstanceRecord {
                store_id: "store:test".to_string(),
                project_id: "proj_test".to_string(),
                store_kind: "code_project".to_string(),
                storage_mode: "profile_sharded".to_string(),
                store_relpath: "projects/proj_test".to_string(),
                manifest_relpath: Some("projects/proj_test/store_manifest.json".to_string()),
                created_at: 110,
                last_verified_at: Some(210),
                last_write_at: Some(220),
            },
            graph_scopes: vec![GraphScopeRecord {
                graph_scope_id: "store:test:branch:main".to_string(),
                project_id: "proj_test".to_string(),
                store_id: "store:test".to_string(),
                branch_name: "main".to_string(),
                db_relpath: "projects/proj_test/branches/main.db".to_string(),
                parent_scope_id: None,
                last_synced_at: Some(230),
                writable: true,
            }],
            artifacts: vec![StoreArtifactRecord {
                store_id: "store:test".to_string(),
                artifact_kind: "graph_db".to_string(),
                relpath: "projects/proj_test/branches/main.db".to_string(),
                size_bytes: Some(4096),
                schema_version: None,
                updated_at: Some(240),
            }],
        }
    }

    fn registry_context() -> ProjectRegistryContext {
        ProjectRegistryContext {
            project: code_project(),
            aliases: Vec::new(),
            stores: vec![store_context()],
        }
    }

    #[test]
    fn registry_context_changes_with_project_metadata() {
        let base = registry_context();
        let mut changed = registry_context();
        changed.project.canonical_root = "/new-repo".to_string();
        changed.project.last_seen_at += 1;

        assert_ne!(base, changed);
    }

    #[test]
    fn registry_context_changes_with_store_metadata() {
        let base = registry_context();
        let mut changed = registry_context();
        changed.stores[0].store.last_write_at = Some(999);
        changed.stores[0].graph_scopes[0].db_relpath =
            "projects/proj_test/branches/feature.db".to_string();
        changed.stores[0].artifacts[0].updated_at = Some(1000);

        assert_ne!(base, changed);
    }
}
