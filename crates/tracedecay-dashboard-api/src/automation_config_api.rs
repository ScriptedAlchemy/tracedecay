//! Dashboard adapter for the daemon-owned automation configuration setting.
//!
//! Automation is one project setting in the configuration control plane. This
//! adapter never creates a profile/dashboard sidecar: it reads the pinned
//! snapshot and submits a revision-fenced project mutation through the same
//! application runtime as every other configuration write.

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::{Value, json};
use tracedecay_api::configuration::{
    DashboardConfigurationRouteErrorV1, configuration_application_problem_error,
    configuration_authority_unavailable_error, configuration_revision_conflict_error,
    settings_validation_error,
};
use tracedecay_application::ApplicationOutcome;
use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::{
    AUTOMATION_SETTINGS_SETTING_KEY, ConfigurationIdempotencyKey, ConfigurationLayerIdV1,
    ConfigurationRevisionId, ConfigurationValueV1, SettingKey,
};

use super::DashboardState;
use crate::application::configuration::DirectConfigurationMutation;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_agent_hosts::automation::backend;
use tracedecay_agent_hosts::automation::config::{
    AutomationConfig, AutomationConfigPatch, effective_config, from_configuration_snapshot,
};

type ApiResult = std::result::Result<Json<Value>, DashboardConfigurationRouteErrorV1>;

/// A caller-stable CAS write. `AutomationConfigPatch` is the maintained
/// automation patch vocabulary; it deliberately has no approval-policy fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationConfigMutationRequest {
    pub expected_revision_id: String,
    pub idempotency_key: String,
    #[serde(flatten)]
    pub patch: AutomationConfigPatch,
}

pub async fn get_config(State(state): State<DashboardState>) -> ApiResult {
    let (configuration_revision_id, effective) = effective_automation_config(&state)
        .map_err(|_| configuration_authority_unavailable_error())?;
    Ok(Json(config_payload(
        &configuration_revision_id,
        &effective,
        None,
    )?))
}

pub async fn patch_config(
    State(state): State<DashboardState>,
    Json(request): Json<AutomationConfigMutationRequest>,
) -> ApiResult {
    let expected_revision =
        ConfigurationRevisionId::new(request.expected_revision_id).map_err(|_| {
            settings_validation_error(json!([{
                "field": "expected_revision_id",
                "message": "expected_revision_id must name one canonical configuration revision"
            }]))
        })?;
    let idempotency_key =
        ConfigurationIdempotencyKey::new(request.idempotency_key).map_err(|_| {
            settings_validation_error(json!([{
                "field": "idempotency_key",
                "message": "idempotency_key must be one non-empty canonical caller-stable value"
            }]))
        })?;
    let (current_revision, current) = effective_automation_config(&state)
        .map_err(|_| configuration_authority_unavailable_error())?;
    if expected_revision != current_revision {
        return Err(configuration_revision_conflict_error(
            "automation settings changed after this edit began; refresh and retry",
            expected_revision.as_str(),
            current_revision.as_str(),
        ));
    }
    let candidate = effective_config(&current, Some(&request.patch)).map_err(|error| {
        settings_validation_error(json!([{
            "field": "automation",
            "message": error.to_string(),
        }]))
    })?;

    let application_outcome = if candidate == current {
        None
    } else {
        let project_id = state
            .project_id
            .as_deref()
            .ok_or_else(configuration_authority_unavailable_error)
            .and_then(|project_id| {
                ProjectId::new(project_id).map_err(|_| configuration_authority_unavailable_error())
            })?;
        let key = SettingKey::new(AUTOMATION_SETTINGS_SETTING_KEY)
            .map_err(|_| configuration_authority_unavailable_error())?;
        let runtime = state
            .application_invocation_executor
            .as_deref()
            .ok_or_else(configuration_authority_unavailable_error)?;
        let request_id = mint_global_request_id(GlobalRequestSurface::DashboardSettings)
            .map_err(|_| configuration_authority_unavailable_error())?;
        let outcome = runtime
            .apply_configuration_batch(
                request_id,
                vec![DirectConfigurationMutation::Set {
                    layer: ConfigurationLayerIdV1::Project { project_id },
                    key,
                    value: ConfigurationValueV1::AutomationSettings(candidate),
                }],
                expected_revision,
                idempotency_key,
            )
            .await
            .map_err(configuration_application_problem_error)?;
        state.reconcile_automation_scheduler();
        Some(outcome)
    };

    // The runtime refreshes the pinned snapshot as part of a settled
    // configuration effect. Re-read it instead of projecting the submitted
    // candidate, so a response never claims a setting that failed activation.
    let (configuration_revision_id, effective) = effective_automation_config(&state)
        .map_err(|_| configuration_authority_unavailable_error())?;
    Ok(Json(config_payload(
        &configuration_revision_id,
        &effective,
        application_outcome.as_ref(),
    )?))
}

/// Returns the one admitted runtime configuration for an automation caller.
/// The revision is returned with the value so consumers cannot accidentally
/// pair a status result with an unrelated configuration revision.
pub(crate) fn effective_automation_config(
    state: &DashboardState,
) -> tracedecay_runtime_core::errors::Result<(ConfigurationRevisionId, AutomationConfig)> {
    let pinned = crate::config::cached_runtime_configuration(&state.project_root)?;
    let config = from_configuration_snapshot(&pinned.snapshot)?;
    Ok((pinned.revision_id, config))
}

fn config_payload(
    configuration_revision_id: &ConfigurationRevisionId,
    effective: &AutomationConfig,
    application_outcome: Option<&ApplicationOutcome<Value>>,
) -> std::result::Result<Value, DashboardConfigurationRouteErrorV1> {
    let mut payload = json!({
        "configuration_revision_id": configuration_revision_id.as_str(),
        "source": "daemon_pinned_snapshot",
        "effective": effective,
        "backend_availability": backend::backend_availability(effective),
    });
    if let Some(application_outcome) = application_outcome {
        payload["application_outcome"] = serde_json::to_value(application_outcome)
            .map_err(|_| configuration_authority_unavailable_error())?;
    }
    Ok(payload)
}
