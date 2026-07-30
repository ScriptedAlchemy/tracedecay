use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

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
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::broker::{CodeDiagnostic, DiagnosticSeverity};
use super::error::{
    AnalyzerCancellation as CancellationToken, AnalyzerResult as Result,
    AnalyzerRuntimeError as TraceDecayError,
};
use crate::{
    AnalyzerEvent, AsyncContentLengthError, AsyncContentLengthReader,
    ConnectionLocalRequestSequence, FramePoll, write_content_length_frame_until,
};

const MIN_MESSAGE_IO_TIMEOUT: Duration = Duration::from_secs(2);
const MIN_INITIALIZE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspRefreshTimeouts {
    refresh: Duration,
    initialize_response: Duration,
    message_io: Duration,
    diagnostics_quiet: Duration,
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
        }
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDocument {
    pub language: String,
    pub language_id: String,
    pub relative_path: String,
    pub text: String,
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
        LspSemanticRequest::Declaration(params) => serde_json::to_value(params),
        LspSemanticRequest::Definition(params) => serde_json::to_value(params),
        LspSemanticRequest::TypeDefinition(params) => serde_json::to_value(params),
        LspSemanticRequest::Implementation(params) => serde_json::to_value(params),
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
    Remote { code: Option<i64>, message: String },
    Transport { class: String },
    InvalidResponse { class: String },
}

