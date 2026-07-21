//! Typed JSON-RPC ingress parsing and LSP method routing.

use serde_json::Value;

use super::gateway::FeedbackCyclePort;
use super::provider::DiagnosticSnapshotPort;
use super::protocol::DaemonLspProtocolSession;
use super::rpc::{
    RpcFailure, call_items_value, deferred_method_reason, document_uri, error_response,
    hover_value, incoming_calls_value, locations_value, outgoing_calls_value, request_id,
    response_value, signature_help_value, type_items_value, workspace_symbols_value,
};
use super::session::LspRequestId;
use super::gateway::SemanticProviderPort;
use super::rpc::document_symbols_value;

/// Known client-originated LSP methods handled by the daemon gateway.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum LspClientMethod {
    Initialize,
    Initialized,
    Shutdown,
    Exit,
    CancelRequest,
    TextDocumentDidOpen,
    TextDocumentDidChange,
    TextDocumentDidClose,
    TextDocumentDidSave,
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
            other => Self::Unknown(other.to_owned()),
        }
    }
}

/// Parsed client ingress after JSON-RPC envelope validation.
#[derive(Clone, Debug)]
pub(super) enum ParsedIncoming {
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
    },
}

/// Validates a decoded JSON-RPC value and classifies it for routing.
pub(super) fn parse_incoming(value: Value) -> Result<ParsedIncoming, (Value, RpcFailure)> {
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
        if let Some(id) = id.as_ref().and_then(request_id)
            && (object.contains_key("result") || object.contains_key("error"))
        {
            return Ok(ParsedIncoming::ClientResponse { id });
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

pub(super) fn dispatch_incoming<P, S, D>(
    session: &mut DaemonLspProtocolSession<P, S, D>,
    incoming: ParsedIncoming,
    now_ms: u64,
) where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    match incoming {
        ParsedIncoming::ClientResponse { id } => session.handle_client_response(id),
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
        LspClientMethod::TextDocumentDiagnostic => {
            match document_uri(&params) {
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
            }
        }
        LspClientMethod::TextDocumentDeclaration => {
            session.handle_position_request(response_id, &params, now_ms, |gateway, uri, position| {
                response_value(gateway.declaration(uri, position), locations_value)
            });
        }
        LspClientMethod::TextDocumentDefinition => {
            session.handle_position_request(response_id, &params, now_ms, |gateway, uri, position| {
                response_value(gateway.definition(uri, position), locations_value)
            });
        }
        LspClientMethod::TextDocumentTypeDefinition => {
            session.handle_position_request(response_id, &params, now_ms, |gateway, uri, position| {
                response_value(gateway.type_definition(uri, position), locations_value)
            });
        }
        LspClientMethod::TextDocumentImplementation => {
            session.handle_position_request(response_id, &params, now_ms, |gateway, uri, position| {
                response_value(gateway.implementation(uri, position), locations_value)
            });
        }
        LspClientMethod::TextDocumentReferences => {
            session.handle_position_request(response_id, &params, now_ms, |gateway, uri, position| {
                response_value(gateway.references(uri, position), locations_value)
            });
        }
        LspClientMethod::TextDocumentHover => {
            session.handle_position_request(response_id, &params, now_ms, |gateway, uri, position| {
                response_value(gateway.hover(uri, position), hover_value)
            });
        }
        LspClientMethod::TextDocumentDocumentSymbol => {
            session.handle_document_request(response_id, &params, now_ms, |gateway, uri| {
                response_value(gateway.document_symbols(uri), document_symbols_value)
            });
        }
        LspClientMethod::WorkspaceSymbol => {
            let query = params
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            session.with_request(response_id, None, now_ms, move |session| {
                response_value(
                    session.gateway.workspace_symbols(&query),
                    workspace_symbols_value,
                )
            });
        }
        LspClientMethod::TextDocumentPrepareCallHierarchy => {
            session.handle_position_request(response_id, &params, now_ms, |gateway, uri, position| {
                response_value(
                    gateway.prepare_call_hierarchy(uri, position),
                    call_items_value,
                )
            });
        }
        LspClientMethod::CallHierarchyIncomingCalls => {
            session.handle_call_request(response_id, &params, now_ms, true);
        }
        LspClientMethod::CallHierarchyOutgoingCalls => {
            session.handle_call_request(response_id, &params, now_ms, false);
        }
        LspClientMethod::TextDocumentSignatureHelp => {
            session.handle_position_request(response_id, &params, now_ms, |gateway, uri, position| {
                response_value(gateway.signature_help(uri, position), signature_help_value)
            });
        }
        LspClientMethod::TextDocumentPrepareTypeHierarchy => {
            session.handle_position_request(response_id, &params, now_ms, |gateway, uri, position| {
                response_value(
                    gateway.prepare_type_hierarchy(uri, position),
                    type_items_value,
                )
            });
        }
        LspClientMethod::TypeHierarchySupertypes => {
            session.handle_type_request(response_id, &params, now_ms, true);
        }
        LspClientMethod::TypeHierarchySubtypes => {
            session.handle_type_request(response_id, &params, now_ms, false);
        }
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
        LspClientMethod::Unknown(method) => {
            let _ = session.enqueue_value(error_response(
                response_id,
                RpcFailure::unavailable(&method, deferred_method_reason(&method)),
            ));
        }
    }
}
