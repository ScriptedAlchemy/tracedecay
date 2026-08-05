//! Typed JSON-RPC ingress parsing and LSP method routing.

use serde_json::Value;

use crate::context::{
    TRACEDECAY_CONTEXT_EXPAND_METHOD, TRACEDECAY_CONTEXT_METHOD, TRACEDECAY_SUBSCRIBE_METHOD,
};
use crate::diagnostics::LspPosition;
use crate::gateway::{FeedbackCyclePort, SemanticProviderPort, SemanticRequest};
use crate::protocol::{DaemonLspProtocolSession, TRACEDECAY_NATIVE_DIAGNOSTICS_METHOD};
use crate::provider::DiagnosticSnapshotPort;
use crate::rpc::{
    RpcFailure, deferred_method_reason, document_position, document_uri, error_response,
    parse_call_item, parse_type_item, request_id,
};
use crate::session::LspRequestId;

/// Known client-originated LSP methods handled by the daemon gateway.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LspClientMethod {
    Initialize,
    Initialized,
    Shutdown,
    Exit,
    CancelRequest,
    TextDocumentDidOpen,
    TextDocumentDidChange,
    TextDocumentDidClose,
    TextDocumentDidSave,
    WorkspaceDiagnostic,
    TextDocumentDiagnostic,
    TextDocumentDeclaration,
    TextDocumentDefinition,
    TextDocumentTypeDefinition,
    TextDocumentImplementation,
    TextDocumentReferences,
    TextDocumentHover,
    TextDocumentDocumentSymbol,
    WorkspaceSymbol,
    TextDocumentPrepareCallHierarchy,
    CallHierarchyIncomingCalls,
    CallHierarchyOutgoingCalls,
    TextDocumentSignatureHelp,
    TextDocumentPrepareTypeHierarchy,
    TypeHierarchySupertypes,
    TypeHierarchySubtypes,
    TraceDecayContext,
    TraceDecayContextExpand,
    TraceDecaySubscribe,
    TraceDecayNativeDiagnostics,
    Unknown(String),
}