impl LspSemanticRequestError {
    pub fn analyzer_event(&self) -> AnalyzerEvent {
        match self {
            Self::Cancelled => AnalyzerEvent::Cancelled,
            Self::TimedOut => AnalyzerEvent::TimedOut,
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

pub async fn collect_document_diagnostics(
    command: &str,
    args: &[String],
    project_root: &Path,
    documents: Vec<LspDocument>,
    diagnostics_quiet_timeout: Duration,
) -> Result<Vec<CodeDiagnostic>> {
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
) -> Result<Vec<CodeDiagnostic>> {
    let mut client =
        StdioLspClient::start_with_timeouts(command, args, project_root, timeouts).await?;
    client
        .collect_document_diagnostics(project_root, documents, timeouts)
        .await
}

pub struct StdioLspClient {
    command: String,
    document_versions: BTreeMap<String, i32>,
    next_request_id: ConnectionLocalRequestSequence,
    stdin: tokio::process::ChildStdin,
    reader: AsyncContentLengthReader<tokio::process::ChildStdout>,
    child: tokio::process::Child,
    stderr_task: JoinHandle<()>,
}

impl StdioLspClient {
    pub async fn start_with_timeouts(
        command: &str,
        args: &[String],
        project_root: &Path,
        timeouts: LspRefreshTimeouts,
    ) -> Result<Self> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .current_dir(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to spawn LSP server '{command}': {e}"),
            })?;

        let mut stdin = child.stdin.take().ok_or_else(|| TraceDecayError::Config {
            message: format!("failed to open stdin for LSP server '{command}'"),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| TraceDecayError::Config {
            message: format!("failed to open stdout for LSP server '{command}'"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| TraceDecayError::Config {
            message: format!("failed to open stderr for LSP server '{command}'"),
        })?;
        let mut reader = AsyncContentLengthReader::new(stdout);
        let stderr_capture = Arc::new(Mutex::new(Vec::new()));
        let stderr_task = spawn_stderr_capture(stderr, Arc::clone(&stderr_capture));

        let send_initialize = write_message_with_timeout(
            &mut stdin,
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
            timeouts.message_io,
        )
        .await;
        // A server that dies immediately can fail the initialize *request*
        // write (broken pipe — on Windows this races the spawn under load)
        // just as easily as the initialize *response* wait. Route both
        // failures through the same stderr-enriched classification so the
        // crash reason (e.g. a toolchain's "unknown binary" complaint) is
        // never dropped.
        let initialize_result = match send_initialize {
            Ok(()) => {
                wait_for_initialize(
                    &mut reader,
                    tokio::time::Instant::now() + timeouts.initialize_response,
                    command,
                    timeouts.initialize_response,
                )
                .await
            }
            Err(err) => Err(err),
        };
        if let Err(err) = initialize_result {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let _ = stderr_task.await;
            let stderr = captured_stderr(&stderr_capture).await;
            return Err(enrich_start_error(command, err, &stderr));
        }
        write_message_with_timeout(
            &mut stdin,
            json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "params": {}
            }),
            timeouts.message_io,
        )
        .await?;

        Ok(Self {
            command: command.to_string(),
            document_versions: BTreeMap::new(),
            next_request_id: ConnectionLocalRequestSequence::starting_at(2),
            stdin,
            reader,
            child,
            stderr_task,
        })
    }

    /// Sends one standard semantic request and returns its standard JSON
    /// result after matching the JSON-RPC correlation id. Notifications and
    /// stale responses from a cancelled request are deliberately ignored.
    pub async fn semantic_request(
        &mut self,
        request: LspSemanticRequest,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
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
                self.document_symbols(params, cancellation, timeouts).await
            }
            LspSemanticRequest::WorkspaceSymbols(params) => {
                self.workspace_symbols(params, cancellation, timeouts).await
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

    pub async fn document_symbols(
        &mut self,
        params: DocumentSymbolParams,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError> {
        self.request_json::<DocumentSymbolRequest>(params, cancellation, timeouts)
            .await
    }

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

    async fn request_json<R>(
        &mut self,
        params: R::Params,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Value, LspSemanticRequestError>
    where
        R: LspRequest,
    {
        let response = self.request::<R>(params, cancellation, timeouts).await?;
        serde_json::to_value(response).map_err(|error| LspSemanticRequestError::InvalidResponse {
            class: error.to_string(),
        })
    }

    async fn request<R>(
        &mut self,
        params: R::Params,
        cancellation: &CancellationToken,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<R::Result, LspSemanticRequestError>
    where
        R: LspRequest,
    {
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
        write_message_with_timeout(
            &mut self.stdin,
            json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": R::METHOD,
                "params": params,
            }),
            timeouts.message_io,
        )
        .await
        .map_err(semantic_transport_error)?;

        let deadline = tokio::time::Instant::now() + timeouts.refresh;
        loop {
            let message = tokio::select! {
                () = cancellation.cancelled() => {
                    let _ = self.cancel_request(request_id, timeouts).await;
                    return Err(LspSemanticRequestError::Cancelled);
                }
                message = read_message_until(&mut self.reader, deadline, timeouts) => {
                    match message {
                        Ok(message) => message,
                        Err(_error) if tokio::time::Instant::now() >= deadline => {
                            let _ = self.cancel_request(request_id, timeouts).await;
                            return Err(LspSemanticRequestError::TimedOut);
                        }
                        Err(error) => return Err(semantic_transport_error(error)),
                    }
                }
            };
            let Some(message) = message else {
                let _ = self.cancel_request(request_id, timeouts).await;
                return Err(LspSemanticRequestError::TimedOut);
            };
            if message.id != Some(json!(request_id)) {
                continue;
            }
            if let Some(error) = message.error {
                return Err(LspSemanticRequestError::Remote {
                    code: error.code,
                    message: error.message,
                });
            }
            let result = message.result.unwrap_or(Value::Null);
            return serde_json::from_value(result).map_err(|error| {
                LspSemanticRequestError::InvalidResponse {
                    class: error.to_string(),
                }
            });
        }
    }

    async fn cancel_request(
        &mut self,
        request_id: u64,
        timeouts: LspRefreshTimeouts,
    ) -> Result<()> {
        write_message_with_timeout(
            &mut self.stdin,
            cancel_request_message(request_id),
            timeouts.message_io,
        )
        .await
    }

    pub async fn collect_document_diagnostics(
        &mut self,
        project_root: &Path,
        documents: Vec<LspDocument>,
        timeouts: LspRefreshTimeouts,
    ) -> Result<Vec<CodeDiagnostic>> {
        let mut uri_to_document = BTreeMap::new();
        let mut expected_versions = BTreeMap::new();
        for document in &documents {
            let uri = file_uri(&project_root.join(&document.relative_path));
            uri_to_document.insert(uri.clone(), document.clone());
            let next_version = self.document_versions.get(&uri).copied().unwrap_or(0) + 1;
            if next_version == 1 {
                write_message_with_timeout(
                    &mut self.stdin,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/didOpen",
                        "params": {
                            "textDocument": {
                                "uri": uri,
                                "languageId": document.language_id,
                                "version": next_version,
                                "text": document.text,
                            }
                        }
                    }),
                    timeouts.message_io,
                )
                .await?;
            }
            let change_version = next_version + 1;
            write_message_with_timeout(
                &mut self.stdin,
                json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": {
                            "uri": uri,
                            "version": change_version
                        },
                        "contentChanges": [{
                            "text": document.text,
                        }]
                    }
                }),
                timeouts.message_io,
            )
            .await?;
            self.document_versions.insert(uri.clone(), change_version);
            expected_versions.insert(uri, change_version);
        }

        if uri_to_document.is_empty() {
            return Ok(Vec::new());
        }
        let mut diagnostics_by_uri: BTreeMap<String, Vec<CodeDiagnostic>> = BTreeMap::new();
        let refresh_deadline = tokio::time::Instant::now() + timeouts.refresh;
        let mut quiet_deadline = None;
        loop {
            let now = tokio::time::Instant::now();
            let deadline = quiet_deadline
                .map_or(refresh_deadline, |deadline: tokio::time::Instant| {
                    deadline.min(refresh_deadline)
                });
            if now >= deadline {
                break;
            }
            let Some(message) = read_message_until(&mut self.reader, deadline, timeouts).await?
            else {
                break;
            };
            if message.method.as_deref() != Some("textDocument/publishDiagnostics") {
                continue;
            }
            let Some(params) = message.params else {
                continue;
            };
            let Ok(published) = serde_json::from_value::<PublishDiagnosticsParams>(params) else {
                continue;
            };
            if !is_current_diagnostic_publication(&published, &expected_versions) {
                continue;
            }
            let Some(document) = uri_to_document.get(published.uri.as_str()) else {
                continue;
            };
            diagnostics_by_uri.insert(
                published.uri.as_str().to_owned(),
                published
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| code_diagnostic(diagnostic, document, &self.command))
                    .collect(),
            );
            quiet_deadline = Some(tokio::time::Instant::now() + timeouts.diagnostics_quiet);
        }
        // Servers that suppress empty publishes (only publishing for files WITH
        // problems) never emit for clean files, so a fully-reported batch still
        // looks partial here. Requiring every requested URI throws away the real
        // diagnostics of the one file that did report. Only treat the batch as a
        // genuine timeout when nothing arrived at all.
        if diagnostics_by_uri.is_empty() && !uri_to_document.is_empty() {
            return Err(refresh_timed_out(timeouts));
        }
        Ok(diagnostics_by_uri.into_values().flatten().collect())
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

