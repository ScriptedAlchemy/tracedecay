use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use super::{build_selected_project_state, config_error, DashboardState};
use crate::db::Database;
use crate::errors::Result;
use crate::global_db::{GlobalDb, ProjectRegistryContext};
use crate::storage::{self, StoreArtifactPath};
use crate::tracedecay::TraceDecay;

#[derive(Clone)]
pub(crate) struct DashboardRuntime {
    active: DashboardState,
    project_states: Arc<RwLock<HashMap<String, DashboardState>>>,
    memory_project_states: Arc<RwLock<HashMap<String, DashboardState>>>,
}

impl DashboardRuntime {
    pub(crate) fn new(active: DashboardState) -> Self {
        Self {
            active,
            project_states: Arc::new(RwLock::new(HashMap::new())),
            memory_project_states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub(crate) fn active_state(&self) -> DashboardState {
        self.active.clone()
    }

    pub(crate) fn active_project_id(&self) -> Option<&str> {
        self.active.project_id.as_deref()
    }

    fn active_project_root(&self) -> String {
        self.active.project_root.display().to_string()
    }

    pub(crate) async fn state_for_project(&self, project_id: &str) -> Result<DashboardState> {
        if self.active.project_id.as_deref() == Some(project_id) {
            return Ok(self.active.clone());
        }

        if let Some(state) = self.project_states.read().await.get(project_id).cloned() {
            return Ok(state);
        }

        let db = GlobalDb::open()
            .await
            .ok_or_else(|| config_error("could not open tracedecay project registry"))?;
        let context = db
            .project_registry_context_by_id(project_id)
            .await
            .ok_or_else(|| config_error(format!("registered project not found: {project_id}")))?;
        let project_root = PathBuf::from(context.project.display_root);
        let cg = TraceDecay::open_read_only(&project_root).await?;
        let state = build_selected_project_state(&cg).await;
        self.project_states
            .write()
            .await
            .insert(project_id.to_string(), state.clone());
        Ok(state)
    }

    pub(crate) async fn memory_state_for_project(
        &self,
        project_id: &str,
    ) -> Result<DashboardState> {
        if self.active.project_id.as_deref() == Some(project_id) {
            return Ok(self.active.clone());
        }

        if let Some(state) = self
            .memory_project_states
            .read()
            .await
            .get(project_id)
            .cloned()
        {
            return Ok(state);
        }

        let db = GlobalDb::open()
            .await
            .ok_or_else(|| config_error("could not open tracedecay project registry"))?;
        let context = db
            .project_registry_context_by_id(project_id)
            .await
            .ok_or_else(|| config_error(format!("registered project not found: {project_id}")))?;
        let state = self.memory_state_from_registry(&context).await?;
        self.memory_project_states
            .write()
            .await
            .insert(project_id.to_string(), state.clone());
        Ok(state)
    }

    async fn memory_state_from_registry(
        &self,
        context: &ProjectRegistryContext,
    ) -> Result<DashboardState> {
        let profile_root = storage::default_profile_root()?;
        let store = context
            .stores
            .iter()
            .find(|store| {
                store.store.store_kind == "code_project"
                    && store.store.storage_mode == "profile_sharded"
            })
            .ok_or_else(|| {
                config_error(format!(
                    "registered project has no profile-sharded code-project store: {}",
                    context.project.project_id
                ))
            })?;
        let store_root =
            StoreArtifactPath::resolve(&profile_root, Path::new(&store.store.store_relpath))?
                .absolute_path();
        let graph_db_path = match store.store.manifest_relpath.as_deref() {
            Some(relpath) => {
                let manifest_path =
                    StoreArtifactPath::resolve(&profile_root, Path::new(relpath))?.absolute_path();
                match storage::read_store_manifest(&manifest_path) {
                    Ok(manifest) => store_root.join(manifest.graph_db_relpath),
                    Err(_) => store_root.join(crate::config::db_filename(&store_root)),
                }
            }
            None => store_root.join(crate::config::db_filename(&store_root)),
        };
        let (db, _) = Database::open_read_only(&graph_db_path).await?;
        let conn = db.conn().clone();
        let mut state = self.active.clone();
        state.project_id = Some(context.project.project_id.clone());
        state.graph_conn = conn.clone();
        state.graph_db_path = graph_db_path.display().to_string();
        state.mem_conn = conn;
        state.mem_db_path = graph_db_path.display().to_string();
        state.lcm_conn = None;
        state.lcm_db_path.clear();
        state.lcm_scope.clone_from(&store.store.storage_mode);
        state.project_root = PathBuf::from(&context.project.display_root);
        state.storage_mode.clone_from(&store.store.storage_mode);
        state.store_root.clone_from(&store_root);
        state.dashboard_root = store_root.join("dashboard");
        Ok(state)
    }
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
