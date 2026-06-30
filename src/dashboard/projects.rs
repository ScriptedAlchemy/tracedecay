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
use crate::global_db::GlobalDb;
use crate::tracedecay::TraceDecay;

#[derive(Clone)]
pub(crate) struct DashboardRuntime {
    active: DashboardState,
    project_api: Router<DashboardState>,
    project_states: Arc<RwLock<HashMap<String, DashboardState>>>,
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

        if let Some(state) = self.project_states.read().await.get(project_id).cloned() {
            return Ok(SelectedProjectState { state });
        }

        let db = GlobalDb::open()
            .await
            .ok_or_else(|| config_error("could not open tracedecay project registry"))?;
        let context = db
            .project_registry_context_by_id(project_id)
            .await
            .ok_or_else(|| config_error(format!("registered project not found: {project_id}")))?;
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
            return Ok(SelectedProjectState { state: cached });
        }
        project_states.insert(project_id.to_string(), state.clone());
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
