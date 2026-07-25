//! Thin HTTP adapter for canonical code-diagnostics application operations.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use tracedecay_domain::ManifestDigest;

use super::DashboardState;
use super::util::{JsonError, http_detail};
use crate::application::code_diagnostics_control::{
    CodeDiagnosticsControl, CodeDiagnosticsOperationReceiptV1, CodeDiagnosticsRefreshPreviewV1,
    CodeDiagnosticsRefreshTargetV1, CodeDiagnosticsSettingsPatchV1,
    CodeDiagnosticsSettingsPreviewV1, settings_revision,
};
use crate::diagnostics::lsp::broker::{DiagnosticsSnapshot, EngineState};
use crate::diagnostics::lsp::settings::IdleBackfillMode;

type ApiResult = std::result::Result<Json<Value>, JsonError>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettingsApplyBodyV1 {
    preview: CodeDiagnosticsSettingsPreviewV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefreshPreviewBodyV1 {
    target: CodeDiagnosticsRefreshTargetV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefreshApplyBodyV1 {
    preview: CodeDiagnosticsRefreshPreviewV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RollbackBodyV1 {
    expected_revision: ManifestDigest,
}

pub(crate) async fn overview(State(state): State<DashboardState>) -> ApiResult {
    let application = application(&state)?;
    let snapshot = application.snapshot().await.map_err(internal_error)?;
    maybe_spawn_idle_backfill(&state, &snapshot);
    snapshot_response(&snapshot, None)
}

/// Compatibility route: preview and apply remain separate application
/// operations even when the legacy HTTP shape requests them in one call.
pub(crate) async fn patch_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch =
        serde_json::from_value::<CodeDiagnosticsSettingsPatchV1>(patch).map_err(|error| {
            bad_request(&format!("invalid code diagnostics settings patch: {error}"))
        })?;
    let application = application(&state)?;
    let preview = application
        .preview_settings(patch)
        .await
        .map_err(operation_error)?;
    let receipt = application
        .apply_settings(preview)
        .await
        .map_err(operation_error)?;
    let snapshot = application.snapshot().await.map_err(internal_error)?;
    snapshot_response(&snapshot, Some(&receipt))
}

pub(crate) async fn preview_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch =
        serde_json::from_value::<CodeDiagnosticsSettingsPatchV1>(patch).map_err(|error| {
            bad_request(&format!("invalid code diagnostics settings patch: {error}"))
        })?;
    let preview = application(&state)?
        .preview_settings(patch)
        .await
        .map_err(operation_error)?;
    Ok(Json(json!({ "preview": preview })))
}

pub(crate) async fn apply_settings(
    State(state): State<DashboardState>,
    Json(body): Json<SettingsApplyBodyV1>,
) -> ApiResult {
    let application = application(&state)?;
    let receipt = application
        .apply_settings(body.preview)
        .await
        .map_err(operation_error)?;
    let snapshot = application.snapshot().await.map_err(internal_error)?;
    snapshot_response(&snapshot, Some(&receipt))
}

pub(crate) async fn refresh_all(State(state): State<DashboardState>) -> ApiResult {
    refresh(&state, CodeDiagnosticsRefreshTargetV1::All).await
}

pub(crate) async fn refresh_language(
    State(state): State<DashboardState>,
    AxumPath(language): AxumPath<String>,
) -> ApiResult {
    refresh(&state, CodeDiagnosticsRefreshTargetV1::Language(language)).await
}

pub(crate) async fn preview_refresh(
    State(state): State<DashboardState>,
    Json(body): Json<RefreshPreviewBodyV1>,
) -> ApiResult {
    let preview = application(&state)?
        .preview_refresh(body.target)
        .await
        .map_err(operation_error)?;
    Ok(Json(json!({ "preview": preview })))
}

pub(crate) async fn apply_refresh(
    State(state): State<DashboardState>,
    Json(body): Json<RefreshApplyBodyV1>,
) -> ApiResult {
    let application = application(&state)?;
    let receipt = application
        .apply_refresh(body.preview)
        .await
        .map_err(operation_error)?;
    let snapshot = application.snapshot().await.map_err(internal_error)?;
    snapshot_response(&snapshot, Some(&receipt))
}

pub(crate) async fn operation_status(
    State(state): State<DashboardState>,
    AxumPath(operation_id): AxumPath<String>,
) -> ApiResult {
    let operation_id = ManifestDigest::new(operation_id).map_err(|error| bad_request(&error))?;
    let receipt = application(&state)?
        .status(&operation_id)
        .await
        .map_err(operation_error)?;
    Ok(Json(json!({ "operation": receipt })))
}

pub(crate) async fn rollback_settings(
    State(state): State<DashboardState>,
    AxumPath(operation_id): AxumPath<String>,
    Json(body): Json<RollbackBodyV1>,
) -> ApiResult {
    let operation_id = ManifestDigest::new(operation_id).map_err(|error| bad_request(&error))?;
    let application = application(&state)?;
    let status = application
        .status(&operation_id)
        .await
        .map_err(operation_error)?;
    let rollback = application
        .rollback_settings(&status.receipt, body.expected_revision)
        .await
        .map_err(operation_error)?;
    let snapshot = application.snapshot().await.map_err(internal_error)?;
    snapshot_response(&snapshot, Some(&rollback))
}

async fn refresh(state: &DashboardState, target: CodeDiagnosticsRefreshTargetV1) -> ApiResult {
    let application = application(state)?;
    let preview = application
        .preview_refresh(target)
        .await
        .map_err(operation_error)?;
    let receipt = application
        .apply_refresh(preview)
        .await
        .map_err(operation_error)?;
    let snapshot = application.snapshot().await.map_err(internal_error)?;
    snapshot_response(&snapshot, Some(&receipt))
}

fn application(state: &DashboardState) -> std::result::Result<CodeDiagnosticsControl, JsonError> {
    let raw = state.project_id.as_deref().ok_or_else(|| {
        (
            StatusCode::FORBIDDEN,
            Json(json!({ "detail": "project diagnostics authority is unavailable" })),
        )
    })?;
    let project_id = tracedecay_domain::ProjectId::new(raw).map_err(internal_error)?;
    Ok(CodeDiagnosticsControl::new(
        project_id,
        state.project_root.clone(),
        state.dashboard_root.clone(),
        Arc::clone(&state.mem_db),
        Arc::clone(&state.code_diagnostics),
    ))
}

fn snapshot_response(
    snapshot: &DiagnosticsSnapshot,
    receipt: Option<&CodeDiagnosticsOperationReceiptV1>,
) -> ApiResult {
    let mut payload = serde_json::to_value(snapshot).map_err(internal_error)?;
    payload["settings_revision"] =
        json!(settings_revision(&snapshot.settings).map_err(internal_error)?);
    if let Some(receipt) = receipt {
        payload["operation"] = json!(receipt);
    }
    Ok(Json(payload))
}

fn maybe_spawn_idle_backfill(state: &DashboardState, snapshot: &DiagnosticsSnapshot) {
    if snapshot.settings.idle_backfill != IdleBackfillMode::Idle
        || !snapshot.engines.iter().any(|engine| {
            engine.enabled
                && !matches!(
                    engine.state,
                    EngineState::Disabled | EngineState::Inactive | EngineState::Unavailable
                )
        })
        || state
            .code_diagnostics_backfill_started
            .swap(true, Ordering::AcqRel)
    {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(750)).await;
        let Ok(application) = application(&state) else {
            return;
        };
        let Ok(preview) = application
            .preview_refresh(CodeDiagnosticsRefreshTargetV1::All)
            .await
        else {
            return;
        };
        let _ = application.apply_refresh(preview).await;
    });
}

fn bad_request(error: &impl ToString) -> JsonError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "detail": error.to_string() })),
    )
}

fn operation_error(error: impl ToString) -> JsonError {
    let detail = error.to_string();
    let status = if detail.contains("revision conflict") || detail.contains("stale") {
        StatusCode::CONFLICT
    } else if detail.contains("not authorized") {
        StatusCode::FORBIDDEN
    } else if detail.contains("invalid")
        || detail.contains("disabled")
        || detail.contains("not refreshable")
    {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(json!({ "detail": detail })))
}

fn internal_error(error: impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&error.to_string())),
    )
}
