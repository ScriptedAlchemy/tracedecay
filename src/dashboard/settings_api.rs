//! Dashboard endpoints for project and user settings.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tracedecay_application::{ApplicationProblemKind, RequestId};

use super::DashboardState;
use super::util::{JsonError, http_detail};
use crate::application::configuration::DirectConfigurationMutation;
use crate::application::settings_control::{
    ProjectSettingsPatchV1, ProjectSettingsPreviewErrorV1, UserSettingsOperationErrorV1,
    UserSettingsPatchV1, UserSettingsPreviewV1, apply_user_settings, preview_project_settings,
    preview_user_settings, rollback_user_settings, user_settings_status,
};
use crate::application_surface::{
    ApplicationSurfaceOperation, ApplicationSurfaceRequest, ConfigurationBatchSurfaceRequest,
    ConfigurationDirectMutationSurfaceRequest, ConfigurationSurfaceRequest,
    resolve_dashboard_application_surface,
};
use crate::automation::config as automation_config;
use crate::daemon_client::RequestedOutputFormat;
use crate::user_config::{self, UserConfig};
use tracedecay_domain::ManifestDigest;

type ApiResult = std::result::Result<Json<Value>, JsonError>;

const AUTOMATION_CONFIG_ENDPOINT: &str = "/api/plugins/holographic/curation/config";
static NEXT_SETTINGS_REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UserSettingsApplyBodyV1 {
    preview: UserSettingsPreviewV1,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UserSettingsRollbackBodyV1 {
    expected_revision_id: String,
}

pub(crate) async fn get_settings(State(state): State<DashboardState>) -> ApiResult {
    Ok(Json(settings_payload(&state).await?))
}

pub(crate) async fn patch_project_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<ProjectSettingsPatchV1>(patch)
        .map_err(|err| patch_shape_error("project settings", &err))?;
    let current = crate::config::cached_runtime_configuration(&state.project_root)
        .map_err(|err| configuration_unavailable(&err))?;
    let project_id = state
        .project_id
        .as_deref()
        .ok_or_else(|| configuration_unavailable(&"project authority is unavailable"))
        .and_then(|project_id| {
            tracedecay_domain::ProjectId::new(project_id)
                .map_err(|error| configuration_unavailable(&error))
        })?;
    let preview =
        preview_project_settings(&project_id, &current, patch).map_err(project_preview_error)?;
    let mut operation = None;
    if preview.changed {
        let client = state
            .application_client
            .as_ref()
            .ok_or_else(|| configuration_unavailable(&"application transport is unavailable"))?;
        let mutations = match preview.mutation {
            DirectConfigurationMutation::Batch { mutations } => mutations
                .into_iter()
                .map(surface_mutation)
                .collect::<std::result::Result<Vec<_>, _>>()?,
            _ => {
                return Err(configuration_unavailable(
                    &"project settings preview is invalid",
                ));
            }
        };
        let sequence = NEXT_SETTINGS_REQUEST.fetch_add(1, Ordering::Relaxed);
        let request_id = RequestId::new(format!(
            "request.dashboard.settings.{}.{}",
            crate::tracedecay::current_timestamp(),
            sequence
        ))
        .map_err(|error| configuration_unavailable(&error))?;
        let outcome = resolve_dashboard_application_surface(
            ApplicationSurfaceOperation::ConfigurationBatch,
            request_id,
            ApplicationSurfaceRequest::Configuration(ConfigurationSurfaceRequest::Batch(
                ConfigurationBatchSurfaceRequest {
                    mutations,
                    expected_revision: preview.expected_revision,
                },
            )),
            RequestedOutputFormat::Json,
            Some(client),
        )
        .await
        .map_err(|error| configuration_unavailable(&error))?;
        match outcome.result {
            Ok(receipt) => {
                operation = Some(
                    serde_json::to_value(receipt)
                        .map_err(|error| configuration_unavailable(&error))?,
                );
            }
            Err(problem) => return Err(application_problem(problem)),
        }
    }

    let mut payload = settings_payload(&state).await?;
    payload["resync_recommended"] = json!(preview.resync_recommended);
    if let Some(operation) = operation {
        payload["operation"] = operation;
    }
    Ok(Json(payload))
}

