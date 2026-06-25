use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::diagnostics::lsp::adapters::{LspAdapterDefinition, LspInstallOption};
use crate::diagnostics::lsp::client::{LspDocument, StdioLspClient};
use crate::diagnostics::lsp::settings::CodeDiagnosticsSettings;
use crate::errors::{Result, TraceDecayError};

/// Normalized code diagnostic shared by the LSP broker and dashboard API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeDiagnostic {
    pub language: String,
    pub source: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub character_start: Option<u32>,
    pub character_end: Option<u32>,
    pub severity: DiagnosticSeverity,
    pub code: Option<String>,
    pub message: String,
    pub enclosing_node: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineState {
    Unavailable,
    Disabled,
    Starting,
    Indexing,
    Ready,
    Refreshing,
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStatus {
    pub language: String,
    pub language_id: String,
    pub command: String,
    pub default_command: String,
    pub args: Vec<String>,
    pub enabled: bool,
    pub state: EngineState,
    pub install_options: Vec<LspInstallOption>,
    pub last_error: Option<String>,
    pub last_diagnostic_update: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BackfillProgress {
    pub queued_files: usize,
    pub opened_files: usize,
    pub files_with_diagnostics: usize,
    pub last_completed_sweep: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiagnosticsSummary {
    pub total_errors: usize,
    pub total_warnings: usize,
    pub pending_refreshes: usize,
    pub last_refresh_age_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsSnapshot {
    pub summary: DiagnosticsSummary,
    pub engines: Vec<EngineStatus>,
    pub diagnostics: Vec<CodeDiagnostic>,
    pub backfill: BTreeMap<String, BackfillProgress>,
    pub settings: CodeDiagnosticsSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LspSessionKey {
    language: String,
    command: String,
    workspace_root: PathBuf,
}

struct RefreshBatch {
    workspace_root: PathBuf,
    documents: Vec<LspDocument>,
    client: Arc<Mutex<Option<StdioLspClient>>>,
}

pub struct PreparedRefresh {
    language: String,
    project_root: PathBuf,
    command: String,
    args: Vec<String>,
    batches: Vec<RefreshBatch>,
}

pub struct CompletedRefresh {
    language: String,
    command: String,
    result: std::result::Result<Vec<CodeDiagnostic>, String>,
}

impl CompletedRefresh {
    pub fn is_ok(&self) -> bool {
        self.result.is_ok()
    }
}

impl PreparedRefresh {
    pub async fn collect_diagnostics(self, diagnostics_timeout: Duration) -> CompletedRefresh {
        let language = self.language.clone();
        let command = self.command.clone();
        let result = self.collect(diagnostics_timeout).await;
        CompletedRefresh {
            language,
            command,
            result,
        }
    }

    async fn collect(
        self,
        diagnostics_timeout: Duration,
    ) -> std::result::Result<Vec<CodeDiagnostic>, String> {
        let mut diagnostics = Vec::new();
        for batch in self.batches {
            let mut client_slot = batch.client.lock().await;
            if client_slot.is_none() {
                *client_slot = Some(
                    StdioLspClient::start(&self.command, &self.args, &batch.workspace_root)
                        .await
                        .map_err(|err| err.to_string())?,
                );
            }
            let Some(client) = client_slot.as_mut() else {
                return Err("LSP client should be initialized".to_string());
            };
            let result = client
                .collect_document_diagnostics(
                    &self.project_root,
                    batch.documents,
                    diagnostics_timeout,
                )
                .await;
            match result {
                Ok(mut batch_diagnostics) => diagnostics.append(&mut batch_diagnostics),
                Err(err) => {
                    *client_slot = None;
                    return Err(err.to_string());
                }
            }
        }
        Ok(diagnostics)
    }
}

/// Dashboard-owned diagnostics broker state.
pub struct DiagnosticBroker {
    project_root: PathBuf,
    adapters: Vec<LspAdapterDefinition>,
    settings: CodeDiagnosticsSettings,
    diagnostics: Vec<CodeDiagnostic>,
    clients: BTreeMap<LspSessionKey, Arc<Mutex<Option<StdioLspClient>>>>,
    engine_overrides: BTreeMap<String, EngineState>,
    engine_errors: BTreeMap<String, String>,
    backfill: BTreeMap<String, BackfillProgress>,
}

impl DiagnosticBroker {
    pub fn new(
        project_root: impl Into<PathBuf>,
        adapters: Vec<LspAdapterDefinition>,
        settings: CodeDiagnosticsSettings,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            adapters,
            settings,
            diagnostics: Vec::new(),
            clients: BTreeMap::new(),
            engine_overrides: BTreeMap::new(),
            engine_errors: BTreeMap::new(),
            backfill: BTreeMap::new(),
        }
    }

    pub fn new_for_test(
        project_root: impl Into<PathBuf>,
        adapters: Vec<LspAdapterDefinition>,
    ) -> Self {
        Self::new(project_root, adapters, CodeDiagnosticsSettings::default())
    }

    pub fn snapshot(&self) -> DiagnosticsSnapshot {
        DiagnosticsSnapshot {
            summary: self.summary(),
            engines: self.engine_statuses(),
            diagnostics: self.diagnostics.clone(),
            backfill: self.backfill.clone(),
            settings: self.settings.clone(),
        }
    }

    pub fn adapter_for(&self, language: &str) -> Option<LspAdapterDefinition> {
        self.adapters
            .iter()
            .find(|adapter| adapter.language == language)
            .cloned()
    }

    pub fn update_adapters(&mut self, adapters: Vec<LspAdapterDefinition>) {
        self.adapters = adapters;
        self.clients.clear();
    }

    pub fn set_language_enabled(&mut self, language: &str, enabled: bool) {
        self.settings.set_language_enabled(language, enabled);
        if enabled {
            self.engine_overrides.remove(language);
        } else {
            self.engine_overrides
                .insert(language.to_string(), EngineState::Disabled);
            self.remove_language_clients(language);
            self.clear_language(language);
        }
    }

    pub fn prepare_refresh(
        &mut self,
        language: &str,
        documents: Vec<LspDocument>,
    ) -> Result<Option<PreparedRefresh>> {
        if !self.settings.language_enabled(language) {
            self.engine_overrides
                .insert(language.to_string(), EngineState::Disabled);
            self.remove_language_clients(language);
            self.clear_language(language);
            return Ok(None);
        }
        let adapter = self
            .adapters
            .iter()
            .find(|adapter| adapter.language == language)
            .cloned()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("no LSP adapter registered for language '{language}'"),
            })?;

        let command = self.settings.command_for(language, &adapter.command);
        if !command_available(&command) {
            let message = format!("LSP command '{command}' is not available on PATH");
            self.engine_errors
                .insert(language.to_string(), message.clone());
            self.engine_overrides
                .insert(language.to_string(), EngineState::Unavailable);
            self.remove_language_clients(language);
            return Err(TraceDecayError::Config { message });
        }

        self.engine_overrides
            .insert(language.to_string(), EngineState::Refreshing);
        let project_root = self.project_root.clone();
        let mut documents_by_root: BTreeMap<PathBuf, Vec<LspDocument>> = BTreeMap::new();
        for document in documents {
            let workspace_root =
                workspace_root_for_document(&self.project_root, &adapter, &document);
            documents_by_root
                .entry(workspace_root)
                .or_default()
                .push(document);
        }
        let batches = documents_by_root
            .into_iter()
            .map(|(workspace_root, documents)| {
                let session_key = LspSessionKey {
                    language: language.to_string(),
                    command: command.clone(),
                    workspace_root: workspace_root.clone(),
                };
                let client = self
                    .clients
                    .entry(session_key.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(None)))
                    .clone();
                RefreshBatch {
                    workspace_root,
                    documents,
                    client,
                }
            })
            .collect();
        Ok(Some(PreparedRefresh {
            language: language.to_string(),
            project_root,
            command,
            args: adapter.args,
            batches,
        }))
    }

    pub async fn refresh_documents(
        &mut self,
        language: &str,
        documents: Vec<LspDocument>,
        diagnostics_timeout: Duration,
    ) -> Result<()> {
        let Some(prepared) = self.prepare_refresh(language, documents)? else {
            return Ok(());
        };
        let result = prepared.collect_diagnostics(diagnostics_timeout).await;
        self.finish_refresh(result)
    }

    pub fn finish_refresh(&mut self, completed: CompletedRefresh) -> Result<()> {
        let language = completed.language;
        if !self.settings.language_enabled(&language) {
            self.engine_overrides
                .insert(language.clone(), EngineState::Disabled);
            self.remove_language_clients(&language);
            self.clear_language(&language);
            return Ok(());
        }
        if !self.command_matches_current_settings(&language, &completed.command) {
            return Ok(());
        }
        match completed.result {
            Ok(mut diagnostics) => {
                self.diagnostics
                    .retain(|diagnostic| diagnostic.language != language);
                self.diagnostics.append(&mut diagnostics);
                self.engine_errors.remove(&language);
                self.engine_overrides.insert(language, EngineState::Ready);
                Ok(())
            }
            Err(message) => {
                self.engine_errors.insert(language.clone(), message.clone());
                self.engine_overrides
                    .insert(language.clone(), EngineState::Crashed);
                self.remove_language_clients(&language);
                Err(TraceDecayError::Config { message })
            }
        }
    }

    pub fn update_settings(&mut self, settings: CodeDiagnosticsSettings) {
        self.settings = settings;
        self.clients.clear();
        self.engine_overrides.clear();
        let disabled_languages: Vec<String> = self
            .settings
            .languages
            .iter()
            .filter(|(_, settings)| !settings.enabled)
            .map(|(language, _)| language.clone())
            .collect();
        for language in disabled_languages {
            self.engine_overrides
                .insert(language.clone(), EngineState::Disabled);
            self.clear_language(&language);
        }
    }

    pub fn cache_diagnostic(&mut self, diagnostic: CodeDiagnostic) {
        self.diagnostics.push(diagnostic);
    }

    pub fn record_backfill_progress(
        &mut self,
        language: &str,
        queued_files: usize,
        opened_files: usize,
        files_with_diagnostics: usize,
        last_completed_sweep: Option<i64>,
    ) {
        self.backfill.insert(
            language.to_string(),
            BackfillProgress {
                queued_files,
                opened_files,
                files_with_diagnostics,
                last_completed_sweep,
            },
        );
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    fn clear_language(&mut self, language: &str) {
        self.diagnostics
            .retain(|diagnostic| diagnostic.language != language);
        self.backfill.remove(language);
    }

    fn remove_language_clients(&mut self, language: &str) {
        self.clients
            .retain(|key, _| key.language.as_str() != language);
    }

    fn command_matches_current_settings(&self, language: &str, command: &str) -> bool {
        self.adapters
            .iter()
            .find(|adapter| adapter.language == language)
            .is_some_and(|adapter| self.settings.command_for(language, &adapter.command) == command)
    }

    fn summary(&self) -> DiagnosticsSummary {
        let total_errors = self
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
            .count();
        let total_warnings = self
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Warning)
            .count();
        DiagnosticsSummary {
            total_errors,
            total_warnings,
            pending_refreshes: 0,
            last_refresh_age_seconds: None,
        }
    }

    fn engine_statuses(&self) -> Vec<EngineStatus> {
        self.adapters
            .iter()
            .map(|adapter| {
                let enabled = self.settings.language_enabled(&adapter.language);
                let command = self
                    .settings
                    .command_for(&adapter.language, &adapter.command);
                let state = self
                    .engine_overrides
                    .get(&adapter.language)
                    .copied()
                    .unwrap_or_else(|| default_state(enabled, &command));
                let last_diagnostic_update = self
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.language == adapter.language)
                    .map(|diagnostic| diagnostic.updated_at)
                    .max();
                EngineStatus {
                    language: adapter.language.clone(),
                    language_id: adapter.language_id.clone(),
                    command,
                    default_command: adapter.command.clone(),
                    args: adapter.args.clone(),
                    enabled,
                    state,
                    install_options: adapter.install_options.clone(),
                    last_error: self.engine_errors.get(&adapter.language).cloned(),
                    last_diagnostic_update,
                }
            })
            .collect()
    }
}

