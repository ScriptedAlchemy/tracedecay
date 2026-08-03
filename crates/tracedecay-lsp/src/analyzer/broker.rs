use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::activity::adapter_workspace_root;
use crate::adapters::{LspAdapterDefinition, LspInstallOption};
use crate::client::{LspDocument, LspRefreshTimeouts, StdioLspClient};
use crate::settings::CodeDiagnosticsSettings;
use crate::{LspError, Result};

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

/// Minimal indexed-symbol span used to attribute a diagnostic to the smallest
/// enclosing code-graph node. Line numbers are 0-based, matching
/// [`crate::types::Node`]; they are compared against a diagnostic's 1-based line
/// inside [`enclosing_node_for_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeSpan {
    pub start_line: u32,
    pub end_line: u32,
    pub qualified_name: String,
}

/// Returns the qualified name of the smallest span that encloses `line_1based`,
/// or `None` when no span covers it. Shared by the LSP broker and the
/// `tracedecay_diagnostics` handler so both attribute diagnostics identically.
pub fn enclosing_node_for_line(spans: &[NodeSpan], line_1based: u32) -> Option<String> {
    if line_1based == 0 {
        return None;
    }
    let node_line = line_1based - 1;
    spans
        .iter()
        .filter(|span| span.start_line <= node_line && node_line <= span.end_line)
        .min_by_key(|span| span.end_line.saturating_sub(span.start_line))
        .map(|span| span.qualified_name.clone())
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
    Inactive,
    Available,
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
    epoch: u64,
    batches: Vec<RefreshBatch>,
}

pub struct CompletedRefresh {
    language: String,
    command: String,
    epoch: u64,
    result: std::result::Result<Vec<CodeDiagnostic>, RefreshFailure>,
}

impl CompletedRefresh {
    pub fn is_ok(&self) -> bool {
        self.result.is_ok()
    }
}

impl PreparedRefresh {
    pub async fn collect_diagnostics(
        self,
        diagnostics_quiet_timeout: Duration,
    ) -> CompletedRefresh {
        self.collect_diagnostics_with_timeouts(LspRefreshTimeouts::from_diagnostics_quiet_window(
            diagnostics_quiet_timeout,
        ))
        .await
    }

    pub async fn collect_diagnostics_with_timeouts(
        self,
        timeouts: LspRefreshTimeouts,
    ) -> CompletedRefresh {
        let language = self.language.clone();
        let command = self.command.clone();
        let epoch = self.epoch;
        let result = self.collect(timeouts).await;
        CompletedRefresh {
            language,
            command,
            epoch,
            result,
        }
    }