pub(crate) async fn patch_user_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<UserSettingsPatchV1>(patch)
        .map_err(|err| patch_shape_error("user settings", &err))?;
    let preview =
        preview_user_settings(&UserConfig::load(), patch).map_err(user_operation_error)?;
    let receipt = apply_user_settings(preview).map_err(user_operation_error)?;

    let mut payload = settings_payload(&state).await?;
    payload["restart_recommended"] = json!(receipt.restart_recommended);
    payload["operation"] = json!(receipt);
    Ok(Json(payload))
}

pub(crate) async fn preview_user_settings_route(
    State(_state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<UserSettingsPatchV1>(patch)
        .map_err(|error| patch_shape_error("user settings", &error))?;
    let preview =
        preview_user_settings(&UserConfig::load(), patch).map_err(user_operation_error)?;
    Ok(Json(json!({ "preview": preview })))
}

pub(crate) async fn apply_user_settings_route(
    State(state): State<DashboardState>,
    Json(body): Json<UserSettingsApplyBodyV1>,
) -> ApiResult {
    let receipt = apply_user_settings(body.preview).map_err(user_operation_error)?;
    let mut payload = settings_payload(&state).await?;
    payload["restart_recommended"] = json!(receipt.restart_recommended);
    payload["operation"] = json!(receipt);
    Ok(Json(payload))
}

pub(crate) async fn user_settings_operation_status(
    State(_state): State<DashboardState>,
    axum::extract::Path(operation_id): axum::extract::Path<String>,
) -> ApiResult {
    let operation_id = ManifestDigest::new(operation_id).map_err(|error| {
        user_operation_error(UserSettingsOperationErrorV1::Unavailable(error.to_string()))
    })?;
    let status = user_settings_status(&operation_id).map_err(user_operation_error)?;
    Ok(Json(json!({ "operation": status })))
}

pub(crate) async fn rollback_user_settings_route(
    State(state): State<DashboardState>,
    axum::extract::Path(operation_id): axum::extract::Path<String>,
    Json(body): Json<UserSettingsRollbackBodyV1>,
) -> ApiResult {
    let operation_id = ManifestDigest::new(operation_id).map_err(|error| {
        user_operation_error(UserSettingsOperationErrorV1::Unavailable(error.to_string()))
    })?;
    let status = user_settings_status(&operation_id).map_err(user_operation_error)?;
    let receipt = rollback_user_settings(&status.receipt, &body.expected_revision_id)
        .map_err(user_operation_error)?;
    let mut payload = settings_payload(&state).await?;
    payload["restart_recommended"] = json!(receipt.restart_recommended);
    payload["operation"] = json!(receipt);
    Ok(Json(payload))
}

async fn settings_payload(state: &DashboardState) -> std::result::Result<Value, JsonError> {
    let project_configuration = crate::config::cached_runtime_configuration(&state.project_root)
        .map_err(|err| configuration_unavailable(&err))?;
    let legacy_config_path = state.config_path.clone();
    let user = UserConfig::load();
    let user_settings_revision_id = user
        .revision_id()
        .map_err(|err| internal_error(&format!("failed to revise user config: {err}")))?;
    let user_config_path = user_config::config_path()
        .map(|path| path.display().to_string())
        .unwrap_or_default();

    let global_automation = user.automation.clone();
    let automation = automation_settings_payload(
        &global_automation,
        automation_config::load_project_config(&state.dashboard_root)
            .await
            .map_err(|err| err.to_string()),
    );

    Ok(json!({
        "project": {
            "config_path": legacy_config_path.display().to_string(),
            "legacy_config_path": legacy_config_path.display().to_string(),
            "legacy_config_read_only": true,
            "configuration_snapshot_id": project_configuration.snapshot.snapshot_id.as_str(),
            "configuration_revision_id": project_configuration.revision_id.as_str(),
            "config": project_configuration.config,
            "tracedecay_dir_gitignored": crate::config::is_in_gitignore(&state.project_root),
            "pr_autotrack": pr_autotrack_payload(state),
        },
        "user": {
            "config_path": user_config_path,
            "user_settings_revision_id": user_settings_revision_id,
            "upload_enabled": user.upload_enabled,
            "watcher_debounce": user.watcher_debounce,
            "extraction_timeout_secs": user.extraction_timeout_secs,
            "installed_agents": user.installed_agents,
        },
        "automation": automation,
        "environment": environment_payload(),
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
            "version": crate::version::build_version(),
            "channel": if crate::cloud::is_beta() { "beta" } else { "stable" },
            "cached_latest_version": non_empty(&user.cached_latest_version),
        },
    }))
}

