use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::DashboardState;
use super::util::{JsonError, http_detail};
use crate::application::dashboard_diagnostics::{
    DashboardDiagnosticsAuthorityV1, DashboardDiagnosticsErrorV1, settings_revision,
};
use tracedecay_domain::ManifestDigest;
use tracedecay_lsp::analyzer::adapters::LspAdapterDefinition;
use tracedecay_lsp::analyzer::broker::DiagnosticsSnapshot;
use tracedecay_lsp::analyzer::settings::IdleBackfillMode;

type ApiResult = std::result::Result<Json<Value>, JsonError>;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPatch {
    /// The `settings_revision` the editor read. Required: without it the route
    /// cannot tell an edit of the current settings from one that would
    /// overwrite a writer the caller never saw.
    expected_revision: ManifestDigest,
    #[serde(default)]
    idle_backfill: Option<IdleBackfillMode>,
    #[serde(default)]
    languages: BTreeMap<String, LanguageSettingsPatch>,
    #[serde(default)]
    custom_adapters: Option<Vec<LspAdapterDefinition>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct LanguageSettingsPatch {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_command_override_patch")]
    command_override: CommandOverridePatch,
}

#[derive(Debug, Clone, Default)]
enum CommandOverridePatch {
    #[default]
    Missing,
    Null,
    Value(String),
}

pub async fn overview(State(state): State<DashboardState>) -> ApiResult {
    let snapshot = authority(&state)?
        .overview()
        .await
        .map_err(authority_error)?;
    snapshot_response(&snapshot)
}

pub async fn patch_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<SettingsPatch>(patch).map_err(|error| {
        bad_request(&format!("invalid code diagnostics settings patch: {error}"))
    })?;
    let snapshot = authority(&state)?
        .update_settings(&patch.expected_revision, |settings| {
            if let Some(mode) = patch.idle_backfill {
                settings.idle_backfill = mode;
            }
            for (language, language_patch) in patch.languages {
                let language_settings = settings.languages.entry(language).or_default();
                if let Some(enabled) = language_patch.enabled {
                    language_settings.enabled = enabled;
                }
                match language_patch.command_override {
                    CommandOverridePatch::Missing => {}
                    CommandOverridePatch::Null => language_settings.command_override = None,
                    CommandOverridePatch::Value(command_override) => {
                        language_settings.command_override = Some(command_override);
                    }
                }
            }
            if let Some(custom_adapters) = patch.custom_adapters {
                settings.custom_adapters = custom_adapters;
            }
        })
        .await
        .map_err(authority_error)?;
    snapshot_response(&snapshot)
}

pub async fn refresh_all(State(state): State<DashboardState>) -> ApiResult {
    let snapshot = authority(&state)?
        .refresh_all()
        .await
        .map_err(authority_error)?;
    snapshot_response(&snapshot)
}

pub async fn refresh_language(
    State(state): State<DashboardState>,
    AxumPath(language): AxumPath<String>,
) -> ApiResult {
    let snapshot = authority(&state)?
        .refresh_language(&language)
        .await
        .map_err(authority_error)?;
    snapshot_response(&snapshot)
}

/// The snapshot plus the compare-and-set token for its settings. Every read
/// publishes it, so an editor always holds the revision its next write must
/// be checked against.
fn snapshot_response(snapshot: &DiagnosticsSnapshot) -> ApiResult {
    let revision = settings_revision(&snapshot.settings).map_err(internal_error)?;
    let mut payload = json!(snapshot);
    payload["settings_revision"] = json!(revision);
    Ok(Json(payload))
}

fn authority(
    state: &DashboardState,
) -> std::result::Result<&DashboardDiagnosticsAuthorityV1, JsonError> {
    state.code_diagnostics_authority.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(http_detail(
                "canonical daemon diagnostics authority is unavailable",
            )),
        )
    })
}

fn deserialize_command_override_patch<'de, D>(
    deserializer: D,
) -> std::result::Result<CommandOverridePatch, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match Option::<String>::deserialize(deserializer)? {
        Some(value) => CommandOverridePatch::Value(value),
        None => CommandOverridePatch::Null,
    })
}

fn bad_request(error: &impl ToString) -> JsonError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "detail": error.to_string(),
        })),
    )
}

fn internal_error(error: impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&error.to_string())),
    )
}

