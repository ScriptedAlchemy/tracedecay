use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
    DocumentSymbolRequest, GotoDeclaration, GotoDeclarationParams, GotoDefinition,
    GotoImplementation, GotoImplementationParams, GotoTypeDefinition, GotoTypeDefinitionParams,
    HoverRequest, PrepareRenameRequest, References, Request as LspRequest, SignatureHelpRequest,
    TypeHierarchyPrepare, TypeHierarchySubtypes, TypeHierarchySupertypes, WorkspaceSymbolRequest,
};
use lsp_types::{
    CallHierarchyIncomingCallsParams, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    Diagnostic as StandardDiagnostic, DiagnosticSeverity as StandardDiagnosticSeverity,
    DocumentSymbolParams, GotoDefinitionParams, HoverParams, NumberOrString,
    PublishDiagnosticsParams, ReferenceParams, SignatureHelpParams, TextDocumentPositionParams,
    TypeHierarchyPrepareParams, TypeHierarchySubtypesParams, TypeHierarchySupertypesParams,
    WorkspaceSymbolParams,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_daemon_protocol::{ConnectionLocalRequestSequence, FramePoll};
use tracedecay_domain::ContentDigest;
use tracedecay_runtime_core::path_safety::plain_host_path;

use super::broker::{CodeDiagnostic, DiagnosticSeverity};
use super::error::{
    AnalyzerCancellation as CancellationToken, AnalyzerResult as Result,
    AnalyzerRuntimeError as TraceDecayError,
};
use super::launch::{AnalyzerLaunch, resolve_analyzer_launch};
use super::process::AnalyzerProcess;

use crate::{AnalyzerEvent, AsyncContentLengthError, UpstreamCapabilities};

const MIN_MESSAGE_IO_TIMEOUT: Duration = Duration::from_secs(2);
const MIN_INITIALIZE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspRefreshTimeouts {
    refresh: Duration,
    initialize_response: Duration,
    message_io: Duration,
    diagnostics_quiet: Duration,
    deadline: Option<Instant>,
}

impl LspRefreshTimeouts {
    pub fn new(
        refresh: Duration,
        initialize_response: Duration,
        message_io: Duration,
        diagnostics_quiet: Duration,
    ) -> Self {
        Self {
            refresh,
            initialize_response,
            message_io,
            diagnostics_quiet,
            deadline: None,
        }
    }

    pub(super) fn operation_deadline(self) -> Instant {
        self.deadline.unwrap_or_else(|| {
            Instant::now() + self.initialize_response.saturating_add(self.refresh)
        })
    }

    fn request_deadline(self) -> Instant {
        self.deadline
            .unwrap_or_else(|| Instant::now() + self.refresh)
    }

    pub(super) fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(self.operation_deadline().min(deadline));
        self
    }

    pub fn from_diagnostics_quiet_window(diagnostics_quiet: Duration) -> Self {
        let message_io = diagnostics_quiet.max(MIN_MESSAGE_IO_TIMEOUT);
        let initialize_response = message_io.max(MIN_INITIALIZE_RESPONSE_TIMEOUT);
        let refresh = diagnostics_quiet.saturating_add(message_io);
        Self {
            refresh,
            initialize_response,
            message_io,
            diagnostics_quiet,
            deadline: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDocument {
    pub language: String,
    pub language_id: String,
    pub relative_path: String,
    pub text: String,
    /// The editor version from the admitted overlay, when this is not a disk sweep.
    pub snapshot_version: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DiagnosticDocumentCoverage {
    Published {
        version_verified: bool,
        content_digest: ContentDigest,
    },
    NoPublication,
    StalePublication,
    RootFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticRootCoverage {
    Completed,
    Failed,
}

/// Coverage of the latest admitted refresh scope. A missing document was not
/// observed by that refresh; any retained findings for it remain unverified.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticCoverage {
    pub documents: BTreeMap<String, DiagnosticDocumentCoverage>,
    pub roots: BTreeMap<PathBuf, DiagnosticRootCoverage>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticRefreshResult {
    pub diagnostics: Vec<CodeDiagnostic>,
    pub coverage: DiagnosticCoverage,
}

/// Standard LSP semantic/navigation requests retained by the analyzer client.
///
/// Each variant carries the `lsp-types` request DTO for its standard method;
/// this boundary never invents an analyzer-specific wire shape.
#[derive(Clone, Debug)]
pub enum LspSemanticRequest {
    Declaration(GotoDeclarationParams),
    Definition(GotoDefinitionParams),
    TypeDefinition(GotoTypeDefinitionParams),
    Implementation(GotoImplementationParams),
    References(ReferenceParams),
    Hover(HoverParams),
    DocumentSymbols(DocumentSymbolParams),
    WorkspaceSymbols(WorkspaceSymbolParams),
    PrepareCallHierarchy(CallHierarchyPrepareParams),
    IncomingCalls(CallHierarchyIncomingCallsParams),
    OutgoingCalls(CallHierarchyOutgoingCallsParams),
    SignatureHelp(SignatureHelpParams),
    PrepareTypeHierarchy(TypeHierarchyPrepareParams),
    TypeHierarchySupertypes(TypeHierarchySupertypesParams),
    TypeHierarchySubtypes(TypeHierarchySubtypesParams),
    PrepareRename(TextDocumentPositionParams),
}

pub fn decode_semantic_request(
    request: crate::LspSemanticRequest,
) -> std::result::Result<LspSemanticRequest, LspSemanticRequestError> {
    let method = request.method();
    let params = request.into_params();
    match method {
        "textDocument/declaration" => decode(params).map(LspSemanticRequest::Declaration),
        "textDocument/definition" => decode(params).map(LspSemanticRequest::Definition),
        "textDocument/typeDefinition" => decode(params).map(LspSemanticRequest::TypeDefinition),
        "textDocument/implementation" => decode(params).map(LspSemanticRequest::Implementation),
        "textDocument/references" => decode(params).map(LspSemanticRequest::References),
        "textDocument/hover" => decode(params).map(LspSemanticRequest::Hover),
        "textDocument/documentSymbol" => decode(params).map(LspSemanticRequest::DocumentSymbols),
        "workspace/symbol" => decode(params).map(LspSemanticRequest::WorkspaceSymbols),
        "textDocument/prepareCallHierarchy" => {
            decode(params).map(LspSemanticRequest::PrepareCallHierarchy)
        }
        "callHierarchy/incomingCalls" => decode(params).map(LspSemanticRequest::IncomingCalls),
        "callHierarchy/outgoingCalls" => decode(params).map(LspSemanticRequest::OutgoingCalls),
        "textDocument/signatureHelp" => decode(params).map(LspSemanticRequest::SignatureHelp),
        "textDocument/prepareTypeHierarchy" => {
            decode(params).map(LspSemanticRequest::PrepareTypeHierarchy)
        }
        "typeHierarchy/supertypes" => {
            decode(params).map(LspSemanticRequest::TypeHierarchySupertypes)
        }
        "typeHierarchy/subtypes" => decode(params).map(LspSemanticRequest::TypeHierarchySubtypes),
        "textDocument/prepareRename" => decode(params).map(LspSemanticRequest::PrepareRename),
        _ => Err(LspSemanticRequestError::InvalidResponse {
            class: "unsupported semantic method".to_owned(),
        }),
    }
}

pub fn encode_semantic_request(
    request: LspSemanticRequest,
) -> std::result::Result<crate::LspSemanticRequest, LspSemanticRequestError> {
    let method = request.method();
    let params = match request {
        LspSemanticRequest::Declaration(params)
        | LspSemanticRequest::Definition(params)
        | LspSemanticRequest::TypeDefinition(params)
        | LspSemanticRequest::Implementation(params) => serde_json::to_value(params),
        LspSemanticRequest::References(params) => serde_json::to_value(params),
        LspSemanticRequest::Hover(params) => serde_json::to_value(params),
        LspSemanticRequest::DocumentSymbols(params) => serde_json::to_value(params),
        LspSemanticRequest::WorkspaceSymbols(params) => serde_json::to_value(params),
        LspSemanticRequest::PrepareCallHierarchy(params) => serde_json::to_value(params),
        LspSemanticRequest::IncomingCalls(params) => serde_json::to_value(params),
        LspSemanticRequest::OutgoingCalls(params) => serde_json::to_value(params),
        LspSemanticRequest::SignatureHelp(params) => serde_json::to_value(params),
        LspSemanticRequest::PrepareTypeHierarchy(params) => serde_json::to_value(params),
        LspSemanticRequest::TypeHierarchySupertypes(params) => serde_json::to_value(params),
        LspSemanticRequest::TypeHierarchySubtypes(params) => serde_json::to_value(params),
        LspSemanticRequest::PrepareRename(params) => serde_json::to_value(params),
    }
    .map_err(|error| LspSemanticRequestError::InvalidResponse {
        class: error.to_string(),
    })?;
    Ok(crate::LspSemanticRequest::from_standard(method, params))
}

fn decode<T: DeserializeOwned>(value: Value) -> std::result::Result<T, LspSemanticRequestError> {
    serde_json::from_value(value).map_err(|error| LspSemanticRequestError::InvalidResponse {
        class: error.to_string(),
    })
}

impl LspSemanticRequest {
    pub fn method(&self) -> &'static str {
        match self {
            Self::Declaration(_) => GotoDeclaration::METHOD,
            Self::Definition(_) => GotoDefinition::METHOD,
            Self::TypeDefinition(_) => GotoTypeDefinition::METHOD,
            Self::Implementation(_) => GotoImplementation::METHOD,
            Self::References(_) => References::METHOD,
            Self::Hover(_) => HoverRequest::METHOD,
            Self::DocumentSymbols(_) => DocumentSymbolRequest::METHOD,
            Self::WorkspaceSymbols(_) => WorkspaceSymbolRequest::METHOD,
            Self::PrepareCallHierarchy(_) => CallHierarchyPrepare::METHOD,
            Self::IncomingCalls(_) => CallHierarchyIncomingCalls::METHOD,
            Self::OutgoingCalls(_) => CallHierarchyOutgoingCalls::METHOD,
            Self::SignatureHelp(_) => SignatureHelpRequest::METHOD,
            Self::PrepareTypeHierarchy(_) => TypeHierarchyPrepare::METHOD,
            Self::TypeHierarchySupertypes(_) => TypeHierarchySupertypes::METHOD,
            Self::TypeHierarchySubtypes(_) => TypeHierarchySubtypes::METHOD,
            Self::PrepareRename(_) => PrepareRenameRequest::METHOD,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LspSemanticRequestError {
    Cancelled,
    TimedOut,
    ProcessExited { status: String },
    Remote { code: Option<i64>, message: String },
    Transport { class: String },
    InvalidResponse { class: String },
}

impl LspSemanticRequestError {
    pub fn analyzer_event(&self) -> AnalyzerEvent {
        match self {
            Self::Cancelled => AnalyzerEvent::Cancelled,
            Self::TimedOut => AnalyzerEvent::TimedOut,
            Self::ProcessExited { .. } => AnalyzerEvent::Crashed,
            Self::Remote { .. } => AnalyzerEvent::RemoteError,
            Self::Transport { .. } => AnalyzerEvent::TransportFailed,
            Self::InvalidResponse { .. } => AnalyzerEvent::InvalidResponse,
        }
    }

    pub fn coverage_token(&self) -> &'static str {
        self.analyzer_event()
            .coverage_token()
            .unwrap_or("analyzer-request-failed")
    }
}

impl std::fmt::Display for LspSemanticRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("analyzer request was cancelled"),
            Self::TimedOut => f.write_str("analyzer request timed out"),
            Self::ProcessExited { status } => {
                write!(f, "analyzer process exited before responding ({status})")
            }
            Self::Remote {
                code: Some(code),
                message,
            } => write!(f, "analyzer returned error {code}: {message}"),
            Self::Remote {
                code: None,
                message,
            } => write!(f, "analyzer returned an error: {message}"),
            Self::Transport { class } => write!(f, "analyzer transport failed: {class}"),
            Self::InvalidResponse { class } => {
                write!(f, "analyzer returned an invalid response: {class}")
            }
        }
    }
}

/// Typed outcome of a diagnostics collection after the analyzer initialized.
///
/// A quiet window with no publication is a broker policy decision, while a
/// closed process, broken transport, or malformed protocol frame is evidence
/// against the analyzer. Keeping that distinction here prevents the shared
/// supervisor from treating every failed refresh as the same lifecycle event.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LspRefreshError {
    #[error("{message}")]
    TimedOut { message: String },
    #[error("{message}")]
    NoPublication { message: String },
    #[error("{message}")]
    ProcessExited { message: String },
    #[error("{message}")]
    Transport { message: String },
    #[error("{message}")]
    InvalidResponse { message: String },
}

impl LspRefreshError {
    pub fn analyzer_event(&self) -> AnalyzerEvent {
        match self {
            Self::TimedOut { .. } => AnalyzerEvent::TimedOut,
            Self::NoPublication { .. } => AnalyzerEvent::Retired,
            Self::ProcessExited { .. } => AnalyzerEvent::Crashed,
            Self::Transport { .. } => AnalyzerEvent::TransportFailed,
            Self::InvalidResponse { .. } => AnalyzerEvent::InvalidResponse,
        }
    }

    pub fn is_policy_retirement(&self) -> bool {
        matches!(self, Self::NoPublication { .. })
    }
}

impl From<LspRefreshError> for TraceDecayError {
    fn from(error: LspRefreshError) -> Self {
        Self::Config {
            message: error.to_string(),
        }
    }
}

pub async fn collect_document_diagnostics(
    command: &str,
    args: &[String],
    project_root: &Path,
    documents: Vec<LspDocument>,
    diagnostics_quiet_timeout: Duration,
) -> Result<DiagnosticRefreshResult> {
    let timeouts = LspRefreshTimeouts::from_diagnostics_quiet_window(diagnostics_quiet_timeout);
    collect_document_diagnostics_with_timeouts(command, args, project_root, documents, timeouts)
        .await
}

pub async fn collect_document_diagnostics_with_timeouts(
    command: &str,
    args: &[String],
    project_root: &Path,
    documents: Vec<LspDocument>,
    timeouts: LspRefreshTimeouts,
) -> Result<DiagnosticRefreshResult> {
    let timeouts = timeouts.with_deadline(timeouts.operation_deadline());
    let mut client =
        StdioLspClient::start_with_timeouts(command, args, project_root, timeouts).await?;
    client
        .collect_document_diagnostics(project_root, documents, timeouts)
        .await
        .map_err(TraceDecayError::from)
}

struct SynchronizedDocument {
    wire_version: i32,
    text: String,
    snapshot_version: Option<i64>,
}

pub struct StdioLspClient {
    command: String,
    workspace_root: PathBuf,
    upstream_capabilities: UpstreamCapabilities,
    documents: BTreeMap<String, SynchronizedDocument>,
    next_request_id: ConnectionLocalRequestSequence,
    /// Killed and reaped when the client drops.
    process: AnalyzerProcess,
}

impl StdioLspClient {
    /// Resolves `command` for `project_root` (see [`resolve_analyzer_launch`])
    /// and starts it. A rustup proxy whose toolchain lacks the analyzer is a
    /// typed configuration failure here; no process is spawned for it.
    pub async fn start_with_timeouts(
        command: &str,
        args: &[String],
        project_root: &Path,
        timeouts: LspRefreshTimeouts,
    ) -> Result<Self> {
        let timeouts = timeouts.with_deadline(timeouts.operation_deadline());
        let launch = tokio::time::timeout_at(
            tokio::time::Instant::from_std(timeouts.operation_deadline()),
            resolve_analyzer_launch(command, project_root),
        )
        .await
        .map_err(|_| TraceDecayError::Config {
            message: "LSP analyzer launch resolution timed out".to_owned(),
        })?
        .map_err(|error| TraceDecayError::Config {
            message: error.engine_error(),
        })?;
        Self::start_with_launch(&launch, args, project_root, timeouts).await
    }

    /// Starts an already resolved launch. Every spawn carries the launch
    /// environment, so neither the analyzer nor a `cargo` it runs can trigger a
    /// rustup toolchain install.
    pub async fn start_with_launch(
        launch: &AnalyzerLaunch,
        args: &[String],
        project_root: &Path,
        timeouts: LspRefreshTimeouts,
    ) -> Result<Self> {
        let timeouts = timeouts.with_deadline(timeouts.operation_deadline());
        let deadline = timeouts.operation_deadline();
        let command = launch.program.to_string_lossy().into_owned();
        let command = command.as_str();
        let mut spawn = std::process::Command::new(&launch.program);
        spawn
            .args(args)
            .envs(launch.env.iter().map(|(key, value)| (key, value)))
            .current_dir(project_root);
        let mut process = AnalyzerProcess::spawn(spawn, command).await?;

        let send_initialize = write_message_until(
            &process,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "processId": null,
                    "rootUri": file_uri(project_root),
                    "initializationOptions": lsp_initialization_options(command),
                    "capabilities": {
                        "textDocument": {
                            "publishDiagnostics": {},
                            "declaration": { "linkSupport": true },
                            "definition": { "linkSupport": true },
                            "typeDefinition": { "linkSupport": true },
                            "implementation": { "linkSupport": true },
                            "references": {},
                            "hover": {
                                "contentFormat": ["markdown", "plaintext"]
                            },
                            "documentSymbol": {
                                "hierarchicalDocumentSymbolSupport": true
                            },
                            "callHierarchy": {},
                            "signatureHelp": {
                                "contextSupport": true
                            },
                            "typeHierarchy": {}
                        },
                        "workspace": {
                            "symbol": {}
                        },
                    },
                    "workspaceFolders": [{
                        "uri": file_uri(project_root),
                        "name": project_root
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("workspace")
                    }]
                }
            }),
            deadline,
            timeouts.message_io,
        )
        .await
        .map_err(LspMessageWriteError::into_trace_decay_error);
        // A server that dies immediately can fail the initialize *request*
        // write (broken pipe, on Windows this races the spawn under load)
        // just as easily as the initialize *response* wait. Route both
        // failures through the same stderr-enriched classification so the
        // crash reason (e.g. a toolchain's "unknown binary" complaint) is
        // never dropped.
        let initialize_result = match send_initialize {
            Ok(()) => {
                wait_for_initialize(
                    &process,
                    deadline.min(Instant::now() + timeouts.initialize_response),
                    command,
                    timeouts.initialize_response,
                    project_root,
                    timeouts,
                )
                .await
            }
            Err(err) => Err(err),
        };
        let initialize_response = match initialize_result {
            Ok(response) => response,
            Err(error) => {
                let _ = process.terminate().await;
                return Err(enrich_start_error(command, error, &process.stderr()));
            }
        };
        write_message_until(
            &process,
            json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "params": {}
            }),
            deadline,
            timeouts.message_io,
        )
        .await
        .map_err(LspMessageWriteError::into_trace_decay_error)?;

        Ok(Self {
            command: command.to_string(),
            workspace_root: project_root.to_path_buf(),
            upstream_capabilities: UpstreamCapabilities::from_initialize_response(
                &initialize_response,
            ),
            documents: BTreeMap::new(),
            next_request_id: ConnectionLocalRequestSequence::starting_at(2),
            process,
        })
    }

    pub(crate) fn upstream_capabilities(&self) -> UpstreamCapabilities {
        self.upstream_capabilities.clone()
    }

    /// Synchronizes a semantic request admitted without an overlay. This
    /// explicitly returns the document to disk authority after an overlay closes.
    pub(crate) async fn ensure_document_open(
        &mut self,
        path: &Path,
        language_id: &str,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<(), LspSemanticRequestError> {
        let uri = file_uri(path);
        let path = path.to_path_buf();
        let cx = tracedecay_runtime_core::native_runtime::cx().ok_or_else(|| {
            LspSemanticRequestError::Transport {
                class: "native runtime unavailable for semantic document read".to_owned(),
            }
        })?;
        let read = cx
            .spawn_blocking(move |_| read_semantic_document(&path))
            .map_err(|error| LspSemanticRequestError::InvalidResponse {
                class: format!("failed to schedule semantic document read: {error}"),
            })?;
        let text = tokio::time::timeout_at(
            tokio::time::Instant::from_std(timeouts.request_deadline()),
            read.join(&cx),
        )
        .await
        .map_err(|_| LspSemanticRequestError::TimedOut)?
        .map_err(|error| LspSemanticRequestError::InvalidResponse {
            class: format!("semantic document read did not complete: {error}"),
        })?
        .map_err(|class| LspSemanticRequestError::InvalidResponse { class })?;
        self.synchronize_document(&uri, language_id, &text, None, timeouts)
            .await
            .map_err(semantic_write_error)?;
        Ok(())
    }

    pub(crate) async fn ensure_document_snapshot(
        &mut self,
        path: &Path,
        snapshot: &crate::LspSemanticDocumentSnapshot,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<(), LspSemanticRequestError> {
        let uri = file_uri(path);
        if snapshot.uri != uri
            || ContentDigest::of_bytes(snapshot.text.as_bytes()) != snapshot.content_digest
        {
            return Err(LspSemanticRequestError::InvalidResponse {
                class: "admitted semantic document snapshot is invalid".to_owned(),
            });
        }
        if self.snapshot_is_stale(&uri, snapshot.version, &snapshot.text) {
            return Err(LspSemanticRequestError::InvalidResponse {
                class: "admitted semantic document snapshot is stale".to_owned(),
            });
        }
        self.synchronize_document(
            &uri,
            &snapshot.language_id,
            &snapshot.text,
            Some(snapshot.version),
            timeouts,
        )
        .await
        .map_err(semantic_write_error)?;
        Ok(())
    }

    fn snapshot_is_stale(&self, uri: &str, version: i64, text: &str) -> bool {
        self.documents.get(uri).is_some_and(|current| {
            current.snapshot_version.is_some_and(|current_version| {
                current_version > version || (current_version == version && current.text != text)
            })
        })
    }

    async fn synchronize_document(
        &mut self,
        uri: &str,
        language_id: &str,
        text: &str,
        snapshot_version: Option<i64>,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<i32, LspMessageWriteError> {
        if let Some(current) = self.documents.get_mut(uri)
            && current.text == text
        {
            current.snapshot_version = snapshot_version;
            return Ok(current.wire_version);
        }
        let current_version = self
            .documents
            .get(uri)
            .map_or(0, |current| current.wire_version);
        let (version, notification) = if current_version == 0 {
            (
                1,
                json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didOpen",
                    "params": {
                        "textDocument": {
                            "uri": uri,
                            "languageId": language_id,
                            "version": 1,
                            "text": text,
                        }
                    }
                }),
            )
        } else {
            let version = current_version.checked_add(1).ok_or_else(|| {
                LspMessageWriteError::Failed(TraceDecayError::Config {
                    message: "analyzer document version is exhausted".to_owned(),
                })
            })?;
            (
                version,
                json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": {
                            "uri": uri,
                            "version": version
                        },
                        "contentChanges": [{
                            "text": text,
                        }]
                    }
                }),
            )
        };
        let deadline = timeouts
            .request_deadline()
            .min(Instant::now() + timeouts.refresh);
        write_message_until(&self.process, notification, deadline, timeouts.message_io).await?;
        self.documents.insert(
            uri.to_owned(),
            SynchronizedDocument {
                wire_version: version,
                text: text.to_owned(),
                snapshot_version,
            },
        );
        Ok(version)
    }

    /// Sends one standard semantic request and returns its standard JSON
    /// result after matching the JSON-RPC correlation id. Notifications and
    /// stale responses from a cancelled request are deliberately ignored.
    #[tracing::instrument(name = "lsp_analyzer_semantic_request", level = "trace", skip_all)]
    pub async fn semantic_request(
        &mut self,
        request: LspSemanticRequest,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        let timeouts = timeouts.with_deadline(timeouts.request_deadline());
        match request {
            LspSemanticRequest::Declaration(params) => {
                self.declaration(params, cancellation, timeouts).await
            }
            LspSemanticRequest::Definition(params) => {
                self.definition(params, cancellation, timeouts).await
            }
            LspSemanticRequest::TypeDefinition(params) => {
                self.type_definition(params, cancellation, timeouts).await
            }
            LspSemanticRequest::Implementation(params) => {
                self.implementation(params, cancellation, timeouts).await
            }
            LspSemanticRequest::References(params) => {
                self.references(params, cancellation, timeouts).await
            }
            LspSemanticRequest::Hover(params) => self.hover(params, cancellation, timeouts).await,
            LspSemanticRequest::DocumentSymbols(params) => {
                // Boxed: with profiling enabled this arm's future is the
                // largest in the dispatch and inflates every sibling arm,
                // since a match future is as large as its widest branch.
                Box::pin(self.document_symbols(params, cancellation, timeouts)).await
            }
            LspSemanticRequest::WorkspaceSymbols(params) => {
                // Boxed: with profiling enabled this arm's future is the
                // largest in the dispatch and inflates every sibling arm,
                // since a match future is as large as its widest branch.
                Box::pin(self.workspace_symbols(params, cancellation, timeouts)).await
            }
            LspSemanticRequest::PrepareCallHierarchy(params) => {
                self.prepare_call_hierarchy(params, cancellation, timeouts)
                    .await
            }
            LspSemanticRequest::IncomingCalls(params) => {
                self.incoming_calls(params, cancellation, timeouts).await
            }
            LspSemanticRequest::OutgoingCalls(params) => {
                self.outgoing_calls(params, cancellation, timeouts).await
            }
            LspSemanticRequest::SignatureHelp(params) => {
                self.signature_help(params, cancellation, timeouts).await
            }
            LspSemanticRequest::PrepareTypeHierarchy(params) => {
                self.prepare_type_hierarchy(params, cancellation, timeouts)
                    .await
            }
            LspSemanticRequest::TypeHierarchySupertypes(params) => {
                self.type_hierarchy_supertypes(params, cancellation, timeouts)
                    .await
            }
            LspSemanticRequest::TypeHierarchySubtypes(params) => {
                self.type_hierarchy_subtypes(params, cancellation, timeouts)
                    .await
            }
            LspSemanticRequest::PrepareRename(params) => {
                self.prepare_rename(params, cancellation, timeouts).await
            }
        }
    }

    pub async fn declaration(
        &mut self,
        params: GotoDeclarationParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<GotoDeclaration>(params, cancellation, timeouts)
            .await
    }

    pub async fn definition(
        &mut self,
        params: GotoDefinitionParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<GotoDefinition>(params, cancellation, timeouts)
            .await
    }

    pub async fn type_definition(
        &mut self,
        params: GotoTypeDefinitionParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<GotoTypeDefinition>(params, cancellation, timeouts)
            .await
    }

    pub async fn implementation(
        &mut self,
        params: GotoImplementationParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<GotoImplementation>(params, cancellation, timeouts)
            .await
    }

    pub async fn references(
        &mut self,
        params: ReferenceParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<References>(params, cancellation, timeouts)
            .await
    }

    pub async fn hover(
        &mut self,
        params: HoverParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<HoverRequest>(params, cancellation, timeouts)
            .await
    }

    #[tracing::instrument(name = "lsp_analyzer_document_symbols", level = "trace", skip_all)]
    pub async fn document_symbols(
        &mut self,
        params: DocumentSymbolParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<DocumentSymbolRequest>(params, cancellation, timeouts)
            .await
    }

    #[tracing::instrument(name = "lsp_analyzer_workspace_symbols", level = "trace", skip_all)]
    pub async fn workspace_symbols(
        &mut self,
        params: WorkspaceSymbolParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<WorkspaceSymbolRequest>(params, cancellation, timeouts)
            .await
    }

    pub async fn prepare_call_hierarchy(
        &mut self,
        params: CallHierarchyPrepareParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<CallHierarchyPrepare>(params, cancellation, timeouts)
            .await
    }

    pub async fn incoming_calls(
        &mut self,
        params: CallHierarchyIncomingCallsParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<CallHierarchyIncomingCalls>(params, cancellation, timeouts)
            .await
    }

    pub async fn outgoing_calls(
        &mut self,
        params: CallHierarchyOutgoingCallsParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<CallHierarchyOutgoingCalls>(params, cancellation, timeouts)
            .await
    }

    pub async fn signature_help(
        &mut self,
        params: SignatureHelpParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<SignatureHelpRequest>(params, cancellation, timeouts)
            .await
    }

    pub async fn prepare_type_hierarchy(
        &mut self,
        params: TypeHierarchyPrepareParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<TypeHierarchyPrepare>(params, cancellation, timeouts)
            .await
    }

    pub async fn type_hierarchy_supertypes(
        &mut self,
        params: TypeHierarchySupertypesParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<TypeHierarchySupertypes>(params, cancellation, timeouts)
            .await
    }

    pub async fn type_hierarchy_subtypes(
        &mut self,
        params: TypeHierarchySubtypesParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<TypeHierarchySubtypes>(params, cancellation, timeouts)
            .await
    }

    pub async fn prepare_rename(
        &mut self,
        params: TextDocumentPositionParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<PrepareRenameRequest>(params, cancellation, timeouts)
            .await
    }

    #[tracing::instrument(name = "lsp_analyzer_request_json", level = "trace", skip_all)]
    async fn request_json<R>(
        &mut self,
        params: R::Params,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError>
    where
        R: LspRequest,
    {
        // Boxed: with profiling enabled `request` is the widest I/O future in
        // this client. Awaiting it by value inflates every semantic wrapper
        // and the dispatch match (a match future is as large as its widest
        // arm).
        let response = Box::pin(self.request::<R>(params, cancellation, timeouts)).await?;
        serde_json::to_value(response).map_err(|error| LspSemanticRequestError::InvalidResponse {
            class: error.to_string(),
        })
    }

    #[tracing::instrument(name = "lsp_analyzer_request", level = "trace", skip_all)]
    async fn request<R>(
        &mut self,
        params: R::Params,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<R::Result, LspSemanticRequestError>
    where
        R: LspRequest,
    {
        let deadline = timeouts
            .request_deadline()
            .min(Instant::now() + timeouts.refresh);
        if cancellation.is_cancelled() {
            return Err(LspSemanticRequestError::Cancelled);
        }
        let request_id = self.next_request_id.next_number().map_err(|error| {
            LspSemanticRequestError::InvalidResponse {
                class: error.to_string(),
            }
        })?;
        let params = serde_json::to_value(params).map_err(|error| {
            LspSemanticRequestError::InvalidResponse {
                class: error.to_string(),
            }
        })?;
        let write = tokio::select! {
            () = cancellation.cancelled() => return Err(LspSemanticRequestError::Cancelled),
            result = write_message_until(
                &self.process,
                json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": R::METHOD,
                    "params": params,
                }),
                deadline,
                timeouts.message_io,
            ) => result,
        };
        write.map_err(|error| match error {
            LspMessageWriteError::DeadlineElapsed => LspSemanticRequestError::TimedOut,
            LspMessageWriteError::Failed(error) => semantic_transport_error(error),
        })?;

        loop {
            let message = tokio::select! {
                () = cancellation.cancelled() => {
                    let _ = self.cancel_request(request_id, timeouts).await;
                    return Err(LspSemanticRequestError::Cancelled);
                }
                message = read_message_until(&self.process, deadline) => {
                    match message {
                        Ok(message) => message,
                        Err(LspSemanticRequestError::TimedOut) => {
                            let _ = self.cancel_request(request_id, timeouts).await;
                            return Err(LspSemanticRequestError::TimedOut);
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            let message = match message {
                SemanticMessagePoll::Message(message) => message,
                SemanticMessagePoll::Pending => {
                    let _ = self.cancel_request(request_id, timeouts).await;
                    return Err(LspSemanticRequestError::TimedOut);
                }
                SemanticMessagePoll::Closed => {
                    return Err(self.closed_semantic_error().await);
                }
            };
            match message {
                JsonRpcMessage::Request { id, method, params } => {
                    dispatch_server_request(
                        &self.process,
                        id,
                        &method,
                        params.as_ref(),
                        &self.workspace_root,
                        timeouts,
                    )
                    .await
                    .map_err(semantic_transport_error)?;
                }
                JsonRpcMessage::Notification { .. } => {}
                JsonRpcMessage::Success { id, result } if id == json!(request_id) => {
                    return serde_json::from_value(result).map_err(|error| {
                        LspSemanticRequestError::InvalidResponse {
                            class: error.to_string(),
                        }
                    });
                }
                JsonRpcMessage::Error { id, error } if id == json!(request_id) => {
                    return Err(LspSemanticRequestError::Remote {
                        code: Some(error.code),
                        message: error.message,
                    });
                }
                JsonRpcMessage::Success { .. } | JsonRpcMessage::Error { .. } => {}
            }
        }
    }

    async fn cancel_request(
        &mut self,
        request_id: u64,
        timeouts: LspRefreshTimeouts,
    ) -> Result<()> {
        write_message_until(
            &self.process,
            cancel_request_message(request_id),
            timeouts.request_deadline(),
            timeouts.message_io,
        )
        .await
        .map_err(LspMessageWriteError::into_trace_decay_error)
    }

    #[tracing::instrument(name = "lsp_analyzer_client_refresh", level = "trace", skip_all)]
    pub async fn collect_document_diagnostics(
        &mut self,
        project_root: &Path,
        documents: Vec<LspDocument>,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<DiagnosticRefreshResult, LspRefreshError> {
        let deadline = timeouts.request_deadline();
        let mut uri_to_document = BTreeMap::new();
        let mut expected_versions = BTreeMap::new();
        for mut document in documents {
            let uri = file_uri(&project_root.join(&document.relative_path));
            if let Some(version) = document.snapshot_version
                && self.snapshot_is_stale(&uri, version, &document.text)
            {
                return Err(LspRefreshError::InvalidResponse {
                    message: "admitted diagnostic document snapshot is stale".to_owned(),
                });
            }
            // Background disk sweeps do not revoke an admitted editor snapshot.
            // A diagnostic request carrying a newer overlay may advance the same
            // ledger as the semantic lane; a disk-only semantic request releases it.
            if document.snapshot_version.is_none()
                && let Some(current) = self.documents.get(&uri)
                && current.snapshot_version.is_some()
            {
                document.text.clone_from(&current.text);
                document.snapshot_version = current.snapshot_version;
            }
            let version = self
                .synchronize_document(
                    &uri,
                    &document.language_id,
                    &document.text,
                    document.snapshot_version,
                    timeouts.with_deadline(deadline),
                )
                .await
                .map_err(refresh_write_error)?;
            expected_versions.insert(uri.clone(), version);
            uri_to_document.insert(uri, document);
        }

        if uri_to_document.is_empty() {
            return Ok(DiagnosticRefreshResult::default());
        }
        let mut diagnostics_by_uri: BTreeMap<String, Vec<CodeDiagnostic>> = BTreeMap::new();
        let mut coverage_by_uri = BTreeMap::new();
        let refresh_deadline = deadline.min(Instant::now() + timeouts.refresh);
        let mut quiet_deadline = None;
        loop {
            let now = Instant::now();
            let deadline = quiet_deadline.map_or(refresh_deadline, |deadline: Instant| {
                deadline.min(refresh_deadline)
            });
            if now >= deadline {
                break;
            }
            let message =
                match read_refresh_message_until(&self.process, deadline, timeouts).await? {
                    RefreshMessagePoll::Message(message) => message,
                    RefreshMessagePoll::Quiet => break,
                    RefreshMessagePoll::Closed => return Err(self.closed_refresh_error().await),
                };
            let params = match message {
                JsonRpcMessage::Request { id, method, params } => {
                    dispatch_server_request(
                        &self.process,
                        id,
                        &method,
                        params.as_ref(),
                        &self.workspace_root,
                        timeouts,
                    )
                    .await
                    .map_err(refresh_transport_error)?;
                    continue;
                }
                JsonRpcMessage::Notification { method, params }
                    if method == "textDocument/publishDiagnostics" =>
                {
                    params.ok_or_else(|| LspRefreshError::InvalidResponse {
                        message: "LSP publishDiagnostics notification omitted params".to_owned(),
                    })?
                }
                JsonRpcMessage::Notification { .. }
                | JsonRpcMessage::Success { .. }
                | JsonRpcMessage::Error { .. } => continue,
            };
            let published =
                serde_json::from_value::<PublishDiagnosticsParams>(params).map_err(|error| {
                    LspRefreshError::InvalidResponse {
                        message: format!("failed to decode LSP publishDiagnostics params: {error}"),
                    }
                })?;
            let Some(document) = uri_to_document.get(published.uri.as_str()) else {
                continue;
            };
            if !is_current_diagnostic_publication(&published, &expected_versions) {
                coverage_by_uri
                    .entry(published.uri.as_str().to_owned())
                    .or_insert(DiagnosticDocumentCoverage::StalePublication);
                continue;
            }
            coverage_by_uri.insert(
                published.uri.as_str().to_owned(),
                DiagnosticDocumentCoverage::Published {
                    version_verified: published.version.is_some(),
                    content_digest: ContentDigest::of_bytes(document.text.as_bytes()),
                },
            );
            diagnostics_by_uri.insert(
                published.uri.as_str().to_owned(),
                published
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| code_diagnostic(diagnostic, document, &self.command))
                    .collect(),
            );
            quiet_deadline = Some(deadline.min(Instant::now() + timeouts.diagnostics_quiet));
        }
        // Servers that suppress empty publishes (only publishing for files WITH
        // problems) never emit for clean files, so a fully-reported batch still
        // looks partial here. Requiring every requested URI throws away the real
        // diagnostics of the one file that did report. Only treat the batch as a
        // genuine timeout when nothing arrived at all.
        if diagnostics_by_uri.is_empty() && !uri_to_document.is_empty() {
            return Err(LspRefreshError::NoPublication {
                message: format!(
                    "LSP diagnostics collection timed out after {} ms without a publication",
                    timeouts.refresh.as_millis()
                ),
            });
        }
        let mut result = DiagnosticRefreshResult {
            diagnostics: diagnostics_by_uri.into_values().flatten().collect(),
            coverage: DiagnosticCoverage::default(),
        };
        for (uri, document) in &uri_to_document {
            result.coverage.documents.insert(
                document.relative_path.clone(),
                coverage_by_uri
                    .get(uri)
                    .cloned()
                    .unwrap_or(DiagnosticDocumentCoverage::NoPublication),
            );
        }
        Ok(result)
    }

    async fn closed_refresh_error(&self) -> LspRefreshError {
        match self.process.try_wait().await {
            Ok(Some(status)) => LspRefreshError::ProcessExited {
                message: format!(
                    "LSP server '{}' exited while collecting diagnostics ({status})",
                    self.command
                ),
            },
            Ok(None) => LspRefreshError::Transport {
                message: format!(
                    "LSP server '{}' closed its diagnostics transport",
                    self.command
                ),
            },
            Err(error) => LspRefreshError::Transport {
                message: format!(
                    "failed to inspect LSP server '{}' after its diagnostics transport closed: {error}",
                    self.command
                ),
            },
        }
    }

    async fn closed_semantic_error(&self) -> LspSemanticRequestError {
        match self.process.try_wait().await {
            Ok(Some(status)) => LspSemanticRequestError::ProcessExited {
                status: status.to_string(),
            },
            Ok(None) => LspSemanticRequestError::Transport {
                class: format!(
                    "LSP server '{}' closed its semantic transport",
                    self.command
                ),
            },
            Err(error) => LspSemanticRequestError::Transport {
                class: format!(
                    "failed to inspect LSP server '{}' after its semantic transport closed: {error}",
                    self.command
                ),
            },
        }
    }

    /// Terminates and waits for a retired process. The shared client slot
    /// stores this future when an in-flight owner is dropped, and no successor
    /// may acquire that slot until the future completes.
    pub(crate) async fn reap(mut self) -> Result<()> {
        self.process
            .terminate()
            .await
            .map(drop)
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to reap LSP server '{}': {error}", self.command),
            })
    }
}

fn is_current_diagnostic_publication(
    published: &PublishDiagnosticsParams,
    expected_versions: &BTreeMap<String, i32>,
) -> bool {
    // `version` is optional in the LSP spec and several servers omit it
    // entirely. Rejecting a versionless publication discards every diagnostic
    // those servers ever produce, so treat an absent version as current and
    // reject only versions the server explicitly reports as stale.
    match published.version {
        Some(version) => expected_versions.get(published.uri.as_str()).copied() == Some(version),
        None => true,
    }
}

fn enrich_start_error(command: &str, err: TraceDecayError, stderr: &str) -> TraceDecayError {
    if stderr.is_empty() {
        return err;
    }
    TraceDecayError::Config {
        message: format!("{command} failed during initialize: {err}; stderr: {stderr}"),
    }
}

async fn wait_for_initialize(
    process: &AnalyzerProcess,
    deadline: Instant,
    command: &str,
    timeout: Duration,
    workspace_root: &Path,
    timeouts: LspRefreshTimeouts,
) -> Result<Value> {
    loop {
        let frame = match process.read_frame_until(deadline).await {
            Ok(FramePoll::Frame(frame)) => frame,
            Ok(FramePoll::Pending) | Err(AsyncContentLengthError::DeadlineElapsed) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "LSP server '{command}' initialize timed out after {} ms",
                        timeout.as_millis()
                    ),
                });
            }
            Ok(FramePoll::Closed) => {
                return Err(TraceDecayError::Config {
                    message: "LSP server closed before initialize response".to_string(),
                });
            }
            Err(error) => return Err(frame_read_error(error)),
        };
        let message = decode_message(&frame)?;
        match message {
            JsonRpcMessage::Request { id, method, params } => {
                dispatch_server_request(
                    process,
                    id,
                    &method,
                    params.as_ref(),
                    workspace_root,
                    timeouts,
                )
                .await?;
            }
            JsonRpcMessage::Notification { .. } => {}
            JsonRpcMessage::Success { id, result } if id == json!(1) => return Ok(result),
            JsonRpcMessage::Error { id, error } if id == json!(1) => {
                let error = LspSemanticRequestError::Remote {
                    code: Some(error.code),
                    message: error.message,
                };
                return Err(TraceDecayError::Config {
                    message: format!("LSP server '{command}' rejected initialize: {error}"),
                });
            }
            JsonRpcMessage::Success { .. } | JsonRpcMessage::Error { .. } => {}
        }
    }
}