fn automation_settings_payload(
    global: &automation_config::AutomationConfig,
    project: Result<Option<automation_config::AutomationConfigPatch>, String>,
) -> Value {
    let (project, project_coverage) = match project {
        Ok(project) => {
            let coverage = if project.is_some() {
                "available"
            } else {
                "absent"
            };
            (project, coverage)
        }
        Err(err) => {
            return json!({
                "config_endpoint": AUTOMATION_CONFIG_ENDPOINT,
                "availability": {
                    "available": false,
                    "reason": format!("project automation configuration could not be read: {err}"),
                    "required_authority": "project automation configuration",
                },
                "source_coverage": {
                    "global": "available",
                    "project": "error",
                    "effective": "unavailable",
                },
            });
        }
    };

    match automation_config::effective_config(global, project.as_ref()) {
        Ok(automation) => json!({
            "config_endpoint": AUTOMATION_CONFIG_ENDPOINT,
            "availability": {
                "available": true,
            },
            "source_coverage": {
                "global": "available",
                "project": project_coverage,
                "effective": "complete",
            },
            "enabled": automation.enabled,
            "backend": automation.backend,
            "host_mode": automation.host_mode,
        }),
        Err(err) => json!({
            "config_endpoint": AUTOMATION_CONFIG_ENDPOINT,
            "availability": {
                "available": false,
                "reason": format!(
                    "effective automation configuration could not be resolved: {err}"
                ),
                "required_authority": "effective automation configuration",
            },
            "source_coverage": {
                "global": "available",
                "project": project_coverage,
                "effective": "error",
            },
        }),
    }
}

/// Lists the PR branches the daemon currently auto-tracks for this project, read
/// from the store's PR-autotrack state sidecar. Empty on non-unix or when the
/// feature has tracked nothing yet.
fn pr_autotrack_payload(state: &DashboardState) -> Value {
    #[cfg(unix)]
    {
        let tracked: Vec<Value> = crate::daemon::pr_autotrack::managed_summary(&state.store_root)
            .into_iter()
            .map(|entry| {
                json!({
                    "branch": entry.branch,
                    "pr": entry.pr,
                    "head_branch": entry.head_branch,
                })
            })
            .collect();
        json!({ "tracked": tracked })
    }
    #[cfg(not(unix))]
    {
        let _ = state;
        json!({ "tracked": [] })
    }
}

fn environment_payload() -> Value {
    let accounting_mode = crate::global_db::global_accounting_mode();
    let pricing_offline =
        std::env::var("TRACEDECAY_OFFLINE").is_ok_and(|v| !v.is_empty() && v != "0");
    json!({
        "global_accounting_mode": accounting_mode.as_str(),
        "global_accounting_enabled": accounting_mode.enabled(),
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

fn surface_mutation(
    mutation: DirectConfigurationMutation,
) -> std::result::Result<ConfigurationDirectMutationSurfaceRequest, JsonError> {
    match mutation {
        DirectConfigurationMutation::Set { layer, key, value } => {
            Ok(ConfigurationDirectMutationSurfaceRequest::Set { layer, key, value })
        }
        DirectConfigurationMutation::Unset { layer, key } => {
            Ok(ConfigurationDirectMutationSurfaceRequest::Unset { layer, key })
        }
        DirectConfigurationMutation::Batch { .. } => Err(configuration_unavailable(
            &"nested settings batch is invalid",
        )),
    }
}

fn project_preview_error(error: ProjectSettingsPreviewErrorV1) -> JsonError {
    match error {
        ProjectSettingsPreviewErrorV1::Validation(issues) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "detail": "settings validation failed",
                "validation_errors": issues,
            })),
        ),
        ProjectSettingsPreviewErrorV1::RevisionConflict { expected, actual } => (
            StatusCode::CONFLICT,
            Json(json!({
                "code": "configuration_revision_conflict",
                "detail": "settings changed after this edit began; refresh and retry",
                "expected_revision_id": expected,
                "actual_revision_id": actual,
            })),
        ),
        ProjectSettingsPreviewErrorV1::InvalidAuthority => {
            configuration_unavailable(&"project settings authority is unavailable")
        }
    }
}