fn default_state(enabled: bool, command: &str) -> EngineState {
    if !enabled {
        return EngineState::Disabled;
    }
    if command_available(command) {
        EngineState::Ready
    } else {
        EngineState::Unavailable
    }
}

fn workspace_root_for_document(
    project_root: &Path,
    adapter: &LspAdapterDefinition,
    document: &LspDocument,
) -> PathBuf {
    let file = project_root.join(&document.relative_path);
    let mut current = file.parent();
    while let Some(dir) = current {
        if adapter
            .root_markers
            .iter()
            .any(|marker| dir.join(marker).exists())
        {
            return dir.to_path_buf();
        }
        if dir == project_root {
            break;
        }
        current = dir.parent();
    }
    project_root.to_path_buf()
}

pub fn command_available(command: &str) -> bool {
    if Path::new(command).components().count() > 1 {
        return Path::new(command).is_file();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    let candidates = command_candidates(command);
    std::env::split_paths(&paths).any(|path| {
        candidates
            .iter()
            .any(|candidate| path.join(candidate).is_file())
    })
}

#[cfg(windows)]
fn command_candidates(command: &str) -> Vec<String> {
    if Path::new(command).extension().is_some() {
        return vec![command.to_string()];
    }

    let pathext = std::env::var_os("PATHEXT")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());

    let mut candidates = vec![command.to_string()];
    candidates.extend(pathext.split(';').filter_map(|extension| {
        let extension = extension.trim();
        if extension.is_empty() {
            None
        } else if extension.starts_with('.') {
            Some(format!("{command}{extension}"))
        } else {
            Some(format!("{command}.{extension}"))
        }
    }));
    candidates
}

#[cfg(not(windows))]
fn command_candidates(command: &str) -> Vec<String> {
    vec![command.to_string()]
}