enum LspMessageWriteError {
    DeadlineElapsed,
    Failed(TraceDecayError),
}

impl LspMessageWriteError {
    fn into_trace_decay_error(self) -> TraceDecayError {
        match self {
            Self::DeadlineElapsed => TraceDecayError::Config {
                message: "LSP message write timed out".to_owned(),
            },
            Self::Failed(error) => error,
        }
    }
}

async fn write_message_until(
    process: &AnalyzerProcess,
    value: Value,
    deadline: Instant,
    timeout: Duration,
) -> std::result::Result<(), LspMessageWriteError> {
    let body = serde_json::to_vec(&value).map_err(|error| {
        LspMessageWriteError::Failed(TraceDecayError::Config {
            message: format!("failed to encode LSP message: {error}"),
        })
    })?;
    match process
        .write_frame(&body, deadline.min(Instant::now() + timeout))
        .await
    {
        Err(AsyncContentLengthError::DeadlineElapsed) => Err(LspMessageWriteError::DeadlineElapsed),
        Err(error) => Err(LspMessageWriteError::Failed(frame_write_error(error))),
        Ok(()) => Ok(()),
    }
}

fn semantic_transport_error(error: TraceDecayError) -> LspSemanticRequestError {
    LspSemanticRequestError::Transport {
        class: error.to_string(),
    }
}

