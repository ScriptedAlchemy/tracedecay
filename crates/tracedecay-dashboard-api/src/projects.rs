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
use super::{
    DashboardSessionAuthorityStateV1, DashboardSessionResolutionV1, DashboardState,
    build_selected_project_state, config_error,
};
use crate::project_registry::{
    PublicCodeProject, align_public_checkout_branches, build_project_registry_view,
    public_code_project_for_checkout, public_code_project_from_record,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::ProjectRegistryContext;

#[derive(Clone)]
pub struct DashboardRuntime {
    /// Replaced once, when a still-opening project's session authorities
    /// mount; every other field of the active state is fixed at composition.
    active: Arc<std::sync::RwLock<DashboardState>>,
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
            active: Arc::new(std::sync::RwLock::new(active)),
            project_api,
            project_states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn active(&self) -> DashboardState {
        self.active
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The active project's state. While its session authorities are still
    /// opening, each call asks the daemon again and mounts them the first
    /// time it answers ready.
    pub async fn active_state(&self) -> DashboardState {
        let current = self.active();
        let Some(resolve) = current.session_resolver.clone() else {
            return current;
        };
        match resolve().await {
            DashboardSessionResolutionV1::Opening => current,
            DashboardSessionResolutionV1::Unavailable => DashboardState {
                session_authority: DashboardSessionAuthorityStateV1::Unavailable,
                ..current
            },
            DashboardSessionResolutionV1::Ready(authorities) => {
                let (mounted, newly_mounted) = {
                    let mut active = self
                        .active
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let newly_mounted = active.session_resolver.is_some();
                    if newly_mounted {
                        active.mount_session_authorities(authorities);
                    }
                    (active.clone(), newly_mounted)
                };
                if newly_mounted {
                    tracing::info!(
                        event = "dashboard_session_authorities_mounted",
                        project_root = %mounted.project_root.display(),
                    );
                    crate::token_count::spawn_warm(mounted.clone());
                }
                mounted
            }
        }
    }

    pub fn active_project_id(&self) -> Option<String> {
        self.active().project_id
    }

    pub fn project_api_router(&self) -> Router<DashboardState> {
        self.project_api.clone()
    }

    pub async fn selected_project_state(&self, project_id: &str) -> Result<SelectedProjectState> {
        let active = self.active();
        if active.project_id.as_deref() == Some(project_id) {
            return Ok(SelectedProjectState {
                state: self.active_state().await,
            });
        }

        let db = active
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
        let resolver = active.project_graph_resolver.as_ref().ok_or_else(|| {
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
        if cg.store_layout.identity.project_id.as_deref() != Some(project_id) {
            return Err(config_error(format!(
                "registered project id mismatch for {project_id}: {}",
                project_root.display()
            )));
        }
        let state = build_selected_project_state(cg, &active).await?;
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
}

#[hotpath::measure(label = "dashboard_api.projects.list", future = true)]
pub async fn list(
    State(runtime): State<DashboardRuntime>,
    Query(params): Query<ProjectsParams>,
) -> Json<DashboardEnvelopeV1<ProjectsPayloadV1>> {
    let active = runtime.active();
    let limit = params.limit.unwrap_or(100).clamp(1, 250);
    let Some(db) = active.savings_db.as_ref() else {
        return registry_list_unavailable(
            &active,
            ProjectsPayloadV1 {
                status: "missing_registry".to_owned(),
                error: None,
                limit,
                truncated: None,
                projects: None,
                active_project_id: active.project_id.clone(),
                active_project_root: active.project_root.display().to_string(),
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
                &active,
                ProjectsPayloadV1 {
                    status: "registry_unavailable".to_owned(),
                    error: Some(error.to_string()),
                    limit,
                    truncated: None,
                    projects: None,
                    active_project_id: active.project_id.clone(),
                    active_project_root: active.project_root.display().to_string(),
                    summary: None,
                    project_tree: None,
                },
                error.to_string(),
            );
        }
    };
    let truncated = projects.len() > limit;
    projects.truncate(limit);
    let active_project_id = active.project_id.clone();
    let contexts = match db.project_registry_contexts_for_projects(&projects).await {
        Ok(contexts) => contexts,
        Err(error) => {
            return registry_list_unavailable(
                &active,
                ProjectsPayloadV1 {
                    status: "registry_unavailable".to_owned(),
                    error: Some(error.to_string()),
                    limit,
                    truncated: None,
                    projects: None,
                    active_project_id,
                    active_project_root: active.project_root.display().to_string(),
                    summary: None,
                    project_tree: None,
                },
                error.to_string(),
            );
        }
    };
    let view = build_project_registry_view(
        &contexts,
        active.project_id.as_deref(),
        Some(active.project_root.as_path()),
        truncated,
    );
    let mut rows = projects
        .iter()
        .map(|project| public_code_project_from_record(project, active.project_id.as_deref()))
        .collect::<Vec<_>>();
    align_public_checkout_branches(&mut rows, &view);
    let row_count = rows.len() as u64;

    let payload = ProjectsPayloadV1 {
        status: "ok".to_owned(),
        error: None,
        limit,
        truncated: Some(truncated),
        projects: Some(rows),
        active_project_id,
        active_project_root: active.project_root.display().to_string(),
        summary: Some(view.summary),
        project_tree: Some(view.project_tree),
    };
    let envelope = if truncated {
        DashboardEnvelopeV1::new(
            scope_from_state(&active),
            DashboardDomainStateV1::Partial,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::fresh_now(),
            payload,
        )
    } else {
        DashboardEnvelopeV1::ready(
            scope_from_state(&active),
            DashboardCoverageV1::complete(row_count, "projects"),
            payload,
        )
    };
    Json(envelope)
}

fn registry_list_unavailable(
    active: &DashboardState,
    payload: ProjectsPayloadV1,
    reason: impl Into<String>,
) -> Json<DashboardEnvelopeV1<ProjectsPayloadV1>> {
    Json(DashboardEnvelopeV1::unavailable(
        scope_from_state(active),
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
        },
        error.to_string(),
    ))
}

#[hotpath::measure(label = "dashboard_api.projects.context", future = true)]
pub async fn context(
    State(runtime): State<DashboardRuntime>,
    AxumPath(project_id): AxumPath<String>,
) -> Json<DashboardEnvelopeV1<ProjectContextPayloadV1>> {
    let active = runtime.active();
    let Some(db) = active.savings_db.as_ref() else {
        return Json(DashboardEnvelopeV1::unavailable(
            scope_from_state(&active),
            ProjectContextPayloadV1 {
                status: "missing_registry".to_owned(),
                error: None,
                is_active: None,
                project: None,
                aliases: Vec::new(),
            },
            "project_registry_not_mounted",
        ));
    };
    let context = match db.project_registry_context_by_id(&project_id).await {
        Ok(context) => context,
        Err(error) => return registry_unavailable_response(&active, &error),
    };
    let Some(context) = context else {
        return Json(DashboardEnvelopeV1::complete_zero_findings(
            scope_from_state(&active),
            DashboardCoverageV1::complete(1, "projects"),
            ProjectContextPayloadV1 {
                status: "not_found".to_owned(),
                error: None,
                is_active: None,
                project: None,
                aliases: Vec::new(),
            },
        ));
    };
    let is_active = Some(project_id.as_str()) == active.project_id.as_deref();
    Json(DashboardEnvelopeV1::ready(
        scope_from_state(&active),
        DashboardCoverageV1::complete(1, "projects"),
        ProjectContextPayloadV1 {
            status: "ok".to_owned(),
            error: None,
            is_active: Some(is_active),
            project: Some(public_code_project_for_checkout(
                &context.project,
                &context.aliases,
                active.project_id.as_deref(),
                is_active.then_some(active.project_root.as_path()),
            )),
            aliases: context.aliases,
        },
    ))
}
