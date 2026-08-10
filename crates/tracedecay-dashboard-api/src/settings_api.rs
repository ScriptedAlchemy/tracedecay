//! Dashboard endpoints for project and user settings.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use axum::Json;
use axum::extract::State;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use tracedecay_api::configuration::{
    DashboardConfigurationRouteErrorV1, PROJECT_SETTINGS_APPLY_OPERATION,
    SETTINGS_REFRESH_OPERATION, configuration_application_problem_error,
    configuration_authority_unavailable_error, configuration_revision_conflict_error,
    parse_project_settings_patch, parse_user_settings_patch, settings_validation_error,
    validate_user_settings_patch,
};
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblemEnvelope, ApplicationProblemKind,
};

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardEnvelopeV1, DashboardLegalActionKindV1,
    DashboardLegalActionRefV1, scope_from_state,
};
use crate::application::configuration::{
    DirectConfigurationMutation, UserSettingsMutationV1, UserSettingsSnapshotV1,
    parse_duration_millis, plan_user_settings_mutation,
};
use crate::application::settings_control::{
    ProjectSettingsPatchV1, ProjectSettingsPreviewErrorV1, SyncSettingsPatchV1,
    TelemetrySettingsPatchV1, preview_project_settings,
};
use crate::config::TraceDecayConfig;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_agent_hosts::automation::config::from_configuration_snapshot;
use tracedecay_domain::configuration::{ConfigurationIdempotencyKey, ConfigurationRevisionId};

use crate::application_surface::{DashboardConfigurationApplyError, configuration_apply_error};

pub use tracedecay_api::configuration::{ProjectSettingsPatch, UserSettingsPatch};

type ApiResult = std::result::Result<
    Json<DashboardEnvelopeV1<SettingsPayloadV1>>,
    DashboardConfigurationRouteErrorV1,
>;
type ProjectSettingsPatchResult =
    std::result::Result<Json<ProjectSettingsPatchResponseV1>, DashboardConfigurationRouteErrorV1>;