fn semantic_write_error(error: LspMessageWriteError) -> LspSemanticRequestError {
    match error {
        LspMessageWriteError::DeadlineElapsed => LspSemanticRequestError::TimedOut,
        LspMessageWriteError::Failed(error) => semantic_transport_error(error),
    }
}

fn refresh_transport_error(error: TraceDecayError) -> LspRefreshError {
    LspRefreshError::Transport {
        message: error.to_string(),
    }
}

fn refresh_write_error(error: LspMessageWriteError) -> LspRefreshError {
    match error {
        LspMessageWriteError::DeadlineElapsed => LspRefreshError::TimedOut {
            message: "analyzer refresh timed out while writing a document".to_owned(),
        },
        LspMessageWriteError::Failed(error) => refresh_transport_error(error),
    }
}

enum RefreshMessagePoll {
    Message(JsonRpcMessage),
    Quiet,
    Closed,
}

async fn read_refresh_message_until(
    process: &AnalyzerProcess,
    deadline: Instant,
    timeouts: LspRefreshTimeouts,
) -> std::result::Result<RefreshMessagePoll, LspRefreshError> {
    match process.read_frame_until(deadline).await {
        Ok(FramePoll::Frame(frame)) => serde_json::from_slice(&frame)
            .map(RefreshMessagePoll::Message)
            .map_err(|error| LspRefreshError::InvalidResponse {
                message: format!("failed to parse LSP message: {error}"),
            }),
        Ok(FramePoll::Pending) => Ok(RefreshMessagePoll::Quiet),
        Ok(FramePoll::Closed) => Ok(RefreshMessagePoll::Closed),
        Err(AsyncContentLengthError::Io(error)) => Err(LspRefreshError::Transport {
            message: format!("failed to read LSP frame: {error}"),
        }),
        Err(AsyncContentLengthError::Codec(error)) => Err(LspRefreshError::InvalidResponse {
            message: format!("failed to decode LSP Content-Length frame: {error:?}"),
        }),
        Err(AsyncContentLengthError::DeadlineElapsed) => Err(LspRefreshError::InvalidResponse {
            message: format!(
                "partial LSP diagnostics frame timed out after {} ms",
                timeouts.refresh.as_millis()
            ),
        }),
    }
}

