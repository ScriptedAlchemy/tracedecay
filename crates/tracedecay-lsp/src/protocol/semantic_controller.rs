use std::time::{Duration, Instant};

use crate::gateway::{AdmittedRoot, SemanticResponse, WorkspaceSymbol};
use crate::session::RequestAdmission;

use super::{
    AnalyzerCancellationPort, Arc, BTreeMap, CompletionDisposition, DaemonLspProtocolSession,
    DiagnosticSnapshotPort, FeedbackCyclePort, GatewayResponse, LspRequestFailure, LspRequestId,
    LspSemanticDocumentSnapshot, RpcFailure, SemanticProviderPort, SemanticRequest, Value,
    error_response, json, partial_failure_data, request_id, semantic_response_value,
    success_response,
};

pub(super) struct PendingSemanticRequest {
    pub(super) response_id: Value,
    pub(super) request: SemanticRequest,
    pub(super) document_snapshot: Option<LspSemanticDocumentSnapshot>,
    pub(super) deadline: Instant,
    /// Exact roots captured at admission, retained across workspace changes
    /// for cancellation and for root-by-root completion of workspace symbols.
    pub(super) roots: Vec<AdmittedRoot>,
    workspace_symbols: BTreeMap<String, Vec<WorkspaceSymbol>>,
}

#[derive(Default)]
pub(super) struct SemanticController {
    pub(super) pending: BTreeMap<LspRequestId, PendingSemanticRequest>,
    pub(super) cancellation: Option<Arc<dyn AnalyzerCancellationPort + Send + Sync>>,
}