impl LspClientMethod {
    fn parse(method: &str) -> Self {
        match method {
            "initialize" => Self::Initialize,
            "initialized" => Self::Initialized,
            "shutdown" => Self::Shutdown,
            "exit" => Self::Exit,
            "$/cancelRequest" => Self::CancelRequest,
            "textDocument/didOpen" => Self::TextDocumentDidOpen,
            "textDocument/didChange" => Self::TextDocumentDidChange,
            "textDocument/didClose" => Self::TextDocumentDidClose,
            "textDocument/didSave" => Self::TextDocumentDidSave,
            "workspace/diagnostic" => Self::WorkspaceDiagnostic,
            "textDocument/diagnostic" => Self::TextDocumentDiagnostic,
            "textDocument/declaration" => Self::TextDocumentDeclaration,
            "textDocument/definition" => Self::TextDocumentDefinition,
            "textDocument/typeDefinition" => Self::TextDocumentTypeDefinition,
            "textDocument/implementation" => Self::TextDocumentImplementation,
            "textDocument/references" => Self::TextDocumentReferences,
            "textDocument/hover" => Self::TextDocumentHover,
            "textDocument/documentSymbol" => Self::TextDocumentDocumentSymbol,
            "workspace/symbol" => Self::WorkspaceSymbol,
            "textDocument/prepareCallHierarchy" => Self::TextDocumentPrepareCallHierarchy,
            "callHierarchy/incomingCalls" => Self::CallHierarchyIncomingCalls,
            "callHierarchy/outgoingCalls" => Self::CallHierarchyOutgoingCalls,
            "textDocument/signatureHelp" => Self::TextDocumentSignatureHelp,
            "textDocument/prepareTypeHierarchy" => Self::TextDocumentPrepareTypeHierarchy,
            "typeHierarchy/supertypes" => Self::TypeHierarchySupertypes,
            "typeHierarchy/subtypes" => Self::TypeHierarchySubtypes,
            TRACEDECAY_CONTEXT_METHOD => Self::TraceDecayContext,
            TRACEDECAY_CONTEXT_EXPAND_METHOD => Self::TraceDecayContextExpand,
            TRACEDECAY_SUBSCRIBE_METHOD => Self::TraceDecaySubscribe,
            TRACEDECAY_NATIVE_DIAGNOSTICS_METHOD => Self::TraceDecayNativeDiagnostics,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

/// Parsed client ingress after JSON-RPC envelope validation.
#[derive(Clone, Debug)]
pub(crate) enum ParsedIncoming {
    Request {
        response_id: Value,
        method: LspClientMethod,
        params: Value,
    },
    Notification {
        method: LspClientMethod,
        params: Value,
    },
    ClientResponse {
        id: LspRequestId,
        succeeded: bool,
    },
}

/// Validates a decoded JSON-RPC value and classifies it for routing.
pub(crate) fn parse_incoming(value: Value) -> Result<ParsedIncoming, (Value, RpcFailure)> {
    let Some(object) = value.as_object() else {
        return Err((
            Value::Null,
            RpcFailure {
                code: -32600,
                message: "Invalid Request",
                data: Value::Null,
            },
        ));
    };
    let id = object.get("id").cloned();
    let response_id = match id.as_ref() {
        Some(value) if request_id(value).is_none() => {
            return Err((
                Value::Null,
                RpcFailure {
                    code: -32600,
                    message: "Invalid Request",
                    data: serde_json::json!({ "detail": "request id must be an integer or string" }),
                },
            ));
        }
        Some(value) => value.clone(),
        None => Value::Null,
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err((
            response_id,
            RpcFailure {
                code: -32600,
                message: "Invalid Request",
                data: serde_json::json!({ "detail": "jsonrpc must equal 2.0" }),
            },
        ));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        if let Some(id) = id.as_ref().and_then(request_id) {
            let has_result = object.contains_key("result");
            let has_error = object.contains_key("error");
            if has_result ^ has_error {
                return Ok(ParsedIncoming::ClientResponse {
                    id,
                    succeeded: has_result,
                });
            }
        }
        return Err((
            response_id,
            RpcFailure {
                code: -32600,
                message: "Invalid Request",
                data: serde_json::json!({ "detail": "method is required" }),
            },
        ));
    };
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    let parsed_method = LspClientMethod::parse(method);
    if id.is_some() {
        Ok(ParsedIncoming::Request {
            response_id,
            method: parsed_method,
            params,
        })
    } else {
        Ok(ParsedIncoming::Notification {
            method: parsed_method,
            params,
        })
    }
}

pub(crate) fn dispatch_incoming<P, S, D>(
    session: &mut DaemonLspProtocolSession<P, S, D>,
    incoming: ParsedIncoming,
    now_ms: u64,
) where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    match incoming {
        ParsedIncoming::ClientResponse { id, succeeded } => {
            session.handle_client_response(id, succeeded)
        }
        ParsedIncoming::Notification { method, params } => {
            dispatch_notification(session, method, params, now_ms);
        }
        ParsedIncoming::Request {
            response_id,
            method,
            params,
        } => dispatch_request(session, response_id, method, params, now_ms),
    }
}

fn dispatch_notification<P, S, D>(
    session: &mut DaemonLspProtocolSession<P, S, D>,
    method: LspClientMethod,
    params: Value,
    now_ms: u64,
) where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    match method {
        LspClientMethod::Initialized => session.handle_initialized_notification(),
        LspClientMethod::Exit => session.handle_exit_notification(),
        LspClientMethod::CancelRequest => session.handle_cancel(&params),
        LspClientMethod::TextDocumentDidOpen => {
            let _ = session.handle_did_open(&params, now_ms);
        }
        LspClientMethod::TextDocumentDidChange => {
            let _ = session.handle_did_change(&params, now_ms);
        }
        LspClientMethod::TextDocumentDidClose => {
            let _ = session.handle_did_close(&params, now_ms);
        }
        LspClientMethod::TextDocumentDidSave => {
            let _ = session.handle_did_save(&params, now_ms);
        }
        LspClientMethod::TraceDecayNativeDiagnostics => {
            session.handle_native_diagnostics_notification(&params, now_ms);
        }
        _ => {}
    }
}

fn dispatch_request<P, S, D>(
    session: &mut DaemonLspProtocolSession<P, S, D>,
    response_id: Value,
    method: LspClientMethod,
    params: Value,
    now_ms: u64,
) where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    match method {
        LspClientMethod::Initialize => session.handle_initialize(response_id, &params),
        LspClientMethod::Initialized => session.handle_initialized_request(response_id),
        LspClientMethod::Shutdown => session.handle_shutdown_request(response_id),
        LspClientMethod::Exit => session.handle_exit_request(response_id),
        LspClientMethod::TextDocumentDiagnostic => match document_uri(&params) {
            Ok(uri) => {
                let version = session.document_version(&uri);
                session.with_request(
                    response_id,
                    Some((uri.clone(), version)),
                    now_ms,
                    move |session| session.pull_diagnostics(&uri, &params),
                );
            }
            Err(error) => {
                let _ = session.enqueue_value(error_response(response_id, error));
            }
        },
        LspClientMethod::WorkspaceDiagnostic => {
            session.with_request(response_id, None, now_ms, move |session| {
                session.pull_workspace_diagnostics(&params)
            });
        }
        LspClientMethod::TextDocumentDeclaration => {
            start_position_semantic(
                session,
                response_id,
                &params,
                now_ms,
                |document_uri, position| SemanticRequest::Declaration {
                    document_uri,
                    position,
                },
            );
        }
        LspClientMethod::TextDocumentDefinition => {
            start_position_semantic(
                session,
                response_id,
                &params,
                now_ms,
                |document_uri, position| SemanticRequest::Definition {
                    document_uri,
                    position,
                },
            );
        }
        LspClientMethod::TextDocumentTypeDefinition => {
            start_position_semantic(
                session,
                response_id,
                &params,
                now_ms,
                |document_uri, position| SemanticRequest::TypeDefinition {
                    document_uri,
                    position,
                },
            );
        }
        LspClientMethod::TextDocumentImplementation => {
            start_position_semantic(
                session,
                response_id,
                &params,
                now_ms,
                |document_uri, position| SemanticRequest::Implementation {
                    document_uri,
                    position,
                },
            );
        }
        LspClientMethod::TextDocumentReferences => {
            start_position_semantic(
                session,
                response_id,
                &params,
                now_ms,
                |document_uri, position| SemanticRequest::References {
                    document_uri,
                    position,
                },
            );
        }
        LspClientMethod::TextDocumentHover => {
            start_position_semantic(
                session,
                response_id,
                &params,
                now_ms,
                |document_uri, position| SemanticRequest::Hover {
                    document_uri,
                    position,
                },
            );
        }
        LspClientMethod::TextDocumentDocumentSymbol => match document_uri(&params) {
            Ok(document_uri) => {
                let version = session.document_version(&document_uri);
                session.start_semantic_request(
                    response_id,
                    Some((document_uri.clone(), version)),
                    SemanticRequest::DocumentSymbols { document_uri },
                    now_ms,
                );
            }
            Err(error) => {
                let _ = session.enqueue_value(error_response(response_id, error));
            }
        },
        LspClientMethod::WorkspaceSymbol => {
            let query = params
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            session.start_semantic_request(
                response_id,
                None,
                SemanticRequest::WorkspaceSymbols { query },
                now_ms,
            );
        }
        LspClientMethod::TextDocumentPrepareCallHierarchy => {
            start_position_semantic(
                session,
                response_id,
                &params,
                now_ms,
                |document_uri, position| SemanticRequest::PrepareCallHierarchy {
                    document_uri,
                    position,
                },
            );
        }
        LspClientMethod::CallHierarchyIncomingCalls => {
            start_call_semantic(session, response_id, &params, now_ms, true);
        }
        LspClientMethod::CallHierarchyOutgoingCalls => {
            start_call_semantic(session, response_id, &params, now_ms, false);
        }
        LspClientMethod::TextDocumentSignatureHelp => {
            start_position_semantic(
                session,
                response_id,
                &params,
                now_ms,
                |document_uri, position| SemanticRequest::SignatureHelp {
                    document_uri,
                    position,
                },
            );
        }
        LspClientMethod::TextDocumentPrepareTypeHierarchy => {
            start_position_semantic(
                session,
                response_id,
                &params,
                now_ms,
                |document_uri, position| SemanticRequest::PrepareTypeHierarchy {
                    document_uri,
                    position,
                },
            );
        }
        LspClientMethod::TypeHierarchySupertypes => {
            start_type_semantic(session, response_id, &params, now_ms, true);
        }
        LspClientMethod::TypeHierarchySubtypes => {
            start_type_semantic(session, response_id, &params, now_ms, false);
        }
        LspClientMethod::TraceDecayContext => {
            session.handle_context_request(response_id, &params, now_ms);
        }
        LspClientMethod::TraceDecayContextExpand => {
            session.handle_context_expand_request(response_id, &params, now_ms);
        }
        LspClientMethod::TraceDecaySubscribe => {
            session.handle_context_subscribe(response_id, &params, now_ms);
        }
        LspClientMethod::TraceDecayNativeDiagnostics => {
            let _ = session.enqueue_value(error_response(
                response_id,
                RpcFailure::invalid_params("tracedecay/nativeDiagnostics must be a notification"),
            ));
        }
        LspClientMethod::CancelRequest
        | LspClientMethod::TextDocumentDidOpen
        | LspClientMethod::TextDocumentDidChange
        | LspClientMethod::TextDocumentDidClose
        | LspClientMethod::TextDocumentDidSave => {
            let _ = session.enqueue_value(error_response(
                response_id,
                RpcFailure {
                    code: -32600,
                    message: "Invalid Request",
                    data: serde_json::json!({
                        "detail": "client lifecycle and cancellation methods must be notifications",
                    }),
                },
            ));
        }
        LspClientMethod::Unknown(method) => {
            let _ = session.enqueue_value(error_response(
                response_id,
                RpcFailure::unavailable(&method, deferred_method_reason(&method)),
            ));
        }
    }
}