const AUTOMATION_CONFIG_ENDPOINT: &str = "/api/plugins/holographic/curation/config";

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct SettingsPayloadV1 {
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

#[derive(Serialize)]
pub struct ProjectSettingsPatchResponseV1 {
    /// Exact application settlement returned by the configuration binding.
    /// `None` means the submitted patch was already the current state.
    application_outcome: Option<ApplicationOutcome<Value>>,
    current: DashboardEnvelopeV1<SettingsPayloadV1>,
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
    legacy_config_path: String,
    legacy_config_read_only: bool,
    configuration_snapshot_id: String,
    configuration_revision_id: String,
    upload_enabled: bool,
    watcher_debounce: String,
    extraction_timeout_secs: u64,
    installed_agents: Vec<String>,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
struct AutomationSettingsPayloadV1 {
    config_endpoint: String,
    availability: SettingsAvailabilityV1,
    #[serde(skip_serializing_if = "Option::is_none")]
    configuration_revision_id: Option<String>,
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

#[derive(Clone, Debug)]
pub struct DashboardPrAutoTrackEntryV1 {
    pub branch: String,
    pub pr: u64,
    pub head_branch: String,
}

pub trait DashboardPrAutoTrackReadPort: Send + Sync {
    fn managed_summary(&self, store_root: &Path) -> Vec<DashboardPrAutoTrackEntryV1>;
}

static PR_AUTOTRACK_READ_PORT: OnceLock<Arc<dyn DashboardPrAutoTrackReadPort>> = OnceLock::new();

pub fn install_dashboard_pr_autotrack_read_port(
    port: Arc<dyn DashboardPrAutoTrackReadPort>,
) -> Result<(), Arc<dyn DashboardPrAutoTrackReadPort>> {
    PR_AUTOTRACK_READ_PORT.set(port)
}

pub async fn get_settings(State(state): State<DashboardState>) -> ApiResult {
    Ok(Json(settings_envelope(&state, None, None).await?))
}

pub async fn patch_project_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ProjectSettingsPatchResult {
    let patch = parse_project_settings_patch(patch)?;
    let idempotency_key =
        ConfigurationIdempotencyKey::new(patch.idempotency_key.clone()).map_err(|_| {
            settings_validation_error(json!([{
                "field": "idempotency_key",
                "message": "idempotency_key must be one non-empty canonical caller-stable value"
            }]))
        })?;
    let current = crate::config::cached_runtime_configuration(&state.project_root)
        .map_err(|_| configuration_authority_unavailable_error())?;
    let project_id = state
        .project_id
        .as_deref()
        .ok_or_else(configuration_authority_unavailable_error)
        .and_then(|project_id| {
            tracedecay_domain::ProjectId::new(project_id)
                .map_err(|_| configuration_authority_unavailable_error())
        })?;
    let preview = preview_project_settings(
        &project_id,
        &current,
        ProjectSettingsPatchV1 {
            expected_revision_id: patch.expected_revision_id,
            include: patch.include,
            exclude: patch.exclude,
            max_file_size: patch.max_file_size,
            extract_docstrings: patch.extract_docstrings,
            track_call_sites: patch.track_call_sites,
            git_ignore: patch.git_ignore,
            telemetry: patch.telemetry.map(|telemetry| TelemetrySettingsPatchV1 {
                timings: telemetry.timings,
            }),
            sync: patch.sync.map(|sync| SyncSettingsPatchV1 {
                auto_track_pr_branches: sync.auto_track_pr_branches,
                auto_track_pr_poll_secs: sync.auto_track_pr_poll_secs,
            }),
        },
    )
    .map_err(project_preview_error)?;
    let application_outcome = if preview.changed {
        let runtime = state
            .application_invocation_executor
            .as_deref()
            .ok_or_else(configuration_authority_unavailable_error)?;
        let DirectConfigurationMutation::Batch { mutations } = preview.mutation else {
            return Err(configuration_authority_unavailable_error());
        };
        let request_id = mint_global_request_id(GlobalRequestSurface::DashboardSettings)
            .map_err(|_| configuration_authority_unavailable_error())?;
        let expected_revision = preview.expected_revision.clone();
        match runtime
            .apply_configuration_batch(
                request_id,
                mutations,
                preview.expected_revision,
                idempotency_key,
            )
            .await
        {
            Ok(outcome) => Some(outcome),
            Err(DashboardConfigurationApplyError::ApplicationProblem(problem)) => {
                return Err(project_apply_error(
                    &state.project_root,
                    &expected_revision,
                    problem,
                ));
            }
            Err(error) => return Err(configuration_apply_error(error)),
        }
    } else {
        None
    };

    Ok(Json(ProjectSettingsPatchResponseV1 {
        application_outcome,
        current: settings_envelope(&state, Some(preview.resync_recommended), None).await?,
    }))
}

pub async fn patch_user_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = parse_user_settings_patch(patch)?;
    validate_user_settings_patch(&patch, |value| parse_duration_millis(value).is_some())?;
    let idempotency_key =
        ConfigurationIdempotencyKey::new(patch.idempotency_key.clone()).map_err(|_| {
            settings_validation_error(json!([{
                "field": "idempotency_key",
                "message": "idempotency_key must be one non-empty canonical caller-stable value"
            }]))
        })?;
    let expected_revision =
        ConfigurationRevisionId::new(patch.expected_revision_id).map_err(|_| {
            settings_validation_error(json!([{
                "field": "expected_revision_id",
                "message": "expected_revision_id must name one canonical configuration revision"
            }]))
        })?;
    let runtime = state
        .application_invocation_executor
        .as_deref()
        .ok_or_else(configuration_authority_unavailable_error)?;
    let profile_id = runtime
        .user_profile_id()
        .cloned()
        .ok_or_else(configuration_authority_unavailable_error)?;
    let current = state
        .user_settings
        .read()
        .await
        .map_err(|_| configuration_authority_unavailable_error())?;
    let plan = plan_user_settings_mutation(
        &current,
        profile_id,
        UserSettingsMutationV1 {
            upload_enabled: patch.upload_enabled,
            watcher_debounce: patch.watcher_debounce,
            extraction_timeout_secs: patch.extraction_timeout_secs,
        },
    )
    .map_err(|_| configuration_authority_unavailable_error())?;
    if !plan.mutations.is_empty() {
        let request_id = mint_global_request_id(GlobalRequestSurface::DashboardSettings)
            .map_err(|_| configuration_authority_unavailable_error())?;
        if let Err(error) = runtime
            .apply_configuration_batch(
                request_id,
                plan.mutations,
                expected_revision,
                idempotency_key,
            )
            .await
        {
            return Err(configuration_apply_error(error));
        }
    }

    Ok(Json(
        settings_envelope(&state, None, Some(plan.restart_recommended)).await?,
    ))
}

async fn settings_envelope(
    state: &DashboardState,
    resync_recommended: Option<bool>,
    restart_recommended: Option<bool>,
) -> std::result::Result<DashboardEnvelopeV1<SettingsPayloadV1>, DashboardConfigurationRouteErrorV1>
{
    let project_configuration = crate::config::cached_runtime_configuration(&state.project_root)
        .map_err(|_| configuration_authority_unavailable_error())?;
    let legacy_config_path = state.config_path.clone();
    let user = state
        .user_settings
        .read()
        .await
        .map_err(|_| configuration_authority_unavailable_error())?;
    let automation = automation_settings_payload(&project_configuration);
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
            version: crate::version::build_version().to_owned(),
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
    // Both editable scopes settle through the one cataloged daemon
    // configuration effect. A profile write additionally requires the exact
    // profile identity bound by the daemon handshake.
    let mut legal_actions = Vec::new();
    if state.application_invocation_executor.is_some() {
        legal_actions.push(DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::RequestApply,
            PROJECT_SETTINGS_APPLY_OPERATION,
        ));
    }
    legal_actions.push(DashboardLegalActionRefV1::new(
        DashboardLegalActionKindV1::Refresh,
        SETTINGS_REFRESH_OPERATION,
    ));

    Ok(DashboardEnvelopeV1::ready(
        scope_from_state(state),
        DashboardCoverageV1::complete(1, "configuration_control_plane"),
        payload,
    )
    .with_legal_actions(legal_actions))
}

fn user_settings_payload(user: &UserSettingsSnapshotV1) -> UserSettingsPayloadV1 {
    UserSettingsPayloadV1 {
        legacy_config_path: user.legacy_config_path.clone(),
        legacy_config_read_only: true,
        configuration_snapshot_id: user.configuration_snapshot_id.clone(),
        configuration_revision_id: user.configuration_revision_id.clone(),
        upload_enabled: user.upload_enabled,
        watcher_debounce: user.watcher_debounce.clone(),
        extraction_timeout_secs: user.extraction_timeout_secs,
        installed_agents: user.installed_agents.clone(),
    }
}

fn automation_settings_payload(
    project_configuration: &crate::config::PinnedRuntimeConfiguration,
) -> AutomationSettingsPayloadV1 {
    match from_configuration_snapshot(&project_configuration.snapshot) {
        Ok(automation) => AutomationSettingsPayloadV1 {
            config_endpoint: AUTOMATION_CONFIG_ENDPOINT.to_owned(),
            availability: SettingsAvailabilityV1 {
                available: true,
                reason: None,
                required_authority: None,
            },
            configuration_revision_id: Some(project_configuration.revision_id.as_str().to_owned()),
            enabled: Some(automation.enabled),
            backend: Some(automation.backend.as_str().to_owned()),
            host_mode: Some(automation.host_mode.as_str().to_owned()),
        },
        Err(err) => AutomationSettingsPayloadV1 {
            config_endpoint: AUTOMATION_CONFIG_ENDPOINT.to_owned(),
            availability: SettingsAvailabilityV1 {
                available: false,
                reason: Some(format!(
                    "pinned automation configuration could not be resolved: {err}"
                )),
                required_authority: Some("pinned automation configuration".to_owned()),
            },
            configuration_revision_id: Some(project_configuration.revision_id.as_str().to_owned()),
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
    let tracked = PR_AUTOTRACK_READ_PORT
        .get()
        .map(|port| {
            port.managed_summary(&state.store_root)
                .into_iter()
                .map(|entry| PrAutoTrackEntryV1 {
                    branch: entry.branch,
                    pr: entry.pr,
                    head_branch: entry.head_branch,
                })
                .collect()
        })
        .unwrap_or_default();
    PrAutoTrackPayloadV1 { tracked }
}

fn environment_payload() -> EnvironmentSettingsPayloadV1 {
    let accounting_mode = tracedecay_global_db::global_accounting_mode();
    EnvironmentSettingsPayloadV1 {
        global_accounting_mode: accounting_mode.as_str().to_owned(),
        global_accounting_enabled: accounting_mode.enabled(),
        pricing_offline: true,
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

fn project_preview_error(
    error: ProjectSettingsPreviewErrorV1,
) -> DashboardConfigurationRouteErrorV1 {
    match error {
        ProjectSettingsPreviewErrorV1::Validation(issues) => settings_validation_error(issues),
        ProjectSettingsPreviewErrorV1::RevisionConflict { expected, actual } => {
            configuration_revision_conflict_error(
                "settings changed after this edit began; refresh and retry",
                &expected,
                &actual,
            )
        }
        ProjectSettingsPreviewErrorV1::InvalidAuthority => {
            configuration_authority_unavailable_error()
        }
    }
}

/// Renders a rejected project apply as the refusal a settings client can act
/// on.
///
/// The local preview only refuses a stale revision when the patch carries no
/// mutation at all: a stale patch that does carry one may still be an exact
/// idempotent replay, and only the daemon owns that replay authority. So the
/// authority is the one that rejects a genuinely superseded edit — and it
/// collapses revision and idempotency conflicts into a single opaque
/// `configuration.conflict`, which names neither the CAS precondition nor the
/// revision that now holds.
///
/// This re-reads the pinned runtime configuration the same route already
/// serves as the revision authority. When the revision that now holds is no
/// longer the one this edit expected, the CAS precondition provably failed and
/// the typed conflict carries both revisions. Every other rejection — an
/// idempotency conflict against the current revision included — keeps the
/// daemon's own problem envelope rather than being relabeled by a guess.
fn project_apply_error(
    project_root: &Path,
    expected_revision: &ConfigurationRevisionId,
    problem: ApplicationProblemEnvelope,
) -> DashboardConfigurationRouteErrorV1 {
    if problem.problem.kind() == ApplicationProblemKind::Conflict
        && let Ok(current) = crate::config::cached_runtime_configuration(project_root)
        && current.revision_id != *expected_revision
    {
        return configuration_revision_conflict_error(
            "settings changed after this edit began; refresh and retry",
            expected_revision.as_str(),
            current.revision_id.as_str(),
        );
    }
    configuration_application_problem_error(problem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn serialization_schema<T: JsonSchema>() -> Value {
        let generator = schemars::generate::SchemaSettings::default()
            .for_serialize()
            .into_generator();
        serde_json::to_value(generator.into_root_schema_for::<T>())
            .expect("serialize settings patch schema")
    }

    #[test]
    fn route_settings_patch_schemas_are_canonical() {
        let project_route = serialization_schema::<ProjectSettingsPatch>();
        let project_canonical =
            serialization_schema::<tracedecay_api::configuration::ProjectSettingsPatch>();
        assert_eq!(project_route, project_canonical);
        assert_eq!(
            project_route["required"],
            json!(["expected_revision_id", "idempotency_key"])
        );

        let user_route = serialization_schema::<UserSettingsPatch>();
        let user_canonical =
            serialization_schema::<tracedecay_api::configuration::UserSettingsPatch>();
        assert_eq!(user_route, user_canonical);
        assert_eq!(
            user_route["required"],
            json!(["expected_revision_id", "idempotency_key"])
        );
    }
}