impl<P, S, D> DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    #[tracing::instrument(name = "lsp_session_with_request", level = "trace", skip_all)]
    pub(crate) fn with_request(
        &mut self,
        id: Value,
        document: Option<(String, i64)>,
        now_ms: u64,
        route: impl FnOnce(&mut Self) -> Result<Value, RpcFailure>,
    ) {
        let Some(request_id) = request_id(&id) else {
            let _ = self.enqueue_value(error_response(
                Value::Null,
                RpcFailure {
                    code: -32600,
                    message: "Invalid Request",
                    data: json!({ "detail": "request id must be an integer or string" }),
                },
            ));
            return;
        };
        let deadline = now_ms.saturating_add(self.lifecycle.request_deadline_ms);
        let admission = self.lifecycle.control.admit_request_with_deadline(
            request_id.clone(),
            document,
            Some(deadline),
        );
        if let Some(failure) = admission_refusal(admission) {
            let _ = self.enqueue_value(error_response(id, failure));
            return;
        }
        let result = route(self);
        self.finish_admitted_request(request_id, id, result.map(Some));
    }

    #[tracing::instrument(name = "lsp_session_semantic_request", level = "trace", skip_all)]
    pub(crate) fn start_semantic_request(
        &mut self,
        response_id: Value,
        document: Option<(String, i64)>,
        request: SemanticRequest,
        now_ms: u64,
    ) {
        let Some(request_id) = request_id(&response_id) else {
            let _ = self.enqueue_value(error_response(
                Value::Null,
                RpcFailure::invalid_params("semantic request id must be an integer or string"),
            ));
            return;
        };
        let deadline = now_ms.saturating_add(self.lifecycle.request_deadline_ms);
        let operation_deadline =
            Instant::now() + Duration::from_millis(self.lifecycle.request_deadline_ms);
        let admission = self.lifecycle.control.admit_request_with_deadline(
            request_id.clone(),
            document,
            Some(deadline),
        );
        if let Some(failure) = admission_refusal(admission) {
            let _ = self.enqueue_value(error_response(response_id, failure));
            return;
        }
        let document_snapshot = request
            .document_uri()
            .and_then(|uri| self.lifecycle.overlays.snapshot(uri))
            .map(|snapshot| LspSemanticDocumentSnapshot::from_overlay(&snapshot));
        let roots = if matches!(request, SemanticRequest::WorkspaceSymbols { .. }) {
            self.lifecycle.gateway.workspace().roots().to_vec()
        } else {
            request
                .document_uri()
                .and_then(|uri| self.lifecycle.gateway.root_for_document(uri).ok())
                .cloned()
                .into_iter()
                .collect()
        };
        let mut pending = PendingSemanticRequest {
            response_id,
            request,
            document_snapshot,
            deadline: operation_deadline,
            roots,
            workspace_symbols: BTreeMap::new(),
        };
        match self.semantic_request_value(&request_id, &mut pending) {
            Ok(None) => {
                self.semantic.pending.insert(request_id, pending);
            }
            result => {
                if result.is_err() {
                    self.cancel_semantic_upstream(&request_id, &pending);
                }
                self.finish_admitted_request(request_id, pending.response_id, result);
            }
        }
    }

    pub(super) fn semantic_request_value(
        &self,
        request_id: &LspRequestId,
        pending: &mut PendingSemanticRequest,
    ) -> Result<Option<Value>, RpcFailure> {
        if Instant::now() >= pending.deadline {
            return Err(RpcFailure::request_failure(
                LspRequestFailure::ServerCancelled {
                    retrigger_request: true,
                },
            ));
        }
        if let SemanticRequest::WorkspaceSymbols { query } = &pending.request {
            for root in &pending.roots {
                if pending.workspace_symbols.contains_key(root.uri()) {
                    continue;
                }
                match semantic_result(self.lifecycle.gateway.workspace_symbols_for_root(
                    root,
                    request_id,
                    query,
                    Some(pending.deadline),
                ))? {
                    Some(SemanticResponse::WorkspaceSymbols(symbols)) => {
                        pending
                            .workspace_symbols
                            .insert(root.uri().to_owned(), symbols);
                    }
                    Some(_) => {
                        return Err(RpcFailure {
                            code: -32802,
                            message: "Server cancelled request",
                            data: partial_failure_data(
                                "workspace-symbol-response-invalid".to_owned(),
                                None,
                            ),
                        });
                    }
                    None => {}
                }
            }
            if pending.workspace_symbols.len() != pending.roots.len() {
                return Ok(None);
            }
            let symbols = pending
                .roots
                .iter()
                .flat_map(|root| {
                    pending
                        .workspace_symbols
                        .remove(root.uri())
                        .into_iter()
                        .flatten()
                })
                .collect();
            return Ok(Some(semantic_response_value(
                SemanticResponse::WorkspaceSymbols(symbols),
            )));
        }
        semantic_result(self.lifecycle.gateway.semantic_request_with_document(
            request_id,
            &pending.request,
            pending.document_snapshot.as_ref(),
            Some(pending.deadline),
        ))
        .map(|response| response.map(semantic_response_value))
    }

    pub(super) fn cancel_semantic_upstream(
        &self,
        request_id: &LspRequestId,
        pending: &PendingSemanticRequest,
    ) {
        if let Some(cancellation) = &self.semantic.cancellation {
            for root in &pending.roots {
                let _ = cancellation.cancel_upstream(root, request_id);
            }
        }
    }

    pub(super) fn poll_semantic_requests(&mut self) {
        let request_ids = self.semantic.pending.keys().cloned().collect::<Vec<_>>();
        for request_id in request_ids {
            let Some(mut pending) = self.semantic.pending.remove(&request_id) else {
                continue;
            };
            let result = self.semantic_request_value(&request_id, &mut pending);
            if matches!(result, Ok(None)) {
                self.semantic.pending.insert(request_id, pending);
                continue;
            }
            if result.is_err() {
                self.cancel_semantic_upstream(&request_id, &pending);
            }
            self.finish_admitted_request(request_id, pending.response_id, result);
        }
    }

    pub(super) fn finish_admitted_request(
        &mut self,
        request_id: LspRequestId,
        response_id: Value,
        result: Result<Option<Value>, RpcFailure>,
    ) {
        let completion = self.lifecycle.control.complete_request(&request_id);
        if let Some(failure) = completion.failure() {
            let _ = self.enqueue_value(error_response(
                response_id,
                RpcFailure::request_failure(failure),
            ));
        } else if completion == CompletionDisposition::Publish {
            match result {
                Ok(Some(value)) => {
                    let _ = self.enqueue_value(success_response(response_id, value));
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = self.enqueue_value(error_response(response_id, error));
                }
            }
        }
    }
}

fn semantic_result(
    response: GatewayResponse<SemanticResponse>,
) -> Result<Option<SemanticResponse>, RpcFailure> {
    match response {
        GatewayResponse::Value(value) => Ok(Some(value)),
        GatewayResponse::Partial {
            coverage, detail, ..
        } => Err(RpcFailure {
            code: -32802,
            message: "Server cancelled request",
            data: partial_failure_data(coverage, detail),
        }),
        GatewayResponse::Pending => Ok(None),
        GatewayResponse::Unavailable(unavailable) => Err(RpcFailure::unavailable(
            unavailable.method.as_lsp_method(),
            unavailable.reason,
        )),
        GatewayResponse::RequestFailed(failure) => Err(RpcFailure::request_failure(failure)),
    }
}

pub(super) fn admission_refusal(admission: RequestAdmission) -> Option<RpcFailure> {
    match admission {
        RequestAdmission::Accepted => None,
        RequestAdmission::DuplicateId => Some(RpcFailure {
            code: -32600,
            message: "Invalid Request",
            data: json!({ "detail": "duplicate request id" }),
        }),
        RequestAdmission::SessionUnavailable => Some(RpcFailure::request_failure(
            LspRequestFailure::ServerCancelled {
                retrigger_request: true,
            },
        )),
        RequestAdmission::Saturated { retrigger_request } => Some(RpcFailure::request_failure(
            LspRequestFailure::ServerCancelled { retrigger_request },
        )),
    }
}
