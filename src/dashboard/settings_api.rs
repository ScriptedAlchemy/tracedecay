//! Dashboard endpoints for project and user settings.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardEnvelopeV1, DashboardLegalActionKindV1,
    DashboardLegalActionRefV1, scope_from_state,
};
use super::util::{JsonError, http_detail};
use crate::application::configuration::{
    UserSettingsAuthorityError, UserSettingsMutationV1, UserSettingsSnapshotV1,
};
use crate::automation::config as automation_config;
use crate::config::{TelemetryConfig, TraceDecayConfig};
use crate::user_config;

type ApiResult = std::result::Result<Json<DashboardEnvelopeV1<SettingsPayloadV1>>, JsonError>;

const AUTOMATION_CONFIG_ENDPOINT: &str = "/api/plugins/holographic/curation/config";

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectSettingsPatch {
    expected_revision_id: String,
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

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct SyncSettingsPatch {
    #[serde(default)]
    auto_track_pr_branches: Option<bool>,
    #[serde(default)]
    auto_track_pr_poll_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct TelemetrySettingsPatch {
    #[serde(default)]
    timings: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct UserSettingsPatch {
    expected_revision_id: String,
    #[serde(default)]
    upload_enabled: Option<bool>,
    #[serde(default)]
    watcher_debounce: Option<String>,
    #[serde(default)]
    extraction_timeout_secs: Option<u64>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub(crate) struct SettingsPayloadV1 {
    project: ProjectSettingsPayloadV1,
    user: UserSettingsPayloadV1,
    automation: AutomationSettingsPayloadV1,
    environment: EnvironmentSettingsPayloadV1,
    storage: StorageSettingsPayloadV1,
    version: VersionSettingsPayloadV1,
    #[serde(skip_serializing_if = "Option::is_none")]
    resync_recommended: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    restart_recommended: Option<bool>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct ProjectSettingsPayloadV1 {
    config_path: String,
    legacy_config_path: String,
    legacy_config_read_only: bool,
    configuration_snapshot_id: String,
    configuration_revision_id: String,
    config: ProjectEditableSettingsV1,
    tracedecay_dir_gitignored: bool,
    pr_autotrack: PrAutoTrackPayloadV1,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct ProjectEditableSettingsV1 {
    include: Vec<String>,
    exclude: Vec<String>,
    max_file_size: u64,
    extract_docstrings: bool,
    track_call_sites: bool,
    git_ignore: bool,
    telemetry: TelemetrySettingsV1,
    sync: SyncSettingsV1,
}

impl From<&TraceDecayConfig> for ProjectEditableSettingsV1 {
    fn from(config: &TraceDecayConfig) -> Self {
        Self {
            include: config.include.clone(),
            exclude: config.exclude.clone(),
            max_file_size: config.max_file_size,
            extract_docstrings: config.extract_docstrings,
            track_call_sites: config.track_call_sites,
            git_ignore: config.git_ignore,
            telemetry: TelemetrySettingsV1 {
                timings: config.telemetry.timings,
            },
            sync: SyncSettingsV1 {
                auto_track_pr_branches: config.sync.auto_track_pr_branches,
                auto_track_pr_poll_secs: config.sync.auto_track_pr_poll_secs,
            },
        }
    }
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct TelemetrySettingsV1 {
    timings: bool,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct SyncSettingsV1 {
    auto_track_pr_branches: bool,
    auto_track_pr_poll_secs: u64,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct UserSettingsPayloadV1 {
    config_path: String,
    user_settings_revision_id: String,
    upload_enabled: bool,
    watcher_debounce: String,
    extraction_timeout_secs: u64,
    installed_agents: Vec<String>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct AutomationSettingsPayloadV1 {
    config_endpoint: String,
    availability: SettingsAvailabilityV1,
    source_coverage: AutomationSourceCoverageV1,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_mode: Option<String>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct SettingsAvailabilityV1 {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_authority: Option<String>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct AutomationSourceCoverageV1 {
    global: String,
    project: String,
    effective: String,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct EnvironmentSettingsPayloadV1 {
    global_accounting_mode: String,
    global_accounting_enabled: bool,
    pricing_offline: bool,
    variables: Vec<EnvironmentVariableV1>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct EnvironmentVariableV1 {
    name: String,
    active: bool,
    value: Option<String>,
    description: String,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct StorageSettingsPayloadV1 {
    project_id: Option<String>,
    project_root: String,
    storage_mode: String,
    store_root: String,
    dashboard_root: String,
    graph_db: String,
    memory_db: String,
    lcm_db: String,
    lcm_scope: String,
    savings_db: String,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct VersionSettingsPayloadV1 {
    version: String,
    channel: String,
    cached_latest_version: Option<String>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct PrAutoTrackPayloadV1 {
    tracked: Vec<PrAutoTrackEntryV1>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct PrAutoTrackEntryV1 {
    branch: String,
    pr: u64,
    head_branch: String,
}

pub(crate) async fn get_settings(State(state): State<DashboardState>) -> ApiResult {
    Ok(Json(settings_envelope(&state, None, None).await?))
}

pub(crate) async fn patch_project_settings(
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
    if let Some(sync) = &patch.sync
        && let Some(secs) = sync.auto_track_pr_poll_secs
        && secs < crate::config::MIN_AUTO_TRACK_PR_POLL_SECS
    {
        errors.push(validation_error(
            "auto_track_pr_poll_secs",
            &format!(
                "auto_track_pr_poll_secs must be at least {} seconds",
                crate::config::MIN_AUTO_TRACK_PR_POLL_SECS
            ),
        ));
    }
    if !errors.is_empty() {
        return Err(validation_failed(&errors));
    }

    let current = crate::config::cached_runtime_configuration(&state.project_root)
        .map_err(|err| configuration_unavailable(&err))?;
    ensure_expected_revision(&patch.expected_revision_id, current.revision_id.as_str())?;
    let current_config = &current.config;
    let sync = patch.sync.as_ref().map_or_else(
        || current_config.sync.clone(),
        |sync| crate::config::SyncConfig {
            auto_track_pr_branches: sync
                .auto_track_pr_branches
                .unwrap_or(current_config.sync.auto_track_pr_branches),
            auto_track_pr_poll_secs: sync
                .auto_track_pr_poll_secs
                .unwrap_or(current_config.sync.auto_track_pr_poll_secs),
            ..current_config.sync.clone()
        },
    );
    let telemetry = patch.telemetry.map_or_else(
        || current_config.telemetry.clone(),
        |telemetry| TelemetryConfig {
            timings: telemetry
                .timings
                .unwrap_or(current_config.telemetry.timings),
        },
    );
    let updated = TraceDecayConfig {
        include: patch
            .include
            .unwrap_or_else(|| current_config.include.clone()),
        exclude: patch
            .exclude
            .unwrap_or_else(|| current_config.exclude.clone()),
        max_file_size: patch.max_file_size.unwrap_or(current_config.max_file_size),
        extract_docstrings: patch
            .extract_docstrings
            .unwrap_or(current_config.extract_docstrings),
        track_call_sites: patch
            .track_call_sites
            .unwrap_or(current_config.track_call_sites),
        git_ignore: patch.git_ignore.unwrap_or(current_config.git_ignore),
        telemetry,
        sync,
        ..current_config.clone()
    };
    let resync_recommended = updated != *current_config;
    if resync_recommended {
        crate::config::mutate_pinned_runtime_configuration(&current, updated)
            .await
            .map_err(|err| configuration_unavailable(&err))?;
    }

    Ok(Json(
        settings_envelope(&state, Some(resync_recommended), None).await?,
    ))
}

pub(crate) async fn patch_user_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<UserSettingsPatch>(patch)
        .map_err(|err| patch_shape_error("user settings", &err))?;

    let mut errors = Vec::new();
    if let Some(debounce) = &patch.watcher_debounce
        && user_config::parse_duration(debounce).is_none()
    {
        errors.push(validation_error(
            "watcher_debounce",
            "watcher_debounce must be a duration like \"2s\", \"15s\", or \"1m\"",
        ));
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

    let mutation = match state
        .user_settings
        .mutate(
            patch.expected_revision_id,
            UserSettingsMutationV1 {
                upload_enabled: patch.upload_enabled,
                watcher_debounce: patch.watcher_debounce,
                extraction_timeout_secs: patch.extraction_timeout_secs,
            },
        )
        .await
    {
        Ok(mutation) => mutation,
        Err(UserSettingsAuthorityError::RevisionConflict { expected, actual }) => {
            return Err(user_revision_conflict(&expected, &actual));
        }
        Err(error) => return Err(configuration_unavailable(&error)),
    };
    if let Some(backup) = mutation.recovered_backup_path {
        tracing::warn!(backup, "corrupt user config backed up before regeneration");
    }

    Ok(Json(
        settings_envelope(&state, None, Some(mutation.restart_recommended)).await?,
    ))
}

async fn settings_envelope(
    state: &DashboardState,
    resync_recommended: Option<bool>,
    restart_recommended: Option<bool>,
) -> std::result::Result<DashboardEnvelopeV1<SettingsPayloadV1>, JsonError> {
    let project_configuration = crate::config::cached_runtime_configuration(&state.project_root)
        .map_err(|err| configuration_unavailable(&err))?;
    let legacy_config_path = state.config_path.clone();
    let user = state
        .user_settings
        .read()
        .await
        .map_err(|error| configuration_unavailable(&error))?;
    let automation = automation_settings_payload(
        &user.automation,
        automation_config::load_project_config(&state.dashboard_root)
            .await
            .map_err(|err| err.to_string()),
    );
    let payload = SettingsPayloadV1 {
        project: ProjectSettingsPayloadV1 {
            config_path: legacy_config_path.display().to_string(),
            legacy_config_path: legacy_config_path.display().to_string(),
            legacy_config_read_only: true,
            configuration_snapshot_id: project_configuration
                .snapshot
                .snapshot_id
                .as_str()
                .to_owned(),
            configuration_revision_id: project_configuration.revision_id.as_str().to_owned(),
            config: ProjectEditableSettingsV1::from(&project_configuration.config),
            tracedecay_dir_gitignored: crate::config::is_in_gitignore(&state.project_root),
            pr_autotrack: pr_autotrack_payload(state),
        },
        user: user_settings_payload(&user),
        automation,
        environment: environment_payload(),
        storage: StorageSettingsPayloadV1 {
            project_id: state.project_id.clone(),
            project_root: state.project_root.display().to_string(),
            storage_mode: state.storage_mode.clone(),
            store_root: state.store_root.display().to_string(),
            dashboard_root: state.dashboard_root.display().to_string(),
            graph_db: state.graph_db_path.clone(),
            memory_db: state.mem_db_path.clone(),
            lcm_db: state.lcm_db_path.clone(),
            lcm_scope: state.lcm_scope.clone(),
            savings_db: state.savings_db_path.clone(),
        },
        version: VersionSettingsPayloadV1 {
            version: crate::version::build_version(),
            channel: if crate::cloud::is_beta() {
                "beta".to_owned()
            } else {
                "stable".to_owned()
            },
            cached_latest_version: non_empty(&user.cached_latest_version).map(str::to_owned),
        },
        resync_recommended,
        restart_recommended,
    };
    Ok(DashboardEnvelopeV1::ready(
        scope_from_state(state),
        DashboardCoverageV1::complete(2, "configuration_authorities"),
        payload,
    )
    .with_legal_actions(vec![
        DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::RequestApply,
            "configuration_batch",
        ),
        DashboardLegalActionRefV1::new(DashboardLegalActionKindV1::Refresh, "configuration_list"),
    ]))
}

fn user_settings_payload(user: &UserSettingsSnapshotV1) -> UserSettingsPayloadV1 {
    UserSettingsPayloadV1 {
        config_path: user.config_path.clone(),
        user_settings_revision_id: user.revision_id.clone(),
        upload_enabled: user.upload_enabled,
        watcher_debounce: user.watcher_debounce.clone(),
        extraction_timeout_secs: user.extraction_timeout_secs,
        installed_agents: user.installed_agents.clone(),
    }
}

fn automation_settings_payload(
    global: &automation_config::AutomationConfig,
    project: Result<Option<automation_config::AutomationConfigPatch>, String>,
) -> AutomationSettingsPayloadV1 {
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
            return AutomationSettingsPayloadV1 {
                config_endpoint: AUTOMATION_CONFIG_ENDPOINT.to_owned(),
                availability: SettingsAvailabilityV1 {
                    available: false,
                    reason: Some(format!(
                        "project automation configuration could not be read: {err}"
                    )),
                    required_authority: Some("project automation configuration".to_owned()),
                },
                source_coverage: AutomationSourceCoverageV1 {
                    global: "available".to_owned(),
                    project: "error".to_owned(),
                    effective: "unavailable".to_owned(),
                },
                enabled: None,
                backend: None,
                host_mode: None,
            };
        }
    };

    match automation_config::effective_config(global, project.as_ref()) {
        Ok(automation) => AutomationSettingsPayloadV1 {
            config_endpoint: AUTOMATION_CONFIG_ENDPOINT.to_owned(),
            availability: SettingsAvailabilityV1 {
                available: true,
                reason: None,
                required_authority: None,
            },
            source_coverage: AutomationSourceCoverageV1 {
                global: "available".to_owned(),
                project: project_coverage.to_owned(),
                effective: "complete".to_owned(),
            },
            enabled: Some(automation.enabled),
            backend: Some(automation.backend.as_str().to_owned()),
            host_mode: Some(automation.host_mode.as_str().to_owned()),
        },
        Err(err) => AutomationSettingsPayloadV1 {
            config_endpoint: AUTOMATION_CONFIG_ENDPOINT.to_owned(),
            availability: SettingsAvailabilityV1 {
                available: false,
                reason: Some(format!(
                    "effective automation configuration could not be resolved: {err}"
                )),
                required_authority: Some("effective automation configuration".to_owned()),
            },
            source_coverage: AutomationSourceCoverageV1 {
                global: "available".to_owned(),
                project: project_coverage.to_owned(),
                effective: "error".to_owned(),
            },
            enabled: None,
            backend: None,
            host_mode: None,
        },
    }
}

/// Lists the PR branches the daemon currently auto-tracks for this project, read
/// from the store's PR-autotrack state sidecar. Empty on non-unix or when the
/// feature has tracked nothing yet.
fn pr_autotrack_payload(state: &DashboardState) -> PrAutoTrackPayloadV1 {
    #[cfg(unix)]
    {
        let tracked = crate::daemon::pr_autotrack::managed_summary(&state.store_root)
            .into_iter()
            .map(|entry| PrAutoTrackEntryV1 {
                branch: entry.branch,
                pr: entry.pr,
                head_branch: entry.head_branch,
            })
            .collect();
        PrAutoTrackPayloadV1 { tracked }
    }
    #[cfg(not(unix))]
    {
        let _ = state;
        PrAutoTrackPayloadV1 {
            tracked: Vec::new(),
        }
    }
}

fn environment_payload() -> EnvironmentSettingsPayloadV1 {
    let accounting_mode = crate::global_db::global_accounting_mode();
    let pricing_offline =
        std::env::var("TRACEDECAY_OFFLINE").is_ok_and(|v| !v.is_empty() && v != "0");
    EnvironmentSettingsPayloadV1 {
        global_accounting_mode: accounting_mode.as_str().to_owned(),
        global_accounting_enabled: accounting_mode.enabled(),
        pricing_offline,
        variables: vec![
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
    }
}

fn env_variable(name: &str, description: &str) -> EnvironmentVariableV1 {
    let value = std::env::var(name).ok().filter(|value| !value.is_empty());
    EnvironmentVariableV1 {
        name: name.to_owned(),
        active: value.is_some(),
        value,
        description: description.to_owned(),
    }
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

fn ensure_expected_revision(expected: &str, actual: &str) -> std::result::Result<(), JsonError> {
    if expected == actual {
        return Ok(());
    }
    Err((
        StatusCode::CONFLICT,
        Json(json!({
            "code": "configuration_revision_conflict",
            "detail": "settings changed after this edit began; refresh and retry",
            "expected_revision_id": expected,
            "actual_revision_id": actual,
        })),
    ))
}

fn user_revision_conflict(expected: &str, actual: &str) -> JsonError {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "code": "configuration_revision_conflict",
            "detail": "user settings changed after this edit began; refresh and retry",
            "expected_revision_id": expected,
            "actual_revision_id": actual,
        })),
    )
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
        let payload = serde_json::to_value(payload).expect("serialize automation settings");

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
        let payload = serde_json::to_value(payload).expect("serialize automation settings");

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