fn application_problem(problem: tracedecay_application::ApplicationProblemEnvelope) -> JsonError {
    let status = match problem.problem.kind {
        ApplicationProblemKind::InvalidRequest => StatusCode::BAD_REQUEST,
        ApplicationProblemKind::NotFoundOrNotAuthorized => StatusCode::NOT_FOUND,
        ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => StatusCode::CONFLICT,
        ApplicationProblemKind::Unsupported => StatusCode::UNPROCESSABLE_ENTITY,
        ApplicationProblemKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ApplicationProblemKind::Saturated => StatusCode::TOO_MANY_REQUESTS,
        ApplicationProblemKind::Cancelled => StatusCode::CONFLICT,
        ApplicationProblemKind::TimedOut => StatusCode::GATEWAY_TIMEOUT,
    };
    let payload = serde_json::to_value(problem)
        .unwrap_or_else(|_| json!({ "detail": "configuration mutation was rejected" }));
    (status, Json(payload))
}

fn user_operation_error(error: UserSettingsOperationErrorV1) -> JsonError {
    match error {
        UserSettingsOperationErrorV1::Validation(issues) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "detail": "settings validation failed",
                "validation_errors": issues,
            })),
        ),
        UserSettingsOperationErrorV1::RevisionConflict { expected, actual } => (
            StatusCode::CONFLICT,
            Json(json!({
                "code": "configuration_revision_conflict",
                "detail": "user settings changed after this edit began; refresh and retry",
                "expected_revision_id": expected,
                "actual_revision_id": actual,
            })),
        ),
        UserSettingsOperationErrorV1::Unavailable(detail) => internal_error(&detail),
    }
}

fn patch_shape_error(scope: &str, err: &serde_json::Error) -> JsonError {
    let message = format!("invalid {scope} patch: {err}");
    let field = serde_error_field(&message).unwrap_or_else(|| "patch".to_string());
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "detail": message,
            "validation_errors": [{ "field": field, "message": message }],
        })),
    )
}

fn serde_error_field(message: &str) -> Option<String> {
    ["unknown field `", "missing field `"]
        .into_iter()
        .find_map(|prefix| {
            let start = message.find(prefix)? + prefix.len();
            let rest = &message[start..];
            let end = rest.find('`')?;
            Some(rest[..end].to_string())
        })
}

fn internal_error(err: &impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&err.to_string())),
    )
}

fn configuration_unavailable(_err: &impl ToString) -> JsonError {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "code": "configuration_authority_unavailable",
            "detail": "configuration authority is unavailable",
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_settings_payload_preserves_project_authority_failure() {
        let payload = automation_settings_payload(
            &automation_config::AutomationConfig::default(),
            Err("project config unreadable".to_owned()),
        );

        assert_eq!(payload["availability"]["available"], false);
        assert_eq!(
            payload["availability"]["required_authority"],
            "project automation configuration"
        );
        assert_eq!(payload["source_coverage"]["global"], "available");
        assert_eq!(payload["source_coverage"]["project"], "error");
        assert_eq!(payload["source_coverage"]["effective"], "unavailable");
        assert!(payload.get("enabled").is_none());
        assert!(payload.get("backend").is_none());
        assert!(payload.get("host_mode").is_none());
    }

    #[test]
    fn automation_settings_payload_preserves_effective_resolution_failure() {
        let mut global = automation_config::AutomationConfig::default();
        global.timeout_secs = 0;
        let payload = automation_settings_payload(&global, Ok(None));

        assert_eq!(payload["availability"]["available"], false);
        assert_eq!(
            payload["availability"]["required_authority"],
            "effective automation configuration"
        );
        assert_eq!(payload["source_coverage"]["global"], "available");
        assert_eq!(payload["source_coverage"]["project"], "absent");
        assert_eq!(payload["source_coverage"]["effective"], "error");
        assert!(payload.get("enabled").is_none());
    }
}