fn cancel_request_message(request_id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "$/cancelRequest",
        "params": { "id": request_id },
    })
}

/// Wait phase of one analyzer round-trip: the futures lane separates time
/// suspended on analyzer stdout from poll (frame-decode) time, which a wall
/// span alone cannot distinguish.
#[tracing::instrument(name = "lsp.analyzer.response_wait", level = "trace", skip_all)]
async fn read_message_until(
    process: &AnalyzerProcess,
    deadline: Instant,
) -> std::result::Result<SemanticMessagePoll, LspSemanticRequestError> {
    match process.read_frame_until(deadline).await {
        Ok(FramePoll::Frame(frame)) => decode_message(&frame)
            .map(SemanticMessagePoll::Message)
            .map_err(|error| LspSemanticRequestError::InvalidResponse {
                class: error.to_string(),
            }),
        Ok(FramePoll::Pending) => Ok(SemanticMessagePoll::Pending),
        Ok(FramePoll::Closed) => Ok(SemanticMessagePoll::Closed),
        Err(AsyncContentLengthError::DeadlineElapsed) => Err(LspSemanticRequestError::TimedOut),
        Err(AsyncContentLengthError::Io(error)) => Err(LspSemanticRequestError::Transport {
            class: format!("failed to read LSP frame: {error}"),
        }),
        Err(AsyncContentLengthError::Codec(error)) => {
            Err(LspSemanticRequestError::InvalidResponse {
                class: format!("failed to decode LSP Content-Length frame: {error:?}"),
            })
        }
    }
}

