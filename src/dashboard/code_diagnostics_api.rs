use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::util::{http_detail, JsonError};
use super::DashboardState;
use crate::diagnostics::lsp::adapters::LspAdapterDefinition;
use crate::diagnostics::lsp::client::LspDocument;
use crate::diagnostics::lsp::settings::{
    save_settings, IdleBackfillMode, LanguageDiagnosticsSettings,
};

type ApiResult = std::result::Result<Json<Value>, JsonError>;

#[derive(Debug, Clone, Deserialize, Default)]
struct SettingsPatch {
    #[serde(default)]
    idle_backfill: Option<IdleBackfillMode>,
    #[serde(default)]
    languages: BTreeMap<String, LanguageDiagnosticsSettings>,
    #[serde(default)]
    custom_adapters: Option<Vec<LspAdapterDefinition>>,
}

pub(crate) async fn overview(State(state): State<DashboardState>) -> ApiResult {
    maybe_spawn_idle_backfill(&state).await;
    let snapshot = state.code_diagnostics.read().await.snapshot();
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
    for (language, language_settings) in patch.languages {
        settings.languages.insert(language, language_settings);
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
    Ok(Json(json!(broker.snapshot())))
}

pub(crate) async fn refresh_all(State(state): State<DashboardState>) -> ApiResult {
    let languages: Vec<String> = state
        .code_diagnostics
        .read()
        .await
        .snapshot()
        .engines
        .into_iter()
        .map(|engine| engine.language)
        .collect();
    for language in languages {
        refresh_one(&state, &language).await?;
    }
    let snapshot = state.code_diagnostics.read().await.snapshot();
    Ok(Json(json!(snapshot)))
}

pub(crate) async fn refresh_language(
    State(state): State<DashboardState>,
    AxumPath(language): AxumPath<String>,
) -> ApiResult {
    refresh_one(&state, &language).await?;
    let snapshot = state.code_diagnostics.read().await.snapshot();
    Ok(Json(json!(snapshot)))
}

async fn refresh_one(state: &DashboardState, language: &str) -> std::result::Result<(), JsonError> {
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

async fn maybe_spawn_idle_backfill(state: &DashboardState) {
    let snapshot = state.code_diagnostics.read().await.snapshot();
    if snapshot.settings.idle_backfill != IdleBackfillMode::Idle {
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
        let languages: Vec<String> = state
            .code_diagnostics
            .read()
            .await
            .snapshot()
            .engines
            .into_iter()
            .filter(|engine| engine.enabled)
            .map(|engine| engine.language)
            .collect();
        for language in languages {
            let _ = refresh_one(&state, &language).await;
            tokio::task::yield_now().await;
        }
    });
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

async fn documents_for_adapter(
    project_root: &Path,
    adapter: &LspAdapterDefinition,
    files: Vec<String>,
) -> crate::errors::Result<Vec<LspDocument>> {
    let mut documents = Vec::new();
    for file in files {
        if !matches_adapter_extension(adapter, &file) {
            continue;
        }
        let path = project_root.join(&file);
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        documents.push(LspDocument {
            language: adapter.language.clone(),
            language_id: language_id_for_file(adapter, &file),
            relative_path: file,
            text,
        });
    }
    Ok(documents)
}

fn language_id_for_file(adapter: &LspAdapterDefinition, file: &str) -> String {
    let extension = Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    match (adapter.language.as_str(), extension) {
        ("typescript", "tsx") => "typescriptreact".to_string(),
        ("javascript", "jsx") => "javascriptreact".to_string(),
        _ => adapter.language_id.clone(),
    }
}

fn matches_adapter_extension(adapter: &LspAdapterDefinition, file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            adapter
                .extensions
                .iter()
                .any(|candidate| candidate == extension)
        })
}

fn bad_request(err: &impl ToString) -> JsonError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "detail": err.to_string(),
        })),
    )
}

fn internal_error(err: &impl ToString) -> JsonError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(&err.to_string())),
    )
}
