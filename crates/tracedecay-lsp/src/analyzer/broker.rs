use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as SyncMutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use url::Url;

use super::activity::{
    adapter_workspace_root, adapter_workspace_root_from_canonical_root, canonicalize_project_root,
};
use super::adapters::{LspAdapterDefinition, LspInstallOption};
use super::client::{
    LspDocument, LspRefreshTimeouts, LspSemanticRequestError, StdioLspClient,
    decode_semantic_request,
};
use super::error::{
    AnalyzerCancellation as CancellationToken, AnalyzerResult as Result,
    AnalyzerRuntimeError as TraceDecayError,
};
use super::settings::CodeDiagnosticsSettings;
use crate::{
    AdmittedRoot, AnalyzerEvent, AnalyzerState, AnalyzerSupervisor, LspRequestId, LspRuntimeFuture,
    LspSemanticOperationOutcome, LspSemanticRequestAuthority,
};

mod refresh;

use refresh::RefreshBatch;
pub use refresh::{
    CompletedRefresh, MAX_ANALYZER_CONCURRENT_ROOT_FANOUTS, MAX_ANALYZER_QUEUED_ROOT_BATCHES,
    PreparedRefresh,
};

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

/// One project-active diagnostic provider whose configured command is mounted.
///
/// This is the production registration authority: callers must not advertise
/// adapters that are disabled, absent from the project, or unavailable on the
/// current host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedLspProvider {
    pub language: String,
    pub command: String,
}

/// One enabled provider admitted by project language activity.
///
/// Analyzer absence is state, not failed admission: graph-backed semantic and
/// managed diagnostic owners remain mountable when `analyzer_available` is
/// false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedLspProvider {
    pub language: String,
    pub command: String,
    pub analyzer_available: bool,
}

