use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use super::{build_selected_project_state, config_error, DashboardState};
use crate::errors::Result;
use crate::global_db::{GlobalDb, ProjectRegistryContext};
use crate::tracedecay::TraceDecay;

#[derive(Clone)]
pub(crate) struct DashboardRuntime {
    active: DashboardState,
    project_api: Router<DashboardState>,
    project_states: Arc<RwLock<HashMap<String, CachedProjectState>>>,
}

#[derive(Clone)]
struct CachedProjectState {
    fingerprint: ProjectCacheFingerprint,
    state: DashboardState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectCacheFingerprint(ProjectRegistryContext);

impl ProjectCacheFingerprint {
    fn from_registry_context(context: &ProjectRegistryContext) -> Self {
        Self(context.clone())
    }
}

impl DashboardRuntime {
    pub(crate) fn new(active: DashboardState, project_api: Router<DashboardState>) -> Self {
        Self {
            active,
            project_api,
            project_states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub(crate) fn active_state(&self) -> DashboardState {
        self.active.clone()
    }

    pub(crate) fn active_project_id(&self) -> Option<&str> {
        self.active.project_id.as_deref()
    }

    pub(crate) fn project_api_router(&self) -> Router<DashboardState> {
        self.project_api.clone()
    }

    fn active_project_root(&self) -> String {
        self.active.project_root.display().to_string()
    }

    pub(crate) async fn selected_project_state(
        &self,
        project_id: &str,
    ) -> Result<SelectedProjectState> {
        if self.active.project_id.as_deref() == Some(project_id) {
            return Ok(SelectedProjectState {
                state: self.active.clone(),
            });
        }

        let db = GlobalDb::open()
            .await
            .ok_or_else(|| config_error("could not open tracedecay project registry"))?;
        let context = db
            .project_registry_context_by_id(project_id)
            .await
            .ok_or_else(|| config_error(format!("registered project not found: {project_id}")))?;
        let fingerprint = ProjectCacheFingerprint::from_registry_context(&context);
        if let Some(cached) = self.project_states.read().await.get(project_id).cloned() {
            if cached.fingerprint == fingerprint {
                return Ok(SelectedProjectState {
                    state: cached.state,
                });
            }
        }
        let project_root = PathBuf::from(&context.project.canonical_root);
        let cg = TraceDecay::open_read_only(&project_root).await?;
        if cg.store_layout().identity.project_id.as_deref() != Some(project_id) {
            return Err(config_error(format!(
                "registered project id mismatch for {project_id}: {}",
                project_root.display()
            )));
        }
        let state = build_selected_project_state(&cg).await;
        let mut project_states = self.project_states.write().await;
        if let Some(cached) = project_states.get(project_id).cloned() {
            if cached.fingerprint == fingerprint {
                return Ok(SelectedProjectState {
                    state: cached.state,
                });
            }
        }
        project_states.insert(
            project_id.to_string(),
            CachedProjectState {
                fingerprint,
                state: state.clone(),
            },
        );
        Ok(SelectedProjectState { state })
    }
}

pub(crate) struct SelectedProjectState {
    pub(crate) state: DashboardState,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProjectsParams {
    limit: Option<usize>,
}

pub(crate) async fn list(
    State(runtime): State<DashboardRuntime>,
    Query(params): Query<ProjectsParams>,
) -> Json<Value> {
    let limit = params.limit.unwrap_or(100).clamp(1, 250);
    let Some(db) = GlobalDb::open().await else {
        return Json(json!({
            "status": "missing_registry",
            "projects": [],
            "active_project_id": runtime.active_project_id(),
            "active_project_root": runtime.active_project_root(),
        }));
    };

    let mut projects = db.list_code_projects(limit + 1).await;
    let truncated = projects.len() > limit;
    projects.truncate(limit);
    let active_project_id = runtime.active_project_id().map(str::to_string);
    let rows = projects
        .into_iter()
        .map(|project| {
            let is_active = Some(project.project_id.as_str()) == runtime.active_project_id();
            let label = Path::new(&project.display_root)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(project.display_root.as_str())
                .to_string();
            json!({
                "project_id": project.project_id,
                "label": label,
                "project_root": project.display_root,
                "canonical_root": project.canonical_root,
                "default_branch": project.default_branch,
                "last_seen_at": project.last_seen_at,
                "is_active": is_active,
            })
        })
        .collect::<Vec<_>>();

    Json(json!({
        "status": "ok",
        "limit": limit,
        "truncated": truncated,
        "active_project_id": active_project_id,
        "active_project_root": runtime.active_project_root(),
        "projects": rows,
    }))
}

pub(crate) async fn context(
    State(runtime): State<DashboardRuntime>,
    AxumPath(project_id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    let Some(db) = GlobalDb::open().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "missing_registry",
                "project": null,
                "aliases": [],
                "stores": [],
            })),
        );
    };
    let Some(context) = db.project_registry_context_by_id(&project_id).await else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "status": "not_found",
                "project": null,
                "aliases": [],
                "stores": [],
            })),
        );
    };
    let is_active = Some(project_id.as_str()) == runtime.active_project_id();
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "is_active": is_active,
            "project": context.project,
            "aliases": context.aliases,
            "stores": context.stores,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_db::{
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
    fn project_cache_fingerprint_changes_with_project_metadata() {
        let base = registry_context();
        let mut changed = registry_context();
        changed.project.canonical_root = "/new-repo".to_string();
        changed.project.last_seen_at += 1;

        assert_ne!(
            ProjectCacheFingerprint::from_registry_context(&base),
            ProjectCacheFingerprint::from_registry_context(&changed)
        );
    }

    #[test]
    fn project_cache_fingerprint_changes_with_store_metadata() {
        let base = registry_context();
        let mut changed = registry_context();
        changed.stores[0].store.last_write_at = Some(999);
        changed.stores[0].graph_scopes[0].db_relpath =
            "projects/proj_test/branches/feature.db".to_string();
        changed.stores[0].artifacts[0].updated_at = Some(1000);

        assert_ne!(
            ProjectCacheFingerprint::from_registry_context(&base),
            ProjectCacheFingerprint::from_registry_context(&changed)
        );
    }
}
