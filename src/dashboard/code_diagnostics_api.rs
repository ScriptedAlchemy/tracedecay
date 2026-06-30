use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

use super::util::{http_detail, JsonError};
use super::DashboardState;
use crate::diagnostics::lsp::activity::{active_languages_for_files, documents_for_adapter};
use crate::diagnostics::lsp::adapters::LspAdapterDefinition;
use crate::diagnostics::lsp::broker::EngineState;
use crate::diagnostics::lsp::settings::{save_settings, IdleBackfillMode};

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
    let snapshot = diagnostics_snapshot(&state).await?;
    maybe_spawn_idle_backfill(&state, &snapshot);
    Ok(Json(json!(snapshot)))
}

pub(crate) async fn patch_settings(
    State(state): State<DashboardState>,
    Json(patch): Json<Value>,
) -> ApiResult {
    let patch = serde_json::from_value::<SettingsPatch>(patch)
        .map_err(|err| bad_request(&format!("invalid code diagnostics settings patch: {err}")))?;
    let mut settings = state.code_diagnostics.read().await.snapshot().settings;
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
            CommandOverridePatch::Null => {
                language_settings.command_override = None;
            }
            CommandOverridePatch::Value(command_override) => {
                language_settings.command_override = Some(command_override);
            }
        }
    }
    if let Some(custom_adapters) = patch.custom_adapters {
        settings.custom_adapters = custom_adapters;
    }
    save_settings(&state.dashboard_root, &settings)
        .await
        .map_err(|err| internal_error(&err))?;
    let mut adapters = crate::diagnostics::lsp::adapters::builtin_adapters();
    adapters.extend(settings.custom_adapters.clone());
    let mut broker = state.code_diagnostics.write().await;
    broker.update_adapters(adapters);
    broker.update_settings(settings);
    drop(broker);
    let snapshot = diagnostics_snapshot(&state).await?;
    Ok(Json(json!(snapshot)))
}

pub(crate) async fn refresh_all(State(state): State<DashboardState>) -> ApiResult {
    let languages = refreshable_languages(&state).await?;
    for language in languages {
        refresh_one_reconciled(&state, &language).await?;
    }
    let snapshot = diagnostics_snapshot(&state).await?;
    Ok(Json(json!(snapshot)))
}

pub(crate) async fn refresh_language(
    State(state): State<DashboardState>,
    AxumPath(language): AxumPath<String>,
) -> ApiResult {
    refresh_one(&state, &language).await?;
    let snapshot = diagnostics_snapshot(&state).await?;
    Ok(Json(json!(snapshot)))
}

async fn refresh_one(state: &DashboardState, language: &str) -> std::result::Result<(), JsonError> {
    reconcile_project_language_activity(state).await?;
    refresh_one_reconciled(state, language).await
}

async fn refresh_one_reconciled(
    state: &DashboardState,
    language: &str,
) -> std::result::Result<(), JsonError> {
    let snapshot = state.code_diagnostics.read().await.snapshot();
    if !snapshot.settings.language_enabled(language) {
        state
            .code_diagnostics
            .write()
            .await
            .set_language_enabled(language, false);
        return Ok(());
    }
    let Some(adapter) = state.code_diagnostics.read().await.adapter_for(language) else {
        return Err(bad_request(&format!(
            "no code diagnostics adapter registered for language '{language}'"
        )));
    };
    let files = indexed_files(&state.graph_conn)
        .await
        .map_err(|err| internal_error(&err))?;
    let documents = documents_for_adapter(&state.project_root, &adapter, files)
        .await
        .map_err(|err| internal_error(&err))?;
    let document_count = documents.len();
    state
        .code_diagnostics
        .write()
        .await
        .record_backfill_progress(language, document_count, document_count, 0, None);
    if documents.is_empty() {
        state
            .code_diagnostics
            .write()
            .await
            .record_backfill_progress(
                language,
                0,
                0,
                0,
                Some(crate::tracedecay::current_timestamp()),
            );
        return Ok(());
    }
    let prepared = state
        .code_diagnostics
        .write()
        .await
        .prepare_refresh(language, documents);
    let refresh_ok = match prepared {
        Ok(Some(prepared)) => {
            let completed = prepared.collect_diagnostics(Duration::from_secs(5)).await;
            let refresh_ok = completed.is_ok();
            state
                .code_diagnostics
                .write()
                .await
                .finish_refresh(completed)
                .ok();
            refresh_ok
        }
        Ok(None) => true,
        Err(_) => false,
    };
    let snapshot = state.code_diagnostics.read().await.snapshot();
    let files_with_diagnostics = snapshot
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.language == language)
        .map(|diagnostic| diagnostic.file.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    state
        .code_diagnostics
        .write()
        .await
        .record_backfill_progress(
            language,
            document_count,
            document_count,
            files_with_diagnostics,
            refresh_ok.then(crate::tracedecay::current_timestamp),
        );
    Ok(())
}

