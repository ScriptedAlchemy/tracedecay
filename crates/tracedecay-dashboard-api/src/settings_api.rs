//! Dashboard endpoints for project and user settings.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::DashboardState;
use super::util::{JsonError, http_detail};
use crate::automation::config as automation_config;
use crate::config::{
    TelemetryConfig, TraceDecayConfig, load_config_from_path, save_config_to_path,
};
use crate::user_config::{self, UserConfig};

type ApiResult = std::result::Result<Json<Value>, JsonError>;

const AUTOMATION_CONFIG_ENDPOINT: &str = "/api/plugins/holographic/curation/config";

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ProjectSettingsPatch {
    #[serde(default)]
    include: Option<Vec<String>>,
    #[serde(default)]
    exclude: Option<Vec<String>>,
    #[serde(default)]
    max_file_size: Option<u64>,
    #[serde(default)]
    extract_docstrings: Option<bool>,
    #[serde(default)]
    track_call_sites: Option<bool>,
    #[serde(default)]
    git_ignore: Option<bool>,
    #[serde(default)]
    telemetry: Option<TelemetrySettingsPatch>,
    #[serde(default)]
    sync: Option<SyncSettingsPatch>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SyncSettingsPatch {
    #[serde(default)]
    auto_track_pr_branches: Option<bool>,
    #[serde(default)]
    auto_track_pr_poll_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TelemetrySettingsPatch {
    #[serde(default)]
    timings: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct UserSettingsPatch {
    #[serde(default)]
    upload_enabled: Option<bool>,
    #[serde(default)]
    watcher_debounce: Option<String>,
    #[serde(default)]
    extraction_timeout_secs: Option<u64>,
}

pub async fn get_settings(State(state): State<DashboardState>) -> ApiResult {
    Ok(Json(settings_payload(&state).await?))
}

pub async fn patch_project_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<ProjectSettingsPatch>(patch)
        .map_err(|err| patch_shape_error("project settings", &err))?;

    let mut errors = Vec::new();
    if let Some(globs) = &patch.include {
        validate_globs("include", globs, &mut errors);
    }
    if let Some(globs) = &patch.exclude {
        validate_globs("exclude", globs, &mut errors);
    }
    if patch.max_file_size == Some(0) {
        errors.push(validation_error(
            "max_file_size",
            "max_file_size must be at least 1 byte",
        ));
    }
    if let Some(sync) = &patch.sync {
        if let Some(secs) = sync.auto_track_pr_poll_secs {
            if secs < crate::config::MIN_AUTO_TRACK_PR_POLL_SECS {
                errors.push(validation_error(
                    "auto_track_pr_poll_secs",
                    &format!(
                        "auto_track_pr_poll_secs must be at least {} seconds",
                        crate::config::MIN_AUTO_TRACK_PR_POLL_SECS
                    ),
                ));
            }
        }
    }
    if !errors.is_empty() {
        return Err(validation_failed(&errors));
    }

    let current = load_config_from_path(&state.project_root, &state.config_path)
        .map_err(|err| internal_error(&err))?;
    let sync = patch.sync.as_ref().map_or_else(
        || current.sync.clone(),
        |sync| crate::config::SyncConfig {
            auto_track_pr_branches: sync
                .auto_track_pr_branches
                .unwrap_or(current.sync.auto_track_pr_branches),
            auto_track_pr_poll_secs: sync
                .auto_track_pr_poll_secs
                .unwrap_or(current.sync.auto_track_pr_poll_secs),
            ..current.sync.clone()
        },
    );
    let telemetry = patch.telemetry.map_or_else(
        || current.telemetry.clone(),
        |telemetry| TelemetryConfig {
            timings: telemetry.timings.unwrap_or(current.telemetry.timings),
        },
    );
    let updated = TraceDecayConfig {
        include: patch.include.unwrap_or_else(|| current.include.clone()),
        exclude: patch.exclude.unwrap_or_else(|| current.exclude.clone()),
        max_file_size: patch.max_file_size.unwrap_or(current.max_file_size),
        extract_docstrings: patch
            .extract_docstrings
            .unwrap_or(current.extract_docstrings),
        track_call_sites: patch.track_call_sites.unwrap_or(current.track_call_sites),
        git_ignore: patch.git_ignore.unwrap_or(current.git_ignore),
        telemetry,
        sync,
        ..current.clone()
    };
    let resync_recommended = updated != current;
    if resync_recommended {
        save_config_to_path(&state.config_path, &updated).map_err(|err| internal_error(&err))?;
    }

    let mut payload = settings_payload(&state).await?;
    payload["resync_recommended"] = json!(resync_recommended);
    Ok(Json(payload))
}

pub async fn patch_user_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<UserSettingsPatch>(patch)
        .map_err(|err| patch_shape_error("user settings", &err))?;

    let mut errors = Vec::new();
    if let Some(debounce) = &patch.watcher_debounce {
        if user_config::parse_duration(debounce).is_none() {
            errors.push(validation_error(
                "watcher_debounce",
                "watcher_debounce must be a duration like \"2s\", \"15s\", or \"1m\"",
            ));
        }
    }
    if patch.extraction_timeout_secs == Some(0) {
        errors.push(validation_error(
            "extraction_timeout_secs",
            "extraction_timeout_secs must be at least 1 second",
        ));
    }
    if !errors.is_empty() {
        return Err(validation_failed(&errors));
    }

    let mut config = UserConfig::load();
    let restart_recommended = patch
        .watcher_debounce
        .as_ref()
        .is_some_and(|value| *value != config.watcher_debounce)
        || patch
            .extraction_timeout_secs
            .is_some_and(|value| value != config.extraction_timeout_secs);
    if let Some(upload_enabled) = patch.upload_enabled {
        config.upload_enabled = upload_enabled;
    }
    if let Some(debounce) = patch.watcher_debounce {
        config.watcher_debounce = debounce;
    }
    if let Some(timeout) = patch.extraction_timeout_secs {
        config.extraction_timeout_secs = timeout;
    }
    match config.save_with_recovery() {
        Ok(Some(backup)) => {
            eprintln!(
                "[tracedecay] note: corrupt config.toml backed up to {} before regenerating",
                backup.display()
            );
        }
        Ok(None) => {}
        Err(err) => {
            return Err(internal_error(&format!(
                "failed to save user config: {err}"
            )));
        }
    }

    let mut payload = settings_payload(&state).await?;
    payload["restart_recommended"] = json!(restart_recommended);
    Ok(Json(payload))
}

async fn settings_payload(state: &DashboardState) -> std::result::Result<Value, JsonError> {
    let project_config = load_config_from_path(&state.project_root, &state.config_path)
        .map_err(|err| internal_error(&err))?;
    let project_config_path = state.config_path.clone();
    let user = UserConfig::load();
    let user_config_path = user_config::config_path()
        .map(|path| path.display().to_string())
        .unwrap_or_default();

    let global_automation = user.automation.clone();
    let project_automation = automation_config::load_project_config(&state.dashboard_root)
        .await
        .ok()
        .flatten();
    let automation =
        automation_config::effective_config(&global_automation, project_automation.as_ref())
            .unwrap_or(global_automation);

    Ok(json!({
        "project": {
            "config_path": project_config_path.display().to_string(),
            "config": project_config,
            "tracedecay_dir_gitignored": crate::config::is_in_gitignore(&state.project_root),
            "pr_autotrack": pr_autotrack_payload(state),
        },
        "user": {
            "config_path": user_config_path,
            "upload_enabled": user.upload_enabled,
            "watcher_debounce": user.watcher_debounce,
            "extraction_timeout_secs": user.extraction_timeout_secs,
            "installed_agents": user.installed_agents,
        },
        "automation": {
            "config_endpoint": AUTOMATION_CONFIG_ENDPOINT,
            "enabled": automation.enabled,
            "backend": automation.backend,
            "host_mode": automation.host_mode,
        },
        "environment": environment_payload(state),
        "storage": {
            "project_id": state.project_id,
            "project_root": state.project_root.display().to_string(),
            "storage_mode": state.storage_mode,
            "store_root": state.store_root.display().to_string(),
            "dashboard_root": state.dashboard_root.display().to_string(),
            "graph_db": state.graph_db_path,
            "memory_db": state.mem_db_path,
            "lcm_db": state.lcm_db_path,
            "lcm_scope": state.lcm_scope,
            "savings_db": state.savings_db_path,
        },
        "version": {
            "version": state.product_version,
            "channel": state.release_channel,
            "cached_latest_version": non_empty(&user.cached_latest_version),
        },
    }))
}

/// Lists the PR branches the daemon currently auto-tracks for this project, read
/// from the store's PR-autotrack state sidecar. Empty on non-unix or when the
/// feature has tracked nothing yet.
fn pr_autotrack_payload(state: &DashboardState) -> Value {
    let tracked = state
        .pr_autotrack_reader
        .as_ref()
        .map_or_else(Vec::new, |reader| reader(state.store_root.clone()));
    json!({ "tracked": tracked })
}

fn environment_payload(state: &DashboardState) -> Value {
    let pricing_offline =
        std::env::var("TRACEDECAY_OFFLINE").is_ok_and(|v| !v.is_empty() && v != "0");
    json!({
        "global_accounting_mode": state.accounting_mode.source,
        "global_accounting_enabled": state.accounting_mode.enabled,
        "pricing_offline": pricing_offline,
        "variables": [
            env_variable(
                "TRACEDECAY_ENABLE_GLOBAL_DB",
                "Force-enables (truthy) or disables (falsy) global savings-ledger recording. Wins over TRACEDECAY_DISABLE_GLOBAL_DB.",
            ),
            env_variable(
                "TRACEDECAY_DISABLE_GLOBAL_DB",
                "A truthy value disables global savings/accounting recording.",
            ),
            env_variable(
                "TRACEDECAY_OFFLINE",
                "Skips network pricing fetches; the Savings tab uses cached or fallback model prices.",
            ),
            env_variable(
                "TRACEDECAY_GLOBAL_DB",
                "Pins the global accounting database to an explicit path.",
            ),
            env_variable(
                "TRACEDECAY_DATA_DIR",
                "Pins the user-level TraceDecay data directory (default ~/.tracedecay).",
            ),
        ],
    })
}

fn env_variable(name: &str, description: &str) -> Value {
    let value = std::env::var(name).ok().filter(|value| !value.is_empty());
    json!({
        "name": name,
        "active": value.is_some(),
        "value": value,
        "description": description,
    })
}

fn non_empty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn validate_globs(field: &str, globs: &[String], errors: &mut Vec<Value>) {
    for pattern in globs {
        if pattern.trim().is_empty() {
            errors.push(validation_error(
                field,
                &format!("{field} patterns must not be empty"),
            ));
            continue;
        }
        if let Err(err) = glob::Pattern::new(pattern) {
            errors.push(validation_error(
                field,
                &format!("invalid glob pattern '{pattern}': {err}"),
            ));
        }
    }
}

fn validation_error(field: &str, message: &str) -> Value {
    json!({ "field": field, "message": message })
}

fn validation_failed(errors: &[Value]) -> JsonError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "detail": "settings validation failed",
            "validation_errors": errors,
        })),
    )
}

fn patch_shape_error(scope: &str, err: &serde_json::Error) -> JsonError {
    let message = format!("invalid {scope} patch: {err}");
    let field = unknown_field(&message).unwrap_or_else(|| "patch".to_string());
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "detail": message,
            "validation_errors": [{ "field": field, "message": message }],
        })),
    )
}

fn unknown_field(message: &str) -> Option<String> {
    let start = message.find("unknown field `")? + "unknown field `".len();
    let rest = &message[start..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

fn internal_error(err: &impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&err.to_string())),
    )
}