fn authority_error(error: DashboardDiagnosticsErrorV1) -> JsonError {
    match &error {
        DashboardDiagnosticsErrorV1::AdapterUnavailable { .. }
        | DashboardDiagnosticsErrorV1::LanguageDisabled { .. } => bad_request(&error),
        DashboardDiagnosticsErrorV1::RevisionConflict { expected, actual } => (
            StatusCode::CONFLICT,
            Json(json!({
                "code": "code_diagnostics_revision_conflict",
                "detail": error.to_string(),
                "expected_revision": expected,
                "actual_revision": actual,
            })),
        ),
        DashboardDiagnosticsErrorV1::Runtime(_) => internal_error(error),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::application::dashboard_diagnostics::diagnostic_broker;
    use tracedecay_lsp::analyzer::settings::CodeDiagnosticsSettings;

    async fn state_for_test() -> (tempfile::TempDir, DashboardState) {
        crate::events_api::dashboard_state_fixture("project.dashboard-code-diagnostics").await
    }

    #[tokio::test]
    async fn overview_delegates_to_the_exact_mounted_diagnostics_authority() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        let mut settings = CodeDiagnosticsSettings {
            idle_backfill: IdleBackfillMode::Off,
            ..CodeDiagnosticsSettings::default()
        };
        settings.set_language_enabled("rust", false);
        let authority = DashboardDiagnosticsAuthorityV1::new(
            state.project_root.clone(),
            state.dashboard_root.clone(),
            Arc::clone(&state.mem_db),
            Arc::new(tokio::sync::Mutex::new(diagnostic_broker(
                state.project_root.clone(),
                settings,
            ))),
        );
        let expected = authority.overview().await.expect("authority snapshot");
        state.code_diagnostics_authority = Some(authority);

        let Json(actual) = overview(State(state)).await.expect("diagnostics overview");

        assert_eq!(actual["settings"]["idle_backfill"], json!("off"));
        assert_eq!(actual["settings"]["languages"]["rust"]["enabled"], false);
        // Every read publishes the compare-and-set token for the settings it
        // just reported, so the next write is checked against this exact state.
        assert_eq!(
            actual["settings_revision"],
            json!(settings_revision(&expected.settings).expect("settings revision"))
        );
        let mut without_revision = actual.clone();
        without_revision
            .as_object_mut()
            .expect("overview object")
            .remove("settings_revision");
        assert_eq!(without_revision, json!(expected));
    }

    #[tokio::test]
    async fn settings_patch_rejects_a_revision_the_authority_no_longer_holds() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        state.code_diagnostics_authority = Some(DashboardDiagnosticsAuthorityV1::new(
            state.project_root.clone(),
            state.dashboard_root.clone(),
            Arc::clone(&state.mem_db),
            Arc::new(tokio::sync::Mutex::new(diagnostic_broker(
                state.project_root.clone(),
                CodeDiagnosticsSettings::default(),
            ))),
        ));

        let (status, Json(body)) = patch_settings(
            State(state),
            Json(json!({
                "expected_revision": format!("sha256:{}", "0".repeat(64)),
                "idle_backfill": "off",
            })),
        )
        .await
        .expect_err("a stale revision must not apply");

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "code_diagnostics_revision_conflict");
        assert_ne!(body["actual_revision"], body["expected_revision"]);
    }

    #[tokio::test]
    async fn settings_patch_without_a_revision_is_rejected_before_any_write() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        state.code_diagnostics_authority = Some(DashboardDiagnosticsAuthorityV1::new(
            state.project_root.clone(),
            state.dashboard_root.clone(),
            Arc::clone(&state.mem_db),
            Arc::new(tokio::sync::Mutex::new(diagnostic_broker(
                state.project_root.clone(),
                CodeDiagnosticsSettings::default(),
            ))),
        ));

        let (status, Json(body)) =
            patch_settings(State(state), Json(json!({ "idle_backfill": "off" })))
                .await
                .expect_err("a write with no revision must not apply");

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("expected_revision")),
            "the rejection must name the missing revision: {body}"
        );
    }

    #[tokio::test]
    async fn overview_returns_service_unavailable_without_mounted_authority() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;

        let (status, Json(body)) = overview(State(state))
            .await
            .expect_err("unmounted authority must fail closed");

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body["detail"],
            "canonical daemon diagnostics authority is unavailable"
        );
    }
}