fn start_position_semantic<P, S, D>(
    session: &mut DaemonLspProtocolSession<P, S, D>,
    response_id: Value,
    params: &Value,
    now_ms: u64,
    request: impl FnOnce(String, LspPosition) -> SemanticRequest,
) where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    match document_position(params) {
        Ok((document_uri, position)) => {
            let version = session.document_version(&document_uri);
            session.start_semantic_request(
                response_id,
                Some((document_uri.clone(), version)),
                request(document_uri, position),
                now_ms,
            );
        }
        Err(error) => {
            let _ = session.enqueue_value(error_response(response_id, error));
        }
    }
}

fn start_call_semantic<P, S, D>(
    session: &mut DaemonLspProtocolSession<P, S, D>,
    response_id: Value,
    params: &Value,
    now_ms: u64,
    incoming: bool,
) where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    let item = params
        .get("item")
        .ok_or_else(|| RpcFailure::invalid_params("item is required"))
        .and_then(parse_call_item);
    match item {
        Ok(item) => {
            let document_uri = item.uri.clone();
            let version = session.document_version(&document_uri);
            let request = if incoming {
                SemanticRequest::IncomingCalls { item }
            } else {
                SemanticRequest::OutgoingCalls { item }
            };
            session.start_semantic_request(
                response_id,
                Some((document_uri, version)),
                request,
                now_ms,
            );
        }
        Err(error) => {
            let _ = session.enqueue_value(error_response(response_id, error));
        }
    }
}

fn start_type_semantic<P, S, D>(
    session: &mut DaemonLspProtocolSession<P, S, D>,
    response_id: Value,
    params: &Value,
    now_ms: u64,
    supertypes: bool,
) where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    let item = params
        .get("item")
        .ok_or_else(|| RpcFailure::invalid_params("item is required"))
        .and_then(parse_type_item);
    match item {
        Ok(item) => {
            let document_uri = item.uri.clone();
            let version = session.document_version(&document_uri);
            let request = if supertypes {
                SemanticRequest::TypeHierarchySupertypes { item }
            } else {
                SemanticRequest::TypeHierarchySubtypes { item }
            };
            session.start_semantic_request(
                response_id,
                Some((document_uri, version)),
                request,
                now_ms,
            );
        }
        Err(error) => {
            let _ = session.enqueue_value(error_response(response_id, error));
        }
    }
}
