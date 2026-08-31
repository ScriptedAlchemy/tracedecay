//! Dashboard configuration write descriptors, DTOs, and error mapping.
//!
//! The executable supplies the exact project scope, current configuration, and
//! daemon invocation authority. This adapter owns only the stable dashboard
//! route contract: accepted patch shapes, application-operation references, and
//! typed HTTP error presentation.

use axum::Json;
use axum::http::StatusCode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_application::{ApplicationProblemEnvelope, ApplicationProblemKind};
use tracedecay_domain::configuration::CodeIndexWorkerSelectionV1;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

/// Application operation behind the project settings write.
pub const PROJECT_SETTINGS_APPLY_OPERATION: &str =
    ApplicationSurfaceOperation::ConfigurationBatch.as_str();
/// Application operation used to refresh configuration state.
pub const SETTINGS_REFRESH_OPERATION: &str =
    ApplicationSurfaceOperation::ConfigurationList.as_str();

/// Project-scoped settings patch accepted by `PATCH /api/settings/project`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct ProjectSettingsPatch {
    pub expected_revision_id: String,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract_docstrings: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_call_sites: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ignore: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetrySettingsPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncSettingsPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_scout: Option<bool>,
}

/// Nested synchronization settings patch.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct SyncSettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_track_pr_branches: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_track_pr_poll_secs: Option<u64>,
}

/// Nested telemetry settings patch.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<bool>,
}

/// Profile-scoped settings patch accepted by `PATCH /api/settings/user`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct UserSettingsPatch {
    pub expected_revision_id: String,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watcher_debounce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_timeout_secs: Option<u64>,
}

/// Dedicated profile-session worker patch. Its CAS token is never a project
/// configuration revision, so it cannot be mixed with [`UserSettingsPatch`].
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexWorkerSettingsPatch {
    pub expected_revision_id: String,
    pub idempotency_key: String,
    pub code_index_workers: CodeIndexWorkerSelectionV1,
}

/// Axum-compatible typed error used by dashboard configuration handlers.
pub type DashboardConfigurationRouteErrorV1 = (StatusCode, Json<Value>);

/// Parse a project settings patch while preserving the dashboard's established
/// malformed-payload response shape.
pub fn parse_project_settings_patch(
    patch: Value,
) -> Result<ProjectSettingsPatch, DashboardConfigurationRouteErrorV1> {
    serde_json::from_value(patch).map_err(|error| patch_shape_error("project settings", &error))
}

/// Parse a user settings patch while preserving the dashboard's established
/// malformed-payload response shape.
pub fn parse_user_settings_patch(
    patch: Value,
) -> Result<UserSettingsPatch, DashboardConfigurationRouteErrorV1> {
    serde_json::from_value(patch).map_err(|error| patch_shape_error("user settings", &error))
}

pub fn parse_code_index_worker_settings_patch(
    patch: Value,
) -> Result<CodeIndexWorkerSettingsPatch, DashboardConfigurationRouteErrorV1> {
    serde_json::from_value(patch)
        .map_err(|error| patch_shape_error("code index worker settings", &error))
}

/// Validate the transport-owned user patch invariants. The executable supplies
/// the duration parser because profile configuration remains its authority.
pub fn validate_user_settings_patch(
    patch: &UserSettingsPatch,
    duration_is_valid: impl Fn(&str) -> bool,
) -> Result<(), DashboardConfigurationRouteErrorV1> {
    let mut errors = Vec::new();
    if let Some(debounce) = &patch.watcher_debounce
        && !duration_is_valid(debounce)
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
    if errors.is_empty() {
        Ok(())
    } else {
        Err(settings_validation_error(errors))
    }
}

pub fn validate_code_index_worker_settings_patch(
    patch: &CodeIndexWorkerSettingsPatch,
) -> Result<(), DashboardConfigurationRouteErrorV1> {
    patch.code_index_workers.validate().map_err(|_| {
        settings_validation_error([validation_error(
            "code_index_workers",
            "code_index_workers exact mode must request at least 1 worker",
        )])
    })
}

/// Render validation failures using the generated dashboard wire shape.
pub fn settings_validation_error(errors: impl Serialize) -> DashboardConfigurationRouteErrorV1 {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "detail": "settings validation failed",
            "validation_errors": errors,
        })),
    )
}

