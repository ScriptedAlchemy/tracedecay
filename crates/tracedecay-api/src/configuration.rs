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

use crate::http::HttpApplicationOperation;

/// Dashboard route for the project-scoped configuration batch write.
pub const PROJECT_SETTINGS_ROUTE_PATH: &str = "/api/settings/project";
/// Dashboard route for the profile-scoped user settings write.
pub const USER_SETTINGS_ROUTE_PATH: &str = "/api/settings/user";
/// Application operation behind the project settings write.
pub const PROJECT_SETTINGS_APPLY_OPERATION: &str =
    HttpApplicationOperation::ConfigurationBatch.as_str();
/// Application operation advertised for the user-profile settings authority.
pub const USER_SETTINGS_APPLY_OPERATION: &str = "user_settings_mutate";
/// Application operation used to refresh configuration state.
pub const SETTINGS_REFRESH_OPERATION: &str = HttpApplicationOperation::ConfigurationList.as_str();

/// One dashboard settings write route. `application_operation` is present only
/// when the route is dispatched through the canonical application surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DashboardConfigurationWriteRouteV1 {
    pub method: &'static str,
    pub path: &'static str,
    pub operation: &'static str,
    pub application_operation: Option<HttpApplicationOperation>,
}

const DASHBOARD_CONFIGURATION_WRITE_ROUTES: [DashboardConfigurationWriteRouteV1; 2] = [
    DashboardConfigurationWriteRouteV1 {
        method: "PATCH",
        path: PROJECT_SETTINGS_ROUTE_PATH,
        operation: PROJECT_SETTINGS_APPLY_OPERATION,
        application_operation: Some(HttpApplicationOperation::ConfigurationBatch),
    },
    DashboardConfigurationWriteRouteV1 {
        method: "PATCH",
        path: USER_SETTINGS_ROUTE_PATH,
        operation: USER_SETTINGS_APPLY_OPERATION,
        application_operation: None,
    },
];

/// Every dashboard settings write route, in mount order.
#[must_use]
pub const fn dashboard_configuration_write_routes() -> &'static [DashboardConfigurationWriteRouteV1]
{
    &DASHBOARD_CONFIGURATION_WRITE_ROUTES
}

/// Resolve an exact dashboard settings write route.
#[must_use]
pub fn dashboard_configuration_write_route(
    method: &str,
    path: &str,
) -> Option<&'static DashboardConfigurationWriteRouteV1> {
    dashboard_configuration_write_routes()
        .iter()
        .find(|route| route.method == method && route.path == path)
}

/// Project-scoped settings patch accepted by `PATCH /api/settings/project`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct ProjectSettingsPatch {
    pub expected_revision_id: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watcher_debounce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_timeout_secs: Option<u64>,
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

/// Render validation failures using the generated dashboard wire shape.
#[must_use]
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
#[must_use]
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
#[must_use]
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
#[must_use]
pub fn configuration_application_problem_error(
    problem: ApplicationProblemEnvelope,
) -> DashboardConfigurationRouteErrorV1 {
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
    use super::{ProjectSettingsPatch, UserSettingsPatch};
    use serde_json::json;

    #[test]
    fn settings_patches_omit_absent_edits_when_serialized() {
        let project = ProjectSettingsPatch {
            expected_revision_id: "project-revision".to_owned(),
            ..ProjectSettingsPatch::default()
        };
        assert_eq!(
            serde_json::to_value(project).expect("serialize project settings patch"),
            json!({ "expected_revision_id": "project-revision" })
        );

        let user = UserSettingsPatch {
            expected_revision_id: "user-revision".to_owned(),
            ..UserSettingsPatch::default()
        };
        assert_eq!(
            serde_json::to_value(user).expect("serialize user settings patch"),
            json!({ "expected_revision_id": "user-revision" })
        );
    }
}