fn maybe_spawn_idle_backfill(
    state: &DashboardState,
    snapshot: &crate::diagnostics::lsp::broker::DiagnosticsSnapshot,
) {
    if snapshot.settings.idle_backfill != IdleBackfillMode::Idle {
        return;
    }
    let languages = backfill_languages(snapshot);
    if languages.is_empty() {
        return;
    }
    if state
        .code_diagnostics_backfill_started
        .swap(true, Ordering::AcqRel)
    {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(750)).await;
        for language in languages {
            let _ = refresh_one(&state, &language).await;
            tokio::task::yield_now().await;
        }
    });
}

async fn diagnostics_snapshot(
    state: &DashboardState,
) -> std::result::Result<crate::diagnostics::lsp::broker::DiagnosticsSnapshot, JsonError> {
    reconcile_project_language_activity(state).await?;
    Ok(state.code_diagnostics.read().await.snapshot())
}

async fn refreshable_languages(
    state: &DashboardState,
) -> std::result::Result<Vec<String>, JsonError> {
    let snapshot = diagnostics_snapshot(state).await?;
    Ok(backfill_languages(&snapshot))
}

fn backfill_languages(
    snapshot: &crate::diagnostics::lsp::broker::DiagnosticsSnapshot,
) -> Vec<String> {
    snapshot
        .engines
        .iter()
        .filter(|engine| {
            engine.enabled
                && !matches!(
                    engine.state,
                    EngineState::Disabled | EngineState::Inactive | EngineState::Unavailable
                )
        })
        .map(|engine| engine.language.clone())
        .collect()
}

async fn reconcile_project_language_activity(
    state: &DashboardState,
) -> std::result::Result<(), JsonError> {
    let files = indexed_files(&state.graph_conn)
        .await
        .map_err(|err| internal_error(&err))?;
    let adapters = {
        let broker = state.code_diagnostics.read().await;
        broker
            .snapshot()
            .engines
            .into_iter()
            .filter_map(|engine| broker.adapter_for(&engine.language))
            .collect::<Vec<_>>()
    };
    let active_languages = active_languages_for_files(&state.project_root, &adapters, &files);
    state
        .code_diagnostics
        .write()
        .await
        .update_project_languages(active_languages);
    Ok(())
}

async fn indexed_files(conn: &libsql::Connection) -> crate::errors::Result<Vec<String>> {
    let mut rows = conn
        .query("SELECT path FROM files ORDER BY path ASC", ())
        .await?;
    let mut files = Vec::new();
    while let Some(row) = rows.next().await? {
        if let Ok(path) = row.get::<String>(0) {
            files.push(path);
        }
    }
    Ok(files)
}

fn bad_request(err: &impl ToString) -> JsonError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "detail": err.to_string(),
        })),
    )
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

fn internal_error(err: &impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&err.to_string())),
    )
}