impl Drop for StdioLspClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.stderr_task.abort();
    }
}

fn spawn_stderr_capture(
    mut stderr: tokio::process::ChildStderr,
    capture: Arc<Mutex<Vec<u8>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer = [0_u8; 1024];
        while let Ok(bytes_read) = stderr.read(&mut buffer).await {
            if bytes_read == 0 {
                break;
            }
            let mut captured = capture.lock().await;
            let remaining = 8192_usize.saturating_sub(captured.len());
            if remaining > 0 {
                captured.extend_from_slice(&buffer[..bytes_read.min(remaining)]);
            }
        }
    })
}

async fn captured_stderr(capture: &Arc<Mutex<Vec<u8>>>) -> String {
    let captured = capture.lock().await;
    String::from_utf8_lossy(&captured).trim().to_string()
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
    reader: &mut AsyncContentLengthReader<tokio::process::ChildStdout>,
    deadline: tokio::time::Instant,
    command: &str,
    timeout: Duration,
) -> Result<()> {
    loop {
        let frame = match reader.read_frame_until(deadline).await {
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
        if message.id == Some(json!(1)) {
            return Ok(());
        }
    }
}

async fn write_message_with_timeout(
    stdin: &mut tokio::process::ChildStdin,
    value: Value,
    timeout: Duration,
) -> Result<()> {
    let body = serde_json::to_vec(&value).map_err(|e| TraceDecayError::Config {
        message: format!("failed to encode LSP message: {e}"),
    })?;
    write_content_length_frame_until(stdin, &body, tokio::time::Instant::now() + timeout)
        .await
        .map_err(|error| match error {
            AsyncContentLengthError::DeadlineElapsed => TraceDecayError::Config {
                message: format!(
                    "LSP message write timed out after {} ms",
                    timeout.as_millis()
                ),
            },
            error => frame_write_error(error),
        })
}

fn refresh_timed_out(timeouts: LspRefreshTimeouts) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "LSP diagnostics collection timed out after {} ms",
            timeouts.refresh.as_millis()
        ),
    }
}

fn semantic_transport_error(error: TraceDecayError) -> LspSemanticRequestError {
    LspSemanticRequestError::Transport {
        class: error.to_string(),
    }
}

fn cancel_request_message(request_id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "$/cancelRequest",
        "params": { "id": request_id },
    })
}

async fn read_message_until(
    reader: &mut AsyncContentLengthReader<tokio::process::ChildStdout>,
    deadline: tokio::time::Instant,
    timeouts: LspRefreshTimeouts,
) -> Result<Option<JsonRpcMessage>> {
    match reader.read_frame_until(deadline).await {
        Ok(FramePoll::Frame(frame)) => decode_message(&frame).map(Some),
        Ok(FramePoll::Pending | FramePoll::Closed) => Ok(None),
        Err(AsyncContentLengthError::DeadlineElapsed) => Err(refresh_timed_out(timeouts)),
        Err(error) => Err(frame_read_error(error)),
    }
}

fn decode_message(body: &[u8]) -> Result<JsonRpcMessage> {
    serde_json::from_slice(body).map_err(|e| TraceDecayError::Config {
        message: format!("failed to parse LSP message: {e}"),
    })
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

fn file_uri(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    file_uri_from_path_text(&absolute.to_string_lossy())
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
    let normalized = path.replace('\\', "/");
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

#[derive(Debug, Deserialize)]
struct JsonRpcMessage {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: Option<i64>,
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
        updated_at: now_unix(),
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
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
        lsp_initialization_options,
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
    fn rust_analyzer_initialization_disables_competing_cargo_flycheck() {
        assert_eq!(
            lsp_initialization_options("/toolchains/stable/bin/rust-analyzer"),
            json!({ "checkOnSave": false })
        );
        assert_eq!(lsp_initialization_options("clangd"), json!({}));
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