impl AdmittedLspProvider {
    pub fn mounted(&self) -> Option<MountedLspProvider> {
        self.analyzer_available.then(|| MountedLspProvider {
            language: self.language.clone(),
            command: self.command.clone(),
        })
    }
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

/// Why a broker holds default analyzer settings instead of the project's
/// persisted ones.
///
/// A project that never configured settings is not degraded — `load_settings`
/// returns the defaults as `Ok` for an absent file. This is set only when a
/// settings file exists and could not be read or parsed, in which case every
/// custom analyzer the user configured is missing from this broker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsUnavailable {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsSnapshot {
    pub summary: DiagnosticsSummary,
    pub engines: Vec<EngineStatus>,
    pub diagnostics: Vec<CodeDiagnostic>,
    pub backfill: BTreeMap<String, BackfillProgress>,
    pub settings: CodeDiagnosticsSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_unavailable: Option<SettingsUnavailable>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LspSessionKey {
    language: String,
    command: String,
    workspace_root: PathBuf,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SemanticOperationKey {
    root_uri: String,
    request_id: LspRequestId,
}

struct StdioLspSemanticAuthorityInner {
    command: String,
    args: Vec<String>,
    project_root: PathBuf,
    root_uri: String,
    timeouts: LspRefreshTimeouts,
    client: Arc<Mutex<Option<StdioLspClient>>>,
    operations: Mutex<BTreeMap<SemanticOperationKey, CancellationToken>>,
    supervisor: SyncMutex<AnalyzerSupervisor>,
}

/// Retained analyzer authority sharing the broker's stdio client slot.
///
/// Queued operations race lock acquisition against their cancellation token;
/// in-flight operations delegate cancellation to `StdioLspClient`, which
/// writes the standard `$/cancelRequest` notification.
#[derive(Clone)]
pub struct StdioLspSemanticAuthority {
    inner: Arc<StdioLspSemanticAuthorityInner>,
}

impl StdioLspSemanticAuthority {
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        project_root: PathBuf,
        root_uri: impl Into<String>,
        timeouts: LspRefreshTimeouts,
    ) -> Arc<Self> {
        Self::from_shared_client(
            command,
            args,
            project_root,
            root_uri,
            timeouts,
            Arc::new(Mutex::new(None)),
        )
    }

    fn from_shared_client(
        command: impl Into<String>,
        args: Vec<String>,
        project_root: PathBuf,
        root_uri: impl Into<String>,
        timeouts: LspRefreshTimeouts,
        client: Arc<Mutex<Option<StdioLspClient>>>,
    ) -> Arc<Self> {
        let root_uri = root_uri.into();
        Arc::new(Self {
            inner: Arc::new(StdioLspSemanticAuthorityInner {
                command: command.into(),
                args,
                project_root,
                root_uri: root_uri.clone(),
                timeouts,
                client,
                operations: Mutex::new(BTreeMap::new()),
                supervisor: SyncMutex::new(AnalyzerSupervisor::new(AdmittedRoot::new(root_uri))),
            }),
        })
    }

    /// Atomic project-scoped lifecycle evidence for doctor, dashboard, and
    /// other non-LSP callers. The snapshot contains no process or stderr data.
    pub fn analyzer_readiness(&self) -> AnalyzerSupervisor {
        self.inner
            .supervisor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn terminal_outcome(&self) -> Option<LspSemanticOperationOutcome> {
        analyzer_terminal_outcome(&self.inner)
    }
}

impl LspSemanticRequestAuthority for StdioLspSemanticAuthority {
    fn start(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: crate::LspSemanticRequest,
    ) -> LspRuntimeFuture<LspSemanticOperationOutcome> {
        let request = match decode_semantic_request(request) {
            Ok(request) => request,
            Err(error) => {
                return Box::pin(async move { analyzer_event_outcome(error.analyzer_event()) });
            }
        };
        if root.uri() != self.inner.root_uri {
            return Box::pin(async { LspSemanticOperationOutcome::Unavailable });
        }
        if let Some(outcome) = self.terminal_outcome() {
            return Box::pin(async move { outcome });
        }
        let key = SemanticOperationKey {
            root_uri: root.uri().to_owned(),
            request_id,
        };
        let cancellation = CancellationToken::new();
        let inserted = match self.inner.operations.try_lock() {
            Ok(mut operations) => {
                if operations.contains_key(&key) {
                    false
                } else {
                    operations.insert(key.clone(), cancellation.clone());
                    true
                }
            }
            Err(_) => {
                return Box::pin(async {
                    LspSemanticOperationOutcome::Partial {
                        value: serde_json::Value::Null,
                        coverage: "semantic-runtime-busy".to_owned(),
                        detail: None,
                    }
                });
            }
        };
        if !inserted {
            return Box::pin(async {
                LspSemanticOperationOutcome::Partial {
                    value: serde_json::Value::Null,
                    coverage: "semantic-duplicate-operation".to_owned(),
                    detail: None,
                }
            });
        }

        let inner = Arc::clone(&self.inner);
        let analyzer_root = root;
        Box::pin(async move {
            let outcome = tokio::select! {
                () = cancellation.cancelled() => {
                    LspSemanticOperationOutcome::Partial {
                        value: serde_json::Value::Null,
                        coverage: "semantic-cancelled".to_owned(),
                        detail: None,
                    }
                }
                slot = inner.client.lock() => {
                    let mut slot = slot;
                    if slot.is_none()
                        && let Some(outcome) = analyzer_terminal_outcome(&inner)
                    {
                        inner.operations.lock().await.remove(&key);
                        return outcome;
                    }
                    let client = if let Some(client) = slot.take() {
                        mark_analyzer_ready(&inner, &analyzer_root);
                        Ok(Some(client))
                    } else {
                        begin_analyzer_start(&inner, &analyzer_root);
                        tokio::select! {
                            () = cancellation.cancelled() => Ok(None),
                            client = StdioLspClient::start_with_timeouts(
                                &inner.command,
                                &inner.args,
                                &inner.project_root,
                                inner.timeouts,
                            ) => client.map(Some),
                        }
                    };
                    match client {
                        Ok(Some(mut client)) => {
                            mark_analyzer_ready(&inner, &analyzer_root);
                            let result = client
                                .semantic_request(request, &cancellation, inner.timeouts)
                                .await;
                            // Cancellation drops `read_message_until` wherever
                            // it was suspended, so any bytes it had already
                            // consumed into its local header/body buffers are
                            // gone and the stream may be parked mid-frame.
                            // Reusing the client would make every later request
                            // parse from the middle of a message, so retire it
                            // alongside the transport failures.
                            if !matches!(
                                &result,
                                Err(LspSemanticRequestError::Transport { .. }
                                    | LspSemanticRequestError::InvalidResponse { .. }
                                    | LspSemanticRequestError::Cancelled)
                            ) {
                                *slot = Some(client);
                            }
                            match &result {
                                Ok(_)
                                | Err(LspSemanticRequestError::Remote {
                                    code: Some(-32601),
                                    ..
                                }) => record_analyzer_event(
                                    &inner,
                                    &analyzer_root,
                                    AnalyzerEvent::Ready,
                                ),
                                Err(error) => record_analyzer_event(
                                    &inner,
                                    &analyzer_root,
                                    error.analyzer_event(),
                                ),
                            }
                            semantic_operation_outcome(result)
                        }
                        Ok(None) => {
                            record_analyzer_event(
                                &inner,
                                &analyzer_root,
                                AnalyzerEvent::Cancelled,
                            );
                            analyzer_event_outcome(AnalyzerEvent::Cancelled)
                        }
                        Err(error) => {
                            // Coverage is a stable token vocabulary that callers
                            // match on, so the analyzer's own message cannot live
                            // in it: slugifying stripped the punctuation and cut it
                            // mid-word, and a message happening to contain "stale"
                            // steered rename candidates down the wrong branch.
                            // Callers receive only a static typed template; the
                            // daemon-local event keeps the full operational error.
                            record_analyzer_event(
                                &inner,
                                &analyzer_root,
                                AnalyzerEvent::StartupFailed,
                            );
                            analyzer_start_failure(&error)
                        }
                    }
                }
            };
            inner.operations.lock().await.remove(&key);
            outcome
        })
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        let key = SemanticOperationKey {
            root_uri: root.uri().to_owned(),
            request_id: request_id.clone(),
        };
        self.inner
            .operations
            .try_lock()
            .ok()
            .and_then(|operations| operations.get(&key).cloned())
            .is_some_and(|cancellation| {
                cancellation.cancel();
                true
            })
    }
}

fn begin_analyzer_start(inner: &StdioLspSemanticAuthorityInner, root: &AdmittedRoot) {
    let mut supervisor = inner
        .supervisor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if supervisor.state() == AnalyzerState::Ready {
        let _ = supervisor.apply(root, AnalyzerEvent::Crashed);
    }
    if matches!(
        supervisor.state(),
        AnalyzerState::AwaitingStart | AnalyzerState::RestartBackoff
    ) {
        let _ = supervisor.apply(root, AnalyzerEvent::StartRequested);
    }
}

fn analyzer_terminal_outcome(
    inner: &StdioLspSemanticAuthorityInner,
) -> Option<LspSemanticOperationOutcome> {
    let supervisor = inner
        .supervisor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match supervisor.state() {
        AnalyzerState::Exhausted | AnalyzerState::Unavailable => {
            Some(LspSemanticOperationOutcome::Unavailable)
        }
        AnalyzerState::AwaitingStart
        | AnalyzerState::Starting
        | AnalyzerState::Ready
        | AnalyzerState::RestartBackoff => None,
    }
}

fn mark_analyzer_ready(inner: &StdioLspSemanticAuthorityInner, root: &AdmittedRoot) {
    let mut supervisor = inner
        .supervisor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if matches!(
        supervisor.state(),
        AnalyzerState::AwaitingStart | AnalyzerState::RestartBackoff
    ) {
        let _ = supervisor.apply(root, AnalyzerEvent::StartRequested);
    }
    if supervisor.state() == AnalyzerState::Starting {
        let _ = supervisor.apply(root, AnalyzerEvent::Ready);
    }
}

fn record_analyzer_event(
    inner: &StdioLspSemanticAuthorityInner,
    root: &AdmittedRoot,
    event: AnalyzerEvent,
) {
    let mut supervisor = inner
        .supervisor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = supervisor.apply(root, event);
}

fn analyzer_event_outcome(event: AnalyzerEvent) -> LspSemanticOperationOutcome {
    let Some(coverage) = event.coverage_token() else {
        return LspSemanticOperationOutcome::Unavailable;
    };
    LspSemanticOperationOutcome::Partial {
        value: serde_json::Value::Null,
        coverage: coverage.to_owned(),
        detail: event.failure_detail(),
    }
}

fn analyzer_start_failure(error: &TraceDecayError) -> LspSemanticOperationOutcome {
    eprintln!("[tracedecay] event=analyzer_start_failed error={error}");
    analyzer_event_outcome(AnalyzerEvent::StartupFailed)
}

fn semantic_operation_outcome(
    result: std::result::Result<serde_json::Value, LspSemanticRequestError>,
) -> LspSemanticOperationOutcome {
    match result {
        Ok(value) => LspSemanticOperationOutcome::Complete(value),
        Err(LspSemanticRequestError::Remote {
            code: Some(-32601), ..
        }) => LspSemanticOperationOutcome::Unavailable,
        Err(error) => {
            eprintln!("[tracedecay] event=analyzer_semantic_request_failed error={error}");
            analyzer_event_outcome(error.analyzer_event())
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
    settings_unavailable: Option<SettingsUnavailable>,
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
            settings_unavailable: None,
        }
    }

    /// Records that the project's persisted settings could not be loaded, so
    /// every read of this broker reports the degradation instead of passing
    /// the fallback defaults off as the user's configuration.
    pub fn record_settings_unavailable(&mut self, reason: impl Into<String>) {
        self.settings_unavailable = Some(SettingsUnavailable {
            reason: reason.into(),
        });
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

    pub fn settings(&self) -> &CodeDiagnosticsSettings {
        &self.settings
    }

    pub fn snapshot(&self) -> DiagnosticsSnapshot {
        DiagnosticsSnapshot {
            summary: self.summary(),
            engines: self.engine_statuses(),
            diagnostics: self.diagnostics.clone(),
            backfill: self.backfill.clone(),
            settings: self.settings.clone(),
            settings_unavailable: self.settings_unavailable.clone(),
        }
    }

    /// Current statuses for only the languages active in this project.
    ///
    /// Doctor consumes this live owner view so disabled adapters for languages
    /// absent from the project cannot be misreported as project degradation.
    pub fn project_engine_statuses(&self) -> Vec<EngineStatus> {
        self.engine_statuses()
            .into_iter()
            .filter(|status| self.project_languages.contains(&status.language))
            .collect()
    }

    pub fn adapter_for(&self, language: &str) -> Option<LspAdapterDefinition> {
        self.adapters
            .iter()
            .find(|adapter| adapter.language == language)
            .cloned()
    }

    /// Returns a retained semantic authority over the same stdio client slot
    /// used by diagnostic refreshes, or `None` when the executable is absent.
    pub fn semantic_authority_if_available(
        &mut self,
        language: &str,
        workspace_root: PathBuf,
        root_uri: impl Into<String>,
        timeouts: LspRefreshTimeouts,
    ) -> Result<Option<Arc<StdioLspSemanticAuthority>>> {
        let root_uri = root_uri.into();
        self.validate_semantic_scope(&workspace_root, &root_uri)?;
        let adapter = self
            .adapter_for(language)
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("no LSP adapter registered for language '{language}'"),
            })?;
        let command = self.settings.command_for(language, &adapter.command);
        if !command_available(&command) {
            return Ok(None);
        }
        let key = LspSessionKey {
            language: language.to_owned(),
            command: command.clone(),
            workspace_root: workspace_root.clone(),
        };
        let client = self
            .clients
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone();
        Ok(Some(StdioLspSemanticAuthority::from_shared_client(
            command,
            adapter.args,
            workspace_root,
            root_uri,
            timeouts,
            client,
        )))
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

    /// Reconciles enabled project language activity without requiring the
    /// external analyzer executable to be installed.
    pub fn admitted_providers_for_files(&mut self, files: &[String]) -> Vec<AdmittedLspProvider> {
        let languages =
            super::activity::active_languages_for_files(&self.project_root, &self.adapters, files);
        self.update_project_languages(languages);
        self.adapters
            .iter()
            .filter(|adapter| {
                self.project_languages.contains(&adapter.language)
                    && self.settings.language_enabled(&adapter.language)
            })
            .map(|adapter| {
                let command = self
                    .settings
                    .command_for(&adapter.language, &adapter.command);
                AdmittedLspProvider {
                    language: adapter.language.clone(),
                    analyzer_available: command_available(&command),
                    command,
                }
            })
            .collect()
    }

    /// Returns only analyzer-backed providers that are executable now.
    pub fn mounted_providers_for_files(&mut self, files: &[String]) -> Vec<MountedLspProvider> {
        self.admitted_providers_for_files(files)
            .iter()
            .filter_map(AdmittedLspProvider::mounted)
            .collect()
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
        let epoch = self.next_refresh_epoch(language);
        let project_root = self.project_root.clone();
        let canonical_project_root = canonicalize_project_root(&project_root).ok();
        let mut documents_by_root: BTreeMap<PathBuf, Vec<LspDocument>> = BTreeMap::new();
        for document in documents {
            let workspace_root = match canonical_project_root.as_deref() {
                Some(root) => adapter_workspace_root_from_canonical_root(
                    root,
                    &adapter,
                    &document.relative_path,
                ),
                None => {
                    adapter_workspace_root(&self.project_root, &adapter, &document.relative_path)
                }
            }
            .unwrap_or_else(|| self.project_root.clone());
            documents_by_root
                .entry(workspace_root)
                .or_default()
                .push(document);
        }
        if documents_by_root.len() > MAX_ANALYZER_QUEUED_ROOT_BATCHES {
            let message = format!(
                "analyzer root queue saturated: {} batches exceed the {MAX_ANALYZER_QUEUED_ROOT_BATCHES} limit",
                documents_by_root.len()
            );
            self.engine_errors
                .insert(language.to_string(), message.clone());
            self.engine_overrides
                .insert(language.to_string(), EngineState::Unavailable);
            return Err(TraceDecayError::Config { message });
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
        Ok(Some(PreparedRefresh::new(
            language.to_string(),
            project_root,
            command,
            adapter.args,
            epoch,
            batches,
        )))
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
                Err(TraceDecayError::Config { message })
            }
        }
    }

    pub fn update_settings(&mut self, settings: CodeDiagnosticsSettings) {
        self.settings = settings;
        // These settings were just persisted, so whatever the broker could not
        // read at mount time is no longer what it is serving.
        self.settings_unavailable = None;
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

    fn validate_semantic_scope(&self, workspace_root: &Path, root_uri: &str) -> Result<()> {
        let project_root =
            self.project_root
                .canonicalize()
                .map_err(|error| TraceDecayError::Config {
                    message: format!("failed to resolve admitted project root: {error}"),
                })?;
        let workspace_root =
            workspace_root
                .canonicalize()
                .map_err(|error| TraceDecayError::Config {
                    message: format!("failed to resolve analyzer workspace root: {error}"),
                })?;
        if !workspace_root.starts_with(&project_root) {
            return Err(TraceDecayError::Config {
                message: "analyzer workspace is outside the admitted project root".to_owned(),
            });
        }
        let admitted_root = Url::parse(root_uri)
            .ok()
            .filter(|uri| uri.scheme() == "file")
            .and_then(|uri| uri.to_file_path().ok())
            .and_then(|path| path.canonicalize().ok());
        if admitted_root.as_deref() != Some(project_root.as_path()) {
            return Err(TraceDecayError::Config {
                message: "analyzer root URI does not match the admitted project root".to_owned(),
            });
        }
        Ok(())
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

    let pathext = std::env::var_os("PATHEXT").map_or_else(
        || ".COM;.EXE;.BAT;.CMD".to_string(),
        |value| value.to_string_lossy().into_owned(),
    );

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::activity::{
        project_root_canonicalization_count, reset_project_root_canonicalization_count,
    };
    use crate::analyzer::adapters::DiagnosticMode;

    fn assert_safe_partial_detail(
        outcome: LspSemanticOperationOutcome,
        expected_coverage: &str,
        expected_detail: &str,
    ) {
        let LspSemanticOperationOutcome::Partial {
            coverage, detail, ..
        } = outcome
        else {
            panic!("expected partial semantic outcome");
        };
        assert_eq!(coverage, expected_coverage);
        assert_eq!(detail, Some(expected_detail));
        for forbidden in [
            "bearer-secret",
            "YWxpY2U6c2VjcmV0",
            "alice:hunter2",
            "bob:password",
            "/home/alice",
            r"C:\Users\alice",
            "密碼",
            "🔐",
        ] {
            assert!(
                !detail
                    .expect("typed analyzer failure detail")
                    .contains(forbidden),
                "caller detail leaked {forbidden}"
            );
        }
    }

    #[test]
    fn analyzer_failure_details_never_copy_raw_errors() {
        let sensitive = concat!(
            "stderr first line\n",
            "Authorization: Bearer bearer-secret\n",
            "Authorization: Basic YWxpY2U6c2VjcmV0\n",
            "https://alice:hunter2@example.test/private\n",
            "file://bob:password@localhost/home/bob/private.rs\n",
            "/home/alice/.ssh/id_rsa\n",
            r"C:\Users\alice\AppData\secret.txt",
            "\nUTF-8: 密碼 🔐"
        );

        assert_safe_partial_detail(
            analyzer_start_failure(&TraceDecayError::Config {
                message: sensitive.to_owned(),
            }),
            "analyzer-start-failed",
            LspSemanticOperationOutcome::ANALYZER_START_FAILED_DETAIL,
        );
        assert_safe_partial_detail(
            semantic_operation_outcome(Err(LspSemanticRequestError::Remote {
                code: Some(-32603),
                message: sensitive.to_owned(),
            })),
            "analyzer-remote-error",
            LspSemanticOperationOutcome::ANALYZER_REMOTE_ERROR_DETAIL,
        );
        assert_safe_partial_detail(
            semantic_operation_outcome(Err(LspSemanticRequestError::Transport {
                class: sensitive.to_owned(),
            })),
            "analyzer-transport-failed",
            LspSemanticOperationOutcome::ANALYZER_TRANSPORT_FAILED_DETAIL,
        );
        assert_safe_partial_detail(
            semantic_operation_outcome(Err(LspSemanticRequestError::InvalidResponse {
                class: sensitive.to_owned(),
            })),
            "analyzer-invalid-response",
            LspSemanticOperationOutcome::ANALYZER_INVALID_RESPONSE_DETAIL,
        );
        assert_safe_partial_detail(
            semantic_operation_outcome(Err(LspSemanticRequestError::TimedOut)),
            "analyzer-timeout",
            LspSemanticOperationOutcome::ANALYZER_TIMEOUT_DETAIL,
        );
        assert_safe_partial_detail(
            semantic_operation_outcome(Err(LspSemanticRequestError::Cancelled)),
            "analyzer-cancelled",
            LspSemanticOperationOutcome::ANALYZER_CANCELLED_DETAIL,
        );
    }

    #[test]
    fn semantic_remote_method_missing_remains_unavailable() {
        assert_eq!(
            semantic_operation_outcome(Err(LspSemanticRequestError::Remote {
                code: Some(-32601),
                message: "method not found: stale Bearer secret /private/path?!".to_owned(),
            })),
            LspSemanticOperationOutcome::Unavailable
        );
    }

    fn adapter(
        language: &str,
        command: impl Into<String>,
        extension: &str,
        root_marker: &str,
    ) -> LspAdapterDefinition {
        LspAdapterDefinition {
            language: language.to_owned(),
            language_id: language.to_owned(),
            command: command.into(),
            args: Vec::new(),
            extensions: vec![extension.to_owned()],
            root_markers: vec![root_marker.to_owned()],
            install_options: Vec::new(),
            diagnostics: DiagnosticMode::Push,
        }
    }

    #[test]
    fn admitted_providers_derive_python_and_typescript_from_project_files() {
        let project = tempfile::tempdir().expect("project");
        std::fs::write(project.path().join("pyproject.toml"), "").expect("python root marker");
        std::fs::write(project.path().join("tsconfig.json"), "").expect("typescript root marker");
        let python = project.path().join("pyright-langserver");
        std::fs::write(&python, "").expect("mounted python provider");
        let mut broker = DiagnosticBroker::new(
            project.path(),
            vec![
                adapter(
                    "typescript",
                    project
                        .path()
                        .join("missing-typescript-language-server")
                        .to_string_lossy(),
                    "ts",
                    "tsconfig.json",
                ),
                adapter("python", python.to_string_lossy(), "py", "pyproject.toml"),
            ],
            CodeDiagnosticsSettings::default(),
        );

        let admitted = broker
            .admitted_providers_for_files(&["src/main.ts".to_owned(), "src/main.py".to_owned()]);

        assert_eq!(
            admitted,
            vec![
                AdmittedLspProvider {
                    language: "typescript".to_owned(),
                    command: project
                        .path()
                        .join("missing-typescript-language-server")
                        .to_string_lossy()
                        .into_owned(),
                    analyzer_available: false,
                },
                AdmittedLspProvider {
                    language: "python".to_owned(),
                    command: python.to_string_lossy().into_owned(),
                    analyzer_available: true,
                },
            ]
        );
    }

    #[test]
    fn absent_analyzer_keeps_an_admitted_graph_fallback_provider() {
        let project = tempfile::tempdir().expect("project");
        std::fs::write(project.path().join("Cargo.toml"), "").expect("rust root marker");
        let missing = project.path().join("missing-rust-analyzer");
        let mut broker = DiagnosticBroker::new(
            project.path(),
            vec![adapter(
                "rust",
                missing.to_string_lossy(),
                "rs",
                "Cargo.toml",
            )],
            CodeDiagnosticsSettings::default(),
        );

        assert_eq!(
            broker.admitted_providers_for_files(&["src/lib.rs".to_owned()]),
            vec![AdmittedLspProvider {
                language: "rust".to_owned(),
                command: missing.to_string_lossy().into_owned(),
                analyzer_available: false,
            }]
        );
        assert!(
            broker
                .semantic_authority_if_available(
                    "rust",
                    project.path().to_path_buf(),
                    url::Url::from_directory_path(project.path())
                        .expect("project root URI")
                        .to_string(),
                    LspRefreshTimeouts::from_diagnostics_quiet_window(
                        std::time::Duration::from_millis(10),
                    ),
                )
                .expect("configured adapter")
                .is_none()
        );
        assert!(
            broker
                .mounted_providers_for_files(&["src/lib.rs".to_owned()])
                .is_empty()
        );
    }

    #[test]
    fn refresh_batch_canonicalizes_the_project_root_once() {
        let project = tempfile::tempdir().expect("project");
        std::fs::create_dir(project.path().join("src")).expect("source directory");
        std::fs::write(project.path().join("Cargo.toml"), "").expect("root marker");
        let command = project.path().join("analyzer");
        std::fs::write(&command, "").expect("analyzer command");
        let mut broker = DiagnosticBroker::new_for_test(
            project.path(),
            vec![adapter(
                "rust",
                command.to_string_lossy(),
                "rs",
                "Cargo.toml",
            )],
        );
        reset_project_root_canonicalization_count();

        let prepared = broker
            .prepare_refresh(
                "rust",
                vec![
                    LspDocument {
                        language: "rust".to_owned(),
                        language_id: "rust".to_owned(),
                        relative_path: "src/one.rs".to_owned(),
                        text: "fn one() {}".to_owned(),
                    },
                    LspDocument {
                        language: "rust".to_owned(),
                        language_id: "rust".to_owned(),
                        relative_path: "src/two.rs".to_owned(),
                        text: "fn two() {}".to_owned(),
                    },
                ],
            )
            .expect("refresh preparation");

        assert!(prepared.is_some());
        assert_eq!(project_root_canonicalization_count(), 1);
    }

    #[test]
    fn refresh_rejects_root_batch_queue_saturation_before_starting_analyzers() {
        let project = tempfile::tempdir().expect("project");
        let command = project.path().join("analyzer");
        std::fs::write(&command, "").expect("analyzer command");
        let mut documents = Vec::with_capacity(MAX_ANALYZER_QUEUED_ROOT_BATCHES + 1);
        for index in 0..=MAX_ANALYZER_QUEUED_ROOT_BATCHES {
            let package = project.path().join(format!("package-{index}"));
            std::fs::create_dir_all(package.join("src")).expect("package source directory");
            std::fs::write(package.join("marker"), "").expect("package root marker");
            documents.push(LspDocument {
                language: "rust".to_owned(),
                language_id: "rust".to_owned(),
                relative_path: format!("package-{index}/src/lib.rs"),
                text: "fn package() {}".to_owned(),
            });
        }
        let mut broker = DiagnosticBroker::new_for_test(
            project.path(),
            vec![adapter("rust", command.to_string_lossy(), "rs", "marker")],
        );

        let error = match broker.prepare_refresh("rust", documents) {
            Err(error) => error,
            Ok(_) => panic!("queue saturation must reject before analyzer startup"),
        };

        assert!(error.to_string().contains("analyzer root queue saturated"));
        assert_eq!(broker.snapshot().engines[0].state, EngineState::Unavailable);
    }
}
