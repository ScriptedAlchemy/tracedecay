use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::RwLock;

use super::{DashboardState, config_error};
use crate::errors::Result;

#[derive(Clone)]
pub struct DashboardRuntime {
    active: DashboardState,
    project_api: Router<DashboardState>,
    project_states: Arc<RwLock<HashMap<String, CachedProjectState>>>,
}

#[derive(Clone)]
struct CachedProjectState {
    cache_key: String,
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

        let registry = self
            .active
            .project_registry
            .as_ref()
            .ok_or_else(|| config_error("could not open tracedecay project registry"))?;
        let context = registry
            .context(project_id.to_string(), self.active.project_id.clone())
            .await
            .map_err(|_| config_error("could not open tracedecay project registry"))?
            .ok_or_else(|| config_error(format!("registered project not found: {project_id}")))?;
        if let Some(cached) = self.project_states.read().await.get(project_id).cloned() {
            if cached.cache_key == context.cache_key {
                return Ok(SelectedProjectState {
                    state: cached.state,
                });
            }
        }
        let state_builder = self
            .active
            .project_state_builder
            .as_ref()
            .ok_or_else(|| config_error("dashboard project selection is unavailable"))?;
        let state = state_builder(
            project_id.to_string(),
            context.project_root.clone(),
            self.active.clone(),
        )
        .await?;
        let mut project_states = self.project_states.write().await;
        if let Some(cached) = project_states.get(project_id).cloned() {
            if cached.cache_key == context.cache_key {
                return Ok(SelectedProjectState {
                    state: cached.state,
                });
            }
        }
        project_states.insert(
            project_id.to_string(),
            CachedProjectState {
                cache_key: context.cache_key,
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

pub async fn list(
    State(runtime): State<DashboardRuntime>,
    Query(params): Query<ProjectsParams>,
) -> Json<Value> {
    let limit = params.limit.unwrap_or(100).clamp(1, 250);
    let Some(registry) = runtime.active.project_registry.as_ref() else {
        return Json(json!({
            "status": "missing_registry",
            "limit": limit,
            "truncated": false,
            "projects": [],
            "active_project_id": runtime.active_project_id(),
            "active_project_root": runtime.active_project_root(),
            "summary": {
                "project_count": 0,
                "repo_count": 0,
                "truncated": false,
            },
            "project_tree": [],
        }));
    };

    let active_project_id = runtime.active_project_id().map(str::to_string);
    let Ok(view) = registry.list(limit, active_project_id.clone()).await else {
        return Json(json!({
            "status": "missing_registry",
            "limit": limit,
            "truncated": false,
            "projects": [],
            "active_project_id": runtime.active_project_id(),
            "active_project_root": runtime.active_project_root(),
            "summary": {
                "project_count": 0,
                "repo_count": 0,
                "truncated": false,
            },
            "project_tree": [],
        }));
    };

    Json(json!({
        "status": "ok",
        "limit": limit,
        "truncated": view.truncated,
        "active_project_id": active_project_id,
        "active_project_root": runtime.active_project_root(),
        "summary": view.summary,
        "project_tree": view.project_tree,
        "projects": view.projects,
    }))
}

pub async fn context(
    State(runtime): State<DashboardRuntime>,
    AxumPath(project_id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    let Some(registry) = runtime.active.project_registry.as_ref() else {
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
    let Ok(context) = registry
        .context(project_id.clone(), runtime.active.project_id.clone())
        .await
    else {
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
    let Some(context) = context else {
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
            "project": context.payload.get("project").cloned().unwrap_or(Value::Null),
            "aliases": context.payload.get("aliases").cloned().unwrap_or_else(|| json!([])),
            "stores": context.payload.get("stores").cloned().unwrap_or_else(|| json!([])),
        })),
    )
}
