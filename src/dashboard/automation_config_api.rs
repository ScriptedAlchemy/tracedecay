//! Dashboard endpoints for project/profile self-improvement automation config.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::util::{http_detail, JsonError};
use super::DashboardState;
use crate::automation::backend;
use crate::automation::config::{
    clear_project_config, effective_config, load_project_config, merge_project_config,
    save_project_config, AutomationBackend, AutomationConfig, AutomationConfigPatch,
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
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<AutomationConfigPatch>(patch)
        .map_err(|err| bad_request(&format!("invalid automation config patch: {err}")))?;
    reject_unselectable_backend(&patch)?;
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

pub(crate) async fn reset_config(State(state): State<DashboardState>) -> ApiResult {
    let global = UserConfig::load().automation;
    clear_project_config(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))?;
    config_payload(&state, &global, None)
}

async fn load_project_or_error(
    state: &DashboardState,
) -> std::result::Result<Option<AutomationConfigPatch>, JsonError> {
    load_project_config(&state.dashboard_root)
        .await
        .map_err(|err| internal_error(&err))
}

fn reject_unselectable_backend(
    patch: &AutomationConfigPatch,
) -> std::result::Result<(), JsonError> {
    if patch.backend == Some(AutomationBackend::ExternalCommand) {
        return Err(bad_request(
            &"automation backend external_command is not selectable yet; use disabled or codex_app_server",
        ));
    }
    Ok(())
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
        "backend_availability": backend::backend_availability(effective),
        "project_config_path": crate::automation::config::project_config_path(&state.dashboard_root)
            .display()
            .to_string(),
    })
}

fn bad_request(err: &impl ToString) -> JsonError {
    let message = err.to_string();
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "detail": message,
            "validation_errors": [{
                "field": validation_field(&message),
                "message": message,
            }],
        })),
    )
}

fn internal_error(err: &impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&err.to_string())),
    )
}

fn validation_field(message: &str) -> String {
    if let Some(field) = unknown_field(message) {
        return field;
    }

    for field in [
        "auto_apply_memory_ops",
        "auto_enable_skills",
        "require_dashboard_approval",
        "backend",
        "host_mode",
        "timeout_secs",
        "scheduler_tick_secs",
        "max_tokens",
        "temperature",
    ] {
        if message.contains(field) {
            return field.to_string();
        }
    }
    if message.contains("standalone") && message.contains("delegated_host") {
        return "host_mode".to_string();
    }

    for task in ["memory_curator", "session_reflector", "skill_writer"] {
        if !message.contains(task) {
            continue;
        }
        for field in [
            "schedule",
            "interval_secs",
            "cooldown_secs",
            "min_idle_secs",
            "stale_lock_secs",
        ] {
            if message.contains(field) {
                return format!("{task}.{field}");
            }
        }
        return task.to_string();
    }

    "config".to_string()
}

fn unknown_field(message: &str) -> Option<String> {
    let start = message.find("unknown field `")? + "unknown field `".len();
    let rest = &message[start..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}
