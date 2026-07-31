use super::{
    AnalyzerCancellationPort, Arc, BTreeMap, CompletionDisposition, DaemonLspProtocolSession,
    DiagnosticSnapshotPort, FeedbackCyclePort, GatewayResponse, LspRequestFailure, LspRequestId,
    RpcFailure, SemanticProviderPort, SemanticRequest, Value, error_response, json,
    partial_failure_data, request_id, semantic_response_value, success_response,
};

#[derive(Clone)]
pub(super) struct PendingSemanticRequest {
    pub(super) response_id: Value,
    pub(super) request: SemanticRequest,
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
        match self.lifecycle.control.admit_request_with_deadline(
            request_id.clone(),
            document,
            Some(deadline),
        ) {
            crate::session::RequestAdmission::Accepted => {
                let result = route(self);
                let completion = self.lifecycle.control.complete_request(&request_id);
                if let Some(failure) = completion.failure() {
                    let _ = self
                        .enqueue_value(error_response(id, RpcFailure::request_failure(failure)));
                } else if completion == CompletionDisposition::Publish {
                    match result {
                        Ok(value) => {
                            let _ = self.enqueue_value(success_response(id, value));
                        }
                        Err(error) => {
                            let _ = self.enqueue_value(error_response(id, error));
                        }
                    }
                }
            }
            crate::session::RequestAdmission::DuplicateId => {
                let _ = self.enqueue_value(error_response(
                    id,
                    RpcFailure {
                        code: -32600,
                        message: "Invalid Request",
                        data: json!({ "detail": "duplicate request id" }),
                    },
                ));
            }
            crate::session::RequestAdmission::SessionUnavailable => {
                let _ = self.enqueue_value(error_response(
                    id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request: true,
                    }),
                ));
            }
            crate::session::RequestAdmission::Saturated { retrigger_request } => {
                let _ = self.enqueue_value(error_response(
                    id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request,
                    }),
                ));
            }
        }
    }

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
        match self.lifecycle.control.admit_request_with_deadline(
            request_id.clone(),
            document,
            Some(deadline),
        ) {
            crate::session::RequestAdmission::Accepted => {
                match self.semantic_request_value(&request_id, &request) {
                    Ok(None) => {
                        self.semantic.pending.insert(
                            request_id,
                            PendingSemanticRequest {
                                response_id,
                                request,
                            },
                        );
                    }
                    result => self.complete_semantic_request(request_id, response_id, result),
                }
            }
            crate::session::RequestAdmission::DuplicateId => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure {
                        code: -32600,
                        message: "Invalid Request",
                        data: json!({ "detail": "duplicate request id" }),
                    },
                ));
            }
            crate::session::RequestAdmission::SessionUnavailable => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request: true,
                    }),
                ));
            }
            crate::session::RequestAdmission::Saturated { retrigger_request } => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request,
                    }),
                ));
            }
        }
    }

    pub(super) fn semantic_request_value(
        &self,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> Result<Option<Value>, RpcFailure> {
        match self.lifecycle.gateway.semantic_request(request_id, request) {
            GatewayResponse::Value(value) => Ok(Some(semantic_response_value(value))),
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

    pub(super) fn complete_semantic_request(
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

    pub(super) fn poll_semantic_requests(&mut self) {
        let request_ids = self.semantic.pending.keys().cloned().collect::<Vec<_>>();
        for request_id in request_ids {
            let Some(pending) = self.semantic.pending.get(&request_id).cloned() else {
                continue;
            };
            let result = self.semantic_request_value(&request_id, &pending.request);
            if matches!(result, Ok(None)) {
                continue;
            }
            self.semantic.pending.remove(&request_id);
            self.complete_semantic_request(request_id, pending.response_id, result);
        }
    }
}

#[cfg(test)]
mod controller_tests {
    use super::{SemanticRequest, Value};
    use crate::diagnostics::LspPosition;

    #[test]
    fn semantic_controller_projects_provider_result() {
        let mut session = super::super::tests::session();
        super::super::tests::initialize(&mut session);
        session.start_semantic_request(
            Value::from("semantic-controller"),
            Some(("file:///root/a.rs".to_owned(), 0)),
            SemanticRequest::Definition {
                document_uri: "file:///root/a.rs".to_owned(),
                position: LspPosition {
                    line: 0,
                    character: 0,
                },
            },
            2,
        );

        let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
        assert_eq!(response["id"], "semantic-controller");
        assert_eq!(response["result"][0]["uri"], "file:///root/a.rs");
    }
}