/// Render a revision mismatch without losing either CAS revision.
pub fn configuration_revision_conflict_error(
    detail: &str,
    expected: &str,
    actual: &str,
) -> DashboardConfigurationRouteErrorV1 {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "code": "configuration_revision_conflict",
            "detail": detail,
            "expected_revision_id": expected,
            "actual_revision_id": actual,
        })),
    )
}

/// Render the fail-closed missing-authority response.
pub fn configuration_authority_unavailable_error() -> DashboardConfigurationRouteErrorV1 {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "code": "configuration_authority_unavailable",
            "detail": "configuration authority is unavailable",
        })),
    )
}

/// Preserve canonical application problem bodies for project configuration
/// writes while mapping their problem kind to the dashboard's historic status.
pub fn configuration_application_problem_error(
    problem: ApplicationProblemEnvelope,
) -> DashboardConfigurationRouteErrorV1 {
    let status = match problem.problem.kind {
        ApplicationProblemKind::InvalidRequest => StatusCode::BAD_REQUEST,
        ApplicationProblemKind::NotFoundOrNotAuthorized => StatusCode::NOT_FOUND,
        ApplicationProblemKind::Conflict
        | ApplicationProblemKind::PartialEffect
        | ApplicationProblemKind::Stale => StatusCode::CONFLICT,
        ApplicationProblemKind::Unsupported => StatusCode::UNPROCESSABLE_ENTITY,
        ApplicationProblemKind::ResetRequired | ApplicationProblemKind::Unavailable => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        ApplicationProblemKind::ExecutionFailed => StatusCode::INTERNAL_SERVER_ERROR,
        ApplicationProblemKind::Saturated => StatusCode::TOO_MANY_REQUESTS,
        ApplicationProblemKind::Cancelled => StatusCode::CONFLICT,
        ApplicationProblemKind::TimedOut => StatusCode::GATEWAY_TIMEOUT,
    };
    let payload = serde_json::to_value(problem)
        .unwrap_or_else(|_| json!({ "detail": "configuration mutation was rejected" }));
    (status, Json(payload))
}

fn validation_error(field: &str, message: &str) -> Value {
    json!({ "field": field, "message": message })
}

fn patch_shape_error(scope: &str, error: &serde_json::Error) -> DashboardConfigurationRouteErrorV1 {
    let message = format!("invalid {scope} patch: {error}");
    let field = serde_error_field(&message).unwrap_or_else(|| "patch".to_owned());
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
            Some(rest[..end].to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_worker_patch_round_trips_the_tagged_contract() {
        let automatic = parse_code_index_worker_settings_patch(json!({
            "expected_revision_id": "configuration.revision.fixture",
            "idempotency_key": "configuration.idempotency.fixture",
            "code_index_workers": { "mode": "automatic" }
        }))
        .unwrap();
        assert_eq!(
            automatic.code_index_workers,
            CodeIndexWorkerSelectionV1::Automatic {}
        );

        let exact = parse_code_index_worker_settings_patch(json!({
            "expected_revision_id": "configuration.revision.fixture",
            "idempotency_key": "configuration.idempotency.fixture",
            "code_index_workers": { "mode": "exact", "workers": 64 }
        }))
        .unwrap();
        assert_eq!(
            exact.code_index_workers,
            CodeIndexWorkerSelectionV1::Exact { workers: 64 }
        );
    }

    #[test]
    fn user_worker_patch_denies_zero_exact_workers() {
        let patch = parse_code_index_worker_settings_patch(json!({
            "expected_revision_id": "configuration.revision.fixture",
            "idempotency_key": "configuration.idempotency.fixture",
            "code_index_workers": { "mode": "exact", "workers": 0 }
        }))
        .unwrap();

        let (status, Json(body)) =
            validate_code_index_worker_settings_patch(&patch).expect_err("zero must be denied");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["validation_errors"][0]["field"], "code_index_workers");
    }

    #[test]
    fn project_backed_user_patch_rejects_mixed_profile_worker_mutation() {
        let result = parse_user_settings_patch(json!({
            "expected_revision_id": "configuration.revision.project",
            "idempotency_key": "configuration.idempotency.fixture",
            "upload_enabled": true,
            "code_index_workers": { "mode": "automatic" }
        }));

        let (status, Json(body)) = result.expect_err("mixed authority must be rejected");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["validation_errors"][0]["field"], "code_index_workers");
    }
}
