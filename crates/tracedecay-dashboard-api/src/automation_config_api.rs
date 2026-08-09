//! Dashboard endpoints for project/profile self-improvement automation config.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::DashboardState;
use super::util::{JsonError, internal_error};
use crate::user_config::UserConfig;
use tracedecay_agent_hosts::automation::backend;
use tracedecay_agent_hosts::automation::config::{
    AutomationBackend, AutomationConfig, AutomationConfigPatch, apply_project_config_patch,
    effective_config, load_project_config, merge_project_config,
};

type ApiResult = std::result::Result<Json<Value>, JsonError>;

pub async fn get_config(State(state): State<DashboardState>) -> ApiResult {
    let global = UserConfig::load().automation;
    let project = load_project_or_error(&state).await?;
    config_payload(&state, &global, project.as_ref())
}

pub async fn patch_config(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<AutomationConfigPatch>(patch)
        .map_err(|err| bad_request(&format!("invalid automation config patch: {err}")))?;
    reject_job_command_enablement(&patch)?;
    reject_unselectable_backend(&patch)?;
    let global = UserConfig::load().automation;
    let current = load_project_or_error(&state).await?;
    let candidate = merge_project_config(current, patch.clone());
    effective_config(&global, Some(&candidate)).map_err(|err| bad_request(&err))?;
    let global_for_write = global.clone();
    let payload = super::automation_run_service::execute_dashboard_automation_write(
        &state,
        move |state| async move {
            let (project, effective) =
                apply_project_config_patch(&state.dashboard_root, &global_for_write, patch)
                    .await
                    .map_err(|err| err.to_string())?;
            state.reconcile_automation_scheduler();
            Ok(config_payload_value(
                &state,
                &global_for_write,
                Some(&project),
                &effective,
            ))
        },
    )
    .await
    .map_err(|err| internal_error(&err))?;
    Ok(Json(payload))
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

fn reject_job_command_enablement(
    patch: &AutomationConfigPatch,
) -> std::result::Result<(), JsonError> {
    if patch.allow_job_commands.is_some() {
        return Err(bad_request(
            &"allow_job_commands cannot be changed through dashboard HTTP; use explicit local operator configuration",
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
        "project_config_path": tracedecay_agent_hosts::automation::config::project_config_path(&state.dashboard_root)
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

fn validation_field(message: &str) -> String {
    if let Some(field) = unknown_field(message) {
        return field;
    }

    for field in [
        "auto_apply_memory_ops",
        "auto_enable_skills",
        "export_memory_digest",
        "backend",
        "host_mode",
        "timeout_secs",
        "scheduler_tick_secs",
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
