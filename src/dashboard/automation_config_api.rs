//! Dashboard endpoints for project/profile self-improvement automation config.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::util::{http_detail, JsonError};
use super::DashboardState;
use crate::automation::config::{
    effective_config, load_project_config, merge_project_config, save_project_config,
    AutomationConfig, AutomationConfigPatch,
};
use crate::user_config::UserConfig;

type ApiResult = std::result::Result<Json<Value>, JsonError>;

pub(crate) async fn get_config(State(state): State<DashboardState>) -> ApiResult {
    let global = UserConfig::load().automation;
    let project = load_project_or_error(&state).await?;
    config_payload(&state, &global, project.as_ref())
}

pub(crate) async fn patch_config(
    State(state): State<DashboardState>,
    Json(patch): Json<AutomationConfigPatch>,
) -> ApiResult {
    let global = UserConfig::load().automation;
    let current = load_project_or_error(&state).await?;
    let project = merge_project_config(current, patch);
    let effective = effective_config(&global, Some(&project)).map_err(|err| bad_request(&err))?;
    save_project_config(&state.dashboard_root, &project)
        .await
        .map_err(|err| internal_error(&err))?;
    Ok(Json(config_payload_value(
        &state,
        &global,
        Some(&project),
        &effective,
    )))
}

async fn load_project_or_error(
    state: &DashboardState,
) -> std::result::Result<Option<AutomationConfigPatch>, JsonError> {
    load_project_config(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))
}

fn config_payload(
    state: &DashboardState,
    global: &AutomationConfig,
    project: Option<&AutomationConfigPatch>,
) -> ApiResult {
    let effective = effective_config(global, project).map_err(|err| internal_error(&err))?;
    Ok(Json(config_payload_value(
        state, global, project, &effective,
    )))
}

fn config_payload_value(
    state: &DashboardState,
    global: &AutomationConfig,
    project: Option<&AutomationConfigPatch>,
    effective: &AutomationConfig,
) -> Value {
    json!({
        "global": global,
        "project": project,
        "effective": effective,
        "project_config_path": crate::automation::config::project_config_path(&state.dashboard_root)
            .display()
            .to_string(),
    })
}

fn bad_request(err: &impl ToString) -> JsonError {
    (StatusCode::BAD_REQUEST, Json(http_detail(&err.to_string())))
}

fn internal_error(err: &impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&err.to_string())),
    )
}