enum SemanticMessagePoll {
    Message(JsonRpcMessage),
    Pending,
    Closed,
}

#[tracing::instrument(name = "lsp.analyzer.response_parse", level = "trace", skip_all)]
fn decode_message(body: &[u8]) -> Result<JsonRpcMessage> {
    let value: Value = serde_json::from_slice(body).map_err(|error| TraceDecayError::Config {
        message: format!("failed to parse LSP message: {error}"),
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| invalid_json_rpc_envelope())?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(invalid_json_rpc_envelope());
    }
    let id = object.get("id").cloned();
    let method = object.get("method");
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");
    if let Some(method) = method {
        if has_result || has_error {
            return Err(invalid_json_rpc_envelope());
        }
        let method = method
            .as_str()
            .filter(|method| !method.is_empty())
            .ok_or_else(invalid_json_rpc_envelope)?
            .to_owned();
        let params = object.get("params").cloned();
        if params
            .as_ref()
            .is_some_and(|params| !(params.is_object() || params.is_array()))
        {
            return Err(invalid_json_rpc_envelope());
        }
        return match id {
            Some(id) if valid_json_rpc_id(&id) => {
                Ok(JsonRpcMessage::Request { id, method, params })
            }
            None => Ok(JsonRpcMessage::Notification { method, params }),
            Some(_) => Err(invalid_json_rpc_envelope()),
        };
    }
    if object.contains_key("params") {
        return Err(invalid_json_rpc_envelope());
    }
    let id = id
        .filter(valid_json_rpc_id)
        .ok_or_else(invalid_json_rpc_envelope)?;
    match (has_result, has_error) {
        (true, false) => Ok(JsonRpcMessage::Success {
            id,
            result: object["result"].clone(),
        }),
        (false, true) => {
            let error = serde_json::from_value(object["error"].clone()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("invalid JSON-RPC error response: {error}"),
                }
            })?;
            Ok(JsonRpcMessage::Error { id, error })
        }
        _ => Err(invalid_json_rpc_envelope()),
    }
}