    async fn collect(
        self,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Vec<CodeDiagnostic>, RefreshFailure> {
        let mut diagnostics = Vec::new();
        for batch in self.batches {
            let mut client_slot = batch.client.lock().await;
            let mut client = match client_slot.take() {
                Some(client) => client,
                None => StdioLspClient::start_with_timeouts(
                    &self.command,
                    &self.args,
                    &batch.workspace_root,
                    timeouts,
                )
                .await
                .map_err(|err| RefreshFailure::crashed(&err))?,
            };
            match client
                .collect_document_diagnostics(&self.project_root, batch.documents, timeouts)
                .await
            {
                Ok(mut batch_diagnostics) => {
                    *client_slot = Some(client);
                    diagnostics.append(&mut batch_diagnostics);
                }
                Err(err) => {
                    *client_slot = None;
                    return Err(RefreshFailure::crashed(&err));
                }
            }
        }
        Ok(diagnostics)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefreshFailure {
    state: EngineState,
    message: String,
}

impl RefreshFailure {
    fn crashed(err: &LspError) -> Self {
        Self {
            state: EngineState::Crashed,
            message: err.to_string(),
        }
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
    refresh_epochs: BTreeMap<String, u64>,
    project_languages: BTreeSet<String>,
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
            refresh_epochs: BTreeMap::new(),
            project_languages: BTreeSet::new(),
            backfill: BTreeMap::new(),
        }
    }

    pub fn new_for_test(
        project_root: impl Into<PathBuf>,
        adapters: Vec<LspAdapterDefinition>,
    ) -> Self {
        let project_languages = adapters
            .iter()
            .map(|adapter| adapter.language.clone())
            .collect();
        let mut broker = Self::new(project_root, adapters, CodeDiagnosticsSettings::default());
        broker.update_project_languages(project_languages);
        broker
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

    pub fn update_project_languages(&mut self, languages: BTreeSet<String>) {
        self.project_languages = languages;
        for language in &self.project_languages {
            if self.engine_overrides.get(language) == Some(&EngineState::Inactive) {
                self.engine_overrides.remove(language);
            }
        }
        let inactive_languages: Vec<String> = self
            .adapters
            .iter()
            .filter(|adapter| !self.project_languages.contains(&adapter.language))
            .map(|adapter| adapter.language.clone())
            .collect();
        for language in inactive_languages {
            self.remove_language_clients(&language);
            self.clear_language(&language);
            self.engine_errors.remove(&language);
            if self.engine_overrides.get(&language) != Some(&EngineState::Disabled) {
                self.engine_overrides.remove(&language);
            }
        }
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
        if !self.project_languages.contains(language) {
            self.engine_overrides
                .insert(language.to_string(), EngineState::Inactive);
            self.engine_errors.remove(language);
            self.remove_language_clients(language);
            self.clear_language(language);
            return Ok(None);
        }
        let adapter = self
            .adapters
            .iter()
            .find(|adapter| adapter.language == language)
            .cloned()
            .ok_or_else(|| LspError::Config {
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
            return Err(LspError::Config { message });
        }

        self.engine_overrides
            .insert(language.to_string(), EngineState::Refreshing);
        let epoch = self.next_refresh_epoch(language);
        let project_root = self.project_root.clone();
        let mut documents_by_root: BTreeMap<PathBuf, Vec<LspDocument>> = BTreeMap::new();
        for document in documents {
            let workspace_root =
                adapter_workspace_root(&self.project_root, &adapter, &document.relative_path)
                    .unwrap_or_else(|| self.project_root.clone());
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
                    .entry(session_key)
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
            epoch,
            batches,
        }))
    }

    pub async fn refresh_documents(
        &mut self,
        language: &str,
        documents: Vec<LspDocument>,
        diagnostics_quiet_timeout: Duration,
    ) -> Result<()> {
        self.refresh_documents_with_timeouts(
            language,
            documents,
            LspRefreshTimeouts::from_diagnostics_quiet_window(diagnostics_quiet_timeout),
        )
        .await
    }

    pub async fn refresh_documents_with_timeouts(
        &mut self,
        language: &str,
        documents: Vec<LspDocument>,
        timeouts: LspRefreshTimeouts,
    ) -> Result<()> {
        let Some(prepared) = self.prepare_refresh(language, documents)? else {
            return Ok(());
        };
        let result = prepared.collect_diagnostics_with_timeouts(timeouts).await;
        self.finish_refresh(result)
    }

    pub fn finish_refresh(&mut self, completed: CompletedRefresh) -> Result<()> {
        let language = completed.language;
        if self
            .refresh_epochs
            .get(&language)
            .is_some_and(|current| completed.epoch < *current)
        {
            return Ok(());
        }
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
            Err(failure) => {
                let message = failure.message;
                self.engine_errors.insert(language.clone(), message.clone());
                self.engine_overrides
                    .insert(language.clone(), failure.state);
                self.remove_language_clients(&language);
                Err(LspError::Config { message })
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

    /// Fills `enclosing_node` for cached diagnostics that don't yet have it,
    /// calling `fetch_spans` once per file to load its indexed symbols.
    /// Diagnostics whose file has no covering span leave `enclosing_node` as
    /// `None`. The broker holds no code-graph handle of its own, so callers
    /// inject the lookup (the dashboard backs it with its graph connection).
    pub async fn resolve_enclosing_nodes<F, Fut>(&mut self, mut fetch_spans: F)
    where
        F: FnMut(String) -> Fut,
        Fut: std::future::Future<Output = Vec<NodeSpan>>,
    {
        let mut spans_by_file: BTreeMap<String, Vec<NodeSpan>> = BTreeMap::new();
        for diagnostic in &mut self.diagnostics {
            if diagnostic.enclosing_node.is_some() {
                continue;
            }
            if !spans_by_file.contains_key(&diagnostic.file) {
                let fetched = fetch_spans(diagnostic.file.clone()).await;
                spans_by_file.insert(diagnostic.file.clone(), fetched);
            }
            if let Some(spans) = spans_by_file.get(&diagnostic.file) {
                diagnostic.enclosing_node = enclosing_node_for_line(spans, diagnostic.line_start);
            }
        }
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

    fn next_refresh_epoch(&mut self, language: &str) -> u64 {
        let next = self
            .refresh_epochs
            .get(language)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.refresh_epochs.insert(language.to_string(), next);
        next
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
                    .unwrap_or_else(|| {
                        default_state(
                            enabled,
                            self.project_languages.contains(&adapter.language),
                            &command,
                        )
                    });
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

fn default_state(enabled: bool, active: bool, command: &str) -> EngineState {
    if !enabled {
        return EngineState::Disabled;
    }
    if !active {
        return EngineState::Inactive;
    }
    if command_available(command) {
        EngineState::Available
    } else {
        EngineState::Unavailable
    }
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
