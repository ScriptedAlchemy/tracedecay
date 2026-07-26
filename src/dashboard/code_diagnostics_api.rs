use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::DashboardState;
use super::util::{JsonError, http_detail};
use crate::application::dashboard_diagnostics::{
    DashboardDiagnosticsAuthorityV1, DashboardDiagnosticsErrorV1,
};
use crate::diagnostics::lsp::adapters::LspAdapterDefinition;
use crate::diagnostics::lsp::settings::IdleBackfillMode;

type ApiResult = std::result::Result<Json<Value>, JsonError>;

#[derive(Debug, Clone, Deserialize, Default)]
struct SettingsPatch {
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

pub(crate) async fn overview(State(state): State<DashboardState>) -> ApiResult {
    let snapshot = authority(&state)?
        .overview()
        .await
        .map_err(authority_error)?;
    Ok(Json(json!(snapshot)))
}

pub(crate) async fn patch_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<SettingsPatch>(patch).map_err(|error| {
        bad_request(&format!("invalid code diagnostics settings patch: {error}"))
    })?;
    let authority = authority(&state)?;
    let mut settings = authority
        .snapshot()
        .await
        .map_err(authority_error)?
        .settings;
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
    let snapshot = authority
        .update_settings(settings)
        .await
        .map_err(authority_error)?;
    Ok(Json(json!(snapshot)))
}

pub(crate) async fn refresh_all(State(state): State<DashboardState>) -> ApiResult {
    let snapshot = authority(&state)?
        .refresh_all()
        .await
        .map_err(authority_error)?;
    Ok(Json(json!(snapshot)))
}

pub(crate) async fn refresh_language(
    State(state): State<DashboardState>,
    AxumPath(language): AxumPath<String>,
) -> ApiResult {
    let snapshot = authority(&state)?
        .refresh_language(&language)
        .await
        .map_err(authority_error)?;
    Ok(Json(json!(snapshot)))
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
        DashboardDiagnosticsErrorV1::AdapterUnavailable { .. } => bad_request(&error),
        DashboardDiagnosticsErrorV1::Runtime(_) => internal_error(error),
    }
}