fn valid_json_rpc_id(id: &Value) -> bool {
    id.is_string() || id.is_number()
}

fn invalid_json_rpc_envelope() -> TraceDecayError {
    TraceDecayError::Config {
        message: "invalid JSON-RPC 2.0 message envelope".to_owned(),
    }
}

fn frame_read_error(error: AsyncContentLengthError) -> TraceDecayError {
    let message = match error {
        AsyncContentLengthError::Io(error) => format!("failed to read LSP frame: {error}"),
        AsyncContentLengthError::Codec(error) => {
            format!("failed to decode LSP Content-Length frame: {error:?}")
        }
        AsyncContentLengthError::DeadlineElapsed => "LSP frame read timed out".to_owned(),
    };
    TraceDecayError::Config { message }
}

fn frame_write_error(error: AsyncContentLengthError) -> TraceDecayError {
    let message = match error {
        AsyncContentLengthError::Io(error) => format!("failed to write LSP message: {error}"),
        AsyncContentLengthError::Codec(error) => {
            format!("failed to encode LSP Content-Length frame: {error:?}")
        }
        AsyncContentLengthError::DeadlineElapsed => "LSP message write timed out".to_owned(),
    };
    TraceDecayError::Config { message }
}

async fn dispatch_server_request(
    process: &AnalyzerProcess,
    id: Value,
    method: &str,
    params: Option<&Value>,
    workspace_root: &Path,
    timeouts: LspRefreshTimeouts,
) -> Result<()> {
    let response = match method {
        "workspace/configuration" => {
            let item_count = params
                .and_then(|params| params.get("items"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": vec![Value::Null; item_count],
            })
        }
        "workspace/workspaceFolders" => {
            let name = workspace_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workspace");
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": [{
                    "uri": file_uri(workspace_root),
                    "name": name,
                }],
            })
        }
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create"
        | "window/showMessageRequest" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null,
        }),
        "workspace/applyEdit" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "applied": false,
                "failureReason": "workspace edits are not applied by this adapter",
            },
        }),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("unsupported server request: {method}"),
            },
        }),
    };
    let deadline = timeouts
        .deadline
        .unwrap_or_else(|| Instant::now() + timeouts.message_io);
    write_message_until(process, response, deadline, timeouts.message_io)
        .await
        .map_err(LspMessageWriteError::into_trace_decay_error)
}

pub(crate) fn file_uri(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    file_uri_from_path_text(&absolute.to_string_lossy())
}

fn read_semantic_document(path: &Path) -> std::result::Result<String, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("failed to read semantic document: {error}"))?;
    let mut bytes = Vec::with_capacity(crate::overlay::MAX_OVERLAY_BYTES);
    file.take((crate::overlay::MAX_OVERLAY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read semantic document: {error}"))?;
    if bytes.len() > crate::overlay::MAX_OVERLAY_BYTES {
        return Err(format!(
            "semantic document exceeds the {} byte limit",
            crate::overlay::MAX_OVERLAY_BYTES
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| format!("semantic document is not valid UTF-8: {error}"))
}

fn lsp_initialization_options(command: &str) -> Value {
    if Path::new(command)
        .file_stem()
        .and_then(|name| name.to_str())
        == Some("rust-analyzer")
    {
        // TraceDecay owns compiler diagnostics separately. Keep rust-analyzer's
        // fast native diagnostics without launching a competing Cargo flycheck.
        json!({ "checkOnSave": false })
    } else {
        json!({})
    }
}

/// Build a `file://` URI from raw path text, normalizing `\` to `/` and
/// percent-encoding. Handles POSIX paths, Windows drive paths (`C:/…`), and UNC
/// (`//server/share`) prefixes. Shared with the Kiro installer.
pub fn file_uri_from_path_text(path: &str) -> String {
    let normalized = plain_host_path(Path::new(path))
        .to_string_lossy()
        .replace('\\', "/");
    let encoded = percent_encode_file_uri_path(&normalized);
    if normalized.starts_with("//") {
        format!("file:{encoded}")
    } else if looks_like_windows_drive_path(&normalized) {
        format!("file:///{encoded}")
    } else {
        format!("file://{encoded}")
    }
}

fn looks_like_windows_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

fn percent_encode_file_uri_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(*byte as char);
            }
            _ => {
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

#[derive(Debug)]
enum JsonRpcMessage {
    Request {
        id: Value,
        method: String,
        params: Option<Value>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
    Success {
        id: Value,
        result: Value,
    },
    Error {
        id: Value,
        error: JsonRpcError,
    },
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

fn code_diagnostic(
    diagnostic: StandardDiagnostic,
    document: &LspDocument,
    command: &str,
) -> CodeDiagnostic {
    CodeDiagnostic {
        language: document.language.clone(),
        source: diagnostic.source.unwrap_or_else(|| command.to_string()),
        file: document.relative_path.clone(),
        line_start: diagnostic.range.start.line + 1,
        line_end: diagnostic.range.end.line + 1,
        character_start: Some(diagnostic.range.start.character),
        character_end: Some(diagnostic.range.end.character),
        severity: match diagnostic.severity {
            Some(StandardDiagnosticSeverity::ERROR) => DiagnosticSeverity::Error,
            Some(StandardDiagnosticSeverity::WARNING) => DiagnosticSeverity::Warning,
            Some(StandardDiagnosticSeverity::HINT) => DiagnosticSeverity::Hint,
            _ => DiagnosticSeverity::Information,
        },
        code: diagnostic.code.map(code_to_string),
        message: diagnostic.message,
        // The LSP client has no code-graph handle; the enclosing symbol is
        // resolved later via `DiagnosticBroker::resolve_enclosing_nodes`,
        // which has access to the indexed nodes for the file.
        enclosing_node: None,
        updated_at: tracedecay_runtime_core::tracedecay::saturating_unix_secs(),
    }
}

fn code_to_string(value: NumberOrString) -> String {
    match value {
        NumberOrString::String(value) => value,
        NumberOrString::Number(value) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use lsp_types::PublishDiagnosticsParams;
    use serde_json::json;

    use super::{
        LspSemanticRequestError, cancel_request_message, decode_semantic_request,
        encode_semantic_request, file_uri_from_path_text, is_current_diagnostic_publication,
    };

    #[test]
    fn semantic_request_codec_round_trips_standard_wire_shape() {
        let request = crate::lsp_semantic_request(&crate::SemanticRequest::WorkspaceSymbols {
            query: "needle".to_owned(),
        })
        .expect("generic semantic request");
        let method = request.method();
        let params = request.params().clone();

        let encoded =
            encode_semantic_request(decode_semantic_request(request).expect("decode request"))
                .expect("encode request");

        assert_eq!(encoded.method(), method);
        assert_eq!(encoded.params(), &params);
    }

    #[test]
    fn semantic_error_coverage_ignores_free_form_error_text() {
        let messages = [
            "stale response: Bearer super-secret-token!",
            "https://admin:hunter2@example.test/private?credential=yes",
            "/home/alice/.aws/credentials: permission denied?!",
            r"C:\Users\alice\secret.rs: unexpected !!!",
        ];

        for message in messages {
            assert_eq!(
                LspSemanticRequestError::Remote {
                    code: Some(-32603),
                    message: message.to_owned(),
                }
                .coverage_token(),
                "analyzer-remote-error"
            );
            assert_eq!(
                LspSemanticRequestError::Remote {
                    code: None,
                    message: message.to_owned(),
                }
                .coverage_token(),
                "analyzer-remote-error"
            );
            assert_eq!(
                LspSemanticRequestError::Transport {
                    class: message.to_owned(),
                }
                .coverage_token(),
                "analyzer-transport-failed"
            );
            assert_eq!(
                LspSemanticRequestError::InvalidResponse {
                    class: message.to_owned(),
                }
                .coverage_token(),
                "analyzer-invalid-response"
            );
        }
    }

    #[test]
    fn file_uri_encodes_lsp_paths() {
        assert_eq!(
            file_uri_from_path_text("/tmp/trace decay/main#one.rs"),
            "file:///tmp/trace%20decay/main%23one.rs"
        );
        assert_eq!(
            file_uri_from_path_text(r"C:\repo with spaces\src\main.rs"),
            "file:///C:/repo%20with%20spaces/src/main.rs"
        );
        assert_eq!(
            file_uri_from_path_text("/tmp/100% real.rs"),
            "file:///tmp/100%25%20real.rs"
        );
        assert_eq!(
            file_uri_from_path_text(r"\\?\D:\repo\src\main.rs"),
            "file:///D:/repo/src/main.rs",
            "a canonicalized Windows path must not become a `?` URL host"
        );
    }

    #[test]
    fn cancellation_uses_the_standard_json_rpc_notification() {
        assert_eq!(
            cancel_request_message(42),
            json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": { "id": 42 },
            })
        );
    }

    #[test]
    fn json_rpc_response_classification_preserves_direction_and_null_results() {
        assert!(matches!(
            decode_message(
                br#"{"jsonrpc":"2.0","id":2,"method":"window/showMessageRequest","params":{"message":"hello"}}"#
            )
            .expect("inbound request"),
            JsonRpcMessage::Request { id, .. } if id == json!(2)
        ));
        assert!(matches!(
            decode_message(br#"{"jsonrpc":"2.0","id":"2","result":null}"#)
                .expect("explicit null result"),
            JsonRpcMessage::Success { id, result } if id == json!("2") && result.is_null()
        ));
    }

    #[test]
    fn json_rpc_response_classification_rejects_invalid_envelopes() {
        for body in [
            br#"{"jsonrpc":"1.0","id":2,"result":null}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":2}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":2,"result":null,"error":{"code":-1,"message":"bad"}}"#
                .as_slice(),
            br#"{"jsonrpc":"2.0","id":null,"result":null}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":2,"method":"request","result":null}"#.as_slice(),
        ] {
            assert!(
                decode_message(body).is_err(),
                "invalid envelope was accepted: {}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn diagnostics_require_the_exact_requested_document_version() {
        let uri = "file:///workspace/src/lib.rs";
        let expected = BTreeMap::from([(uri.to_owned(), 4)]);
        let current: PublishDiagnosticsParams = serde_json::from_value(json!({
            "uri": uri,
            "version": 4,
            "diagnostics": [],
        }))
        .expect("current diagnostics");
        let stale: PublishDiagnosticsParams = serde_json::from_value(json!({
            "uri": uri,
            "version": 3,
            "diagnostics": [],
        }))
        .expect("stale diagnostics");
        let versionless: PublishDiagnosticsParams = serde_json::from_value(json!({
            "uri": uri,
            "diagnostics": [],
        }))
        .expect("versionless diagnostics");

        assert!(is_current_diagnostic_publication(&current, &expected));
        assert!(!is_current_diagnostic_publication(&stale, &expected));
        // `version` is optional in the LSP spec; a server that omits it must
        // not have all of its diagnostics discarded.
        assert!(is_current_diagnostic_publication(&versionless, &expected));
    }
}
