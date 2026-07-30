use super::diagnostics_controller::refresh_pending_failure;
use super::*;

fn valid_context_projection_identity(identity: &ContextProjectionIdentity) -> bool {
    CommitId::new(identity.head_commit_id.clone()).is_ok()
        && CodeGenerationId::new(identity.code_generation_id.clone()).is_ok()
        && ManifestDigest::new(identity.snapshot_digest.clone()).is_ok()
        && ManifestDigest::new(identity.invalidation_digest.clone()).is_ok()
        && ContentDigest::new(identity.snapshot_content_digest.clone()).is_ok()
        && identity
            .document_content_digest
            .as_ref()
            .is_none_or(|digest| ContentDigest::new(digest.clone()).is_ok())
}
static NEXT_CONTEXT_OPERATION_ID: ProcessLocalRequestSequence =
    ProcessLocalRequestSequence::starting_at(1);

#[derive(Clone)]
pub(super) struct PendingContextRequest {
    pub(super) response_id: Value,
    pub(super) operation_id: LspRequestId,
    pub(super) request: ContextProjectionRequest,
}

#[derive(Clone)]
pub(super) struct PendingContextExpansion {
    pub(super) response_id: Value,
    pub(super) operation_id: LspRequestId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ContextProjectionCurrentness {
    pub(super) generation: u64,
    pub(super) identity: ContextProjectionIdentity,
}

#[derive(Default)]
pub(super) struct ContextController {
    pub(super) port: Option<Arc<dyn ContextProjectionPort + Send + Sync>>,
    pub(super) subscriptions: BTreeSet<ContextProjectionRegistration>,
    pub(super) currentness:
        BTreeMap<(ContextProjectionKind, Option<String>), ContextProjectionCurrentness>,
    pub(super) pending_requests: BTreeMap<LspRequestId, PendingContextRequest>,
    pub(super) pending_expansions: BTreeMap<LspRequestId, PendingContextExpansion>,
}

pub(super) fn bind_context_document_digest(
    request: &mut ContextProjectionRequest,
    overlays: &OverlayStore,
) {
    request.document_content_digest = request
        .document_uri
        .as_deref()
        .and_then(|uri| overlays.snapshot(uri))
        .map(|snapshot| ContentDigest::of_bytes(snapshot.text.as_bytes()));
}

impl<P, S, D> DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    pub(crate) fn handle_context_request(&mut self, id: Value, params: &Value, now_ms: u64) {
        let request = serde_json::from_value::<ContextProjectionRequest>(params.clone())
            .map_err(|_| RpcFailure::invalid_params("invalid tracedecay/context parameters"));
        match request {
            Ok(mut request) if request.kind.is_valid() => {
                let Some(context_request_id) = request_id(&id) else {
                    let _ = self.enqueue_value(error_response(
                        Value::Null,
                        RpcFailure::invalid_params(
                            "tracedecay/context requires an integer or string request id",
                        ),
                    ));
                    return;
                };
                if let Some(uri) = request.document_uri.as_deref()
                    && let Err(error) = self.require_document_root(uri)
                {
                    let _ = self.enqueue_value(error_response(id, error));
                    return;
                }
                bind_context_document_digest(&mut request, &self.lifecycle.overlays);
                let document = request.document_uri.as_ref().map(|uri| {
                    (
                        uri.clone(),
                        self.lifecycle.overlays.version(uri).unwrap_or_default(),
                    )
                });
                self.start_context_request(id, context_request_id, document, request, now_ms);
            }
            Ok(_) => {
                let _ = self.enqueue_value(error_response(
                    id,
                    RpcFailure::invalid_params("invalid TraceDecay projection kind"),
                ));
            }
            Err(error) => {
                let _ = self.enqueue_value(error_response(id, error));
            }
        }
    }

    pub(crate) fn handle_context_expand_request(&mut self, id: Value, params: &Value, now_ms: u64) {
        let request =
            serde_json::from_value::<ContextExpansionRequest>(params.clone()).map_err(|_| {
                RpcFailure::invalid_params("invalid tracedecay/context/expand parameters")
            });
        match request {
            Ok(request) if valid_retrieval_handle(Some(&request.retrieval_handle)) => {
                let Some(context_request_id) = request_id(&id) else {
                    let _ = self.enqueue_value(error_response(
                        Value::Null,
                        RpcFailure::invalid_params(
                            "tracedecay/context/expand requires an integer or string request id",
                        ),
                    ));
                    return;
                };
                self.start_context_expansion(id, context_request_id, request, now_ms);
            }
            Ok(_) => {
                let _ = self.enqueue_value(error_response(
                    id,
                    RpcFailure::invalid_params("invalid TraceDecay retrieval handle"),
                ));
            }
            Err(error) => {
                let _ = self.enqueue_value(error_response(id, error));
            }
        }
    }

    pub(crate) fn handle_context_subscribe(&mut self, id: Value, params: &Value, now_ms: u64) {
        let request = serde_json::from_value::<ContextSubscribeRequest>(params.clone())
            .map_err(|_| RpcFailure::invalid_params("invalid tracedecay/subscribe parameters"));
        match request {
            Ok(request) => {
                self.with_request(id, None, now_ms, move |session| {
                    session.context_subscription_value(request)
                });
            }
            Err(error) => {
                let _ = self.enqueue_value(error_response(id, error));
            }
        }
    }

    pub(super) fn start_context_request(
        &mut self,
        response_id: Value,
        request_id: LspRequestId,
        document: Option<(String, i64)>,
        request: ContextProjectionRequest,
        now_ms: u64,
    ) {
        let deadline = now_ms.saturating_add(self.lifecycle.request_deadline_ms);
        match self.lifecycle.control.admit_request_with_deadline(
            request_id.clone(),
            document,
            Some(deadline),
        ) {
            crate::session::RequestAdmission::Accepted => {
                let Ok(operation_id) =
                    NEXT_CONTEXT_OPERATION_ID.next_string("lsp-context-operation-")
                else {
                    self.complete_context_request(
                        request_id,
                        response_id,
                        Err(RpcFailure::request_failure(
                            LspRequestFailure::ServerCancelled {
                                retrigger_request: true,
                            },
                        )),
                    );
                    return;
                };
                let operation_id = LspRequestId::String(operation_id);
                match self.context_snapshot_value(&operation_id, &request) {
                    Ok(None) => {
                        self.context.pending_requests.insert(
                            request_id,
                            PendingContextRequest {
                                response_id,
                                operation_id,
                                request,
                            },
                        );
                    }
                    result => self.complete_context_request(request_id, response_id, result),
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

    pub(super) fn start_context_expansion(
        &mut self,
        response_id: Value,
        request_id: LspRequestId,
        request: ContextExpansionRequest,
        now_ms: u64,
    ) {
        let deadline = now_ms.saturating_add(self.lifecycle.request_deadline_ms);
        match self.lifecycle.control.admit_request_with_deadline(
            request_id.clone(),
            None,
            Some(deadline),
        ) {
            crate::session::RequestAdmission::Accepted => {
                let Ok(operation_id) =
                    NEXT_CONTEXT_OPERATION_ID.next_string("lsp-context-expansion-")
                else {
                    self.complete_context_request(
                        request_id,
                        response_id,
                        Err(RpcFailure::request_failure(
                            LspRequestFailure::ServerCancelled {
                                retrigger_request: true,
                            },
                        )),
                    );
                    return;
                };
                let operation_id = LspRequestId::String(operation_id);
                match self.context_expansion_value(&operation_id, &request) {
                    Ok(None) => {
                        self.context.pending_expansions.insert(
                            request_id,
                            PendingContextExpansion {
                                response_id,
                                operation_id,
                            },
                        );
                    }
                    result => self.complete_context_request(request_id, response_id, result),
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

    pub(super) fn complete_context_request(
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

    pub(super) fn context_snapshot_value(
        &mut self,
        request_id: &LspRequestId,
        request: &ContextProjectionRequest,
    ) -> Result<Option<Value>, RpcFailure> {
        let Some(revision) = self
            .lifecycle
            .gateway
            .capabilities()
            .context_projections
            .get(&request.kind)
            .copied()
        else {
            return Err(RpcFailure::unavailable(
                TRACEDECAY_CONTEXT_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        };
        let Some(context) = self.context.port.as_ref() else {
            return Err(RpcFailure::unavailable(
                TRACEDECAY_CONTEXT_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        };
        let root = self
            .optional_document_root(request.document_uri.as_deref(), TRACEDECAY_CONTEXT_METHOD)?;
        let outcome = context.snapshot(&root, request_id, request);
        self.context_projection_value(request, revision, outcome)
    }

    pub(super) fn context_projection_value(
        &mut self,
        request: &ContextProjectionRequest,
        revision: u32,
        outcome: ContextProjectionOutcome,
    ) -> Result<Option<Value>, RpcFailure> {
        let envelope = match outcome {
            ContextProjectionOutcome::Ready(envelope) => envelope,
            ContextProjectionOutcome::Pending => return Ok(None),
            ContextProjectionOutcome::Unsupported | ContextProjectionOutcome::Denied => {
                return Err(RpcFailure::unavailable(
                    TRACEDECAY_CONTEXT_METHOD,
                    MethodUnavailableReason::CapabilityNotNegotiated,
                ));
            }
            ContextProjectionOutcome::Deferred { reason } => {
                return Err(refresh_pending_failure(
                    None,
                    None,
                    Some(bounded_context_text(reason, MAX_CONTEXT_SUMMARY_BYTES)),
                    None,
                ));
            }
            ContextProjectionOutcome::Failed { reason } => {
                return Err(RpcFailure {
                    code: -32603,
                    message: "Internal error",
                    data: json!({
                        "failureClass": bounded_context_text(reason, MAX_CONTEXT_SUMMARY_BYTES),
                    }),
                });
            }
        };
        self.validate_context_envelope(request, revision, &envelope)?;
        let key = (envelope.kind.clone(), envelope.document_uri.clone());
        if self.context.currentness.get(&key).is_some_and(|current| {
            current.generation > envelope.generation
                || (current.generation == envelope.generation
                    && current.identity != envelope.identity)
        }) {
            return Err(refresh_pending_failure(
                None,
                None,
                None,
                Some("superseded-generation".to_owned()),
            ));
        }
        let value = serde_json::to_value(&envelope).map_err(|_| RpcFailure {
            code: -32603,
            message: "Internal error",
            data: Value::Null,
        })?;
        if serde_json::to_vec(&value)
            .map_or(true, |payload| payload.len() > MAX_CONTEXT_PROJECTION_BYTES)
        {
            return Err(refresh_pending_failure(
                None,
                None,
                Some("projection-payload-exceeded".to_owned()),
                None,
            ));
        }
        self.context.currentness.insert(
            key,
            ContextProjectionCurrentness {
                generation: envelope.generation,
                identity: envelope.identity,
            },
        );
        Ok(Some(value))
    }

    pub(super) fn poll_context_requests(&mut self) {
        let Some(context) = self.context.port.clone() else {
            return;
        };
        let request_ids = self
            .context
            .pending_requests
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for request_id in request_ids {
            let Some(operation_id) = self
                .context
                .pending_requests
                .get(&request_id)
                .map(|pending| pending.operation_id.clone())
            else {
                continue;
            };
            let Some(root) = self
                .context
                .pending_requests
                .get(&request_id)
                .and_then(|pending| {
                    self.optional_document_root(
                        pending.request.document_uri.as_deref(),
                        TRACEDECAY_CONTEXT_METHOD,
                    )
                    .ok()
                })
            else {
                continue;
            };
            let Some(outcome) = context.poll_snapshot(&root, &operation_id) else {
                continue;
            };
            if outcome == ContextProjectionOutcome::Pending {
                continue;
            }
            let Some(pending) = self.context.pending_requests.remove(&request_id) else {
                continue;
            };
            let Some(revision) = self
                .lifecycle
                .gateway
                .capabilities()
                .context_projections
                .get(&pending.request.kind)
                .copied()
            else {
                self.complete_context_request(
                    request_id,
                    pending.response_id,
                    Err(RpcFailure::unavailable(
                        TRACEDECAY_CONTEXT_METHOD,
                        MethodUnavailableReason::CapabilityNotNegotiated,
                    )),
                );
                continue;
            };
            let result = self.context_projection_value(&pending.request, revision, outcome);
            self.complete_context_request(request_id, pending.response_id, result);
        }
    }

    pub(super) fn context_expansion_value(
        &self,
        request_id: &LspRequestId,
        request: &ContextExpansionRequest,
    ) -> Result<Option<Value>, RpcFailure> {
        if !self
            .lifecycle
            .gateway
            .capabilities()
            .supports_context_expansion
        {
            return Err(RpcFailure::unavailable(
                TRACEDECAY_CONTEXT_EXPAND_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        }
        let Some(context) = self.context.port.as_ref() else {
            return Err(RpcFailure::unavailable(
                TRACEDECAY_CONTEXT_EXPAND_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        };
        let root = self.workspace_root(TRACEDECAY_CONTEXT_EXPAND_METHOD)?;
        self.context_expansion_outcome_value(context.expand(&root, request_id, request))
    }

    pub(super) fn context_expansion_outcome_value(
        &self,
        outcome: ContextExpansionOutcome,
    ) -> Result<Option<Value>, RpcFailure> {
        match outcome {
            ContextExpansionOutcome::Ready(envelope) => {
                self.validate_context_expansion(&envelope)?;
                let value = serde_json::to_value(envelope).map_err(|_| RpcFailure {
                    code: -32603,
                    message: "Internal error",
                    data: Value::Null,
                })?;
                if serde_json::to_vec(&value)
                    .map_or(true, |payload| payload.len() > MAX_CONTEXT_PROJECTION_BYTES)
                {
                    return Err(refresh_pending_failure(
                        None,
                        None,
                        Some("context-expansion-payload-exceeded".to_owned()),
                        None,
                    ));
                }
                Ok(Some(value))
            }
            ContextExpansionOutcome::Denied => Err(RpcFailure::unavailable(
                TRACEDECAY_CONTEXT_EXPAND_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            )),
            ContextExpansionOutcome::Pending => Ok(None),
            ContextExpansionOutcome::Failed { reason } => Err(RpcFailure {
                code: -32603,
                message: "Internal error",
                data: json!({
                    "failureClass": bounded_context_text(reason, MAX_CONTEXT_SUMMARY_BYTES),
                }),
            }),
        }
    }

    pub(super) fn poll_context_expansions(&mut self) {
        let Some(context) = self.context.port.clone() else {
            return;
        };
        let request_ids = self
            .context
            .pending_expansions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for request_id in request_ids {
            let Some(operation_id) = self
                .context
                .pending_expansions
                .get(&request_id)
                .map(|pending| pending.operation_id.clone())
            else {
                continue;
            };
            let Ok(root) = self.workspace_root(TRACEDECAY_CONTEXT_EXPAND_METHOD) else {
                continue;
            };
            let Some(outcome) = context.poll_expansion(&root, &operation_id) else {
                continue;
            };
            if outcome == ContextExpansionOutcome::Pending {
                continue;
            }
            let Some(pending) = self.context.pending_expansions.remove(&request_id) else {
                continue;
            };
            let result = self.context_expansion_outcome_value(outcome);
            self.complete_context_request(request_id, pending.response_id, result);
        }
    }

    pub(super) fn validate_context_expansion(
        &self,
        envelope: &ContextExpansionEnvelope,
    ) -> Result<(), RpcFailure> {
        let negotiated = self
            .lifecycle
            .gateway
            .capabilities()
            .context_projections
            .get(&envelope.kind)
            == Some(&envelope.revision);
        let routed_root = envelope
            .document_uri
            .as_deref()
            .map_or_else(
                || self.workspace_root(TRACEDECAY_CONTEXT_EXPAND_METHOD),
                |uri| self.document_root(uri),
            )
            .ok();
        let valid_scope = routed_root
            .as_ref()
            .is_some_and(|root| envelope.root_uri == root.uri())
            && is_supported_context_projection(&envelope.kind)
            && envelope.generation > 0
            && envelope.document_uri.as_deref().is_none_or(|uri| {
                routed_root
                    .as_ref()
                    .is_some_and(|root| root.contains_document(uri))
            })
            && !envelope.scope.scope_digest.is_empty()
            && valid_context_projection_identity(&envelope.scope.identity)
            && match (
                envelope.document_uri.is_some(),
                envelope.scope.identity.document_content_digest.as_deref(),
            ) {
                (true, Some(digest)) => !digest.is_empty(),
                (false, None) => true,
                _ => false,
            };
        let current_scope = envelope.coverage != ContextCoverage::Complete
            || self
                .context
                .currentness
                .get(&(envelope.kind.clone(), envelope.document_uri.clone()))
                .is_some_and(|current| {
                    current.generation == envelope.generation
                        && current.identity == envelope.scope.identity
                });
        let valid_payload = !envelope.stable_id.is_empty()
            && envelope.stable_id.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES
            && envelope.expires_at > 0
            && valid_retrieval_handle(envelope.next_retrieval_handle.as_deref())
            && match envelope.coverage {
                ContextCoverage::Complete => {
                    envelope.evidence.is_some()
                        && envelope.omission_reason.is_none()
                        && envelope.next_retrieval_handle.is_none()
                }
                ContextCoverage::Partial => envelope.omission_reason.is_some(),
                ContextCoverage::Unavailable | ContextCoverage::Failed => false,
            };
        if negotiated && valid_scope && current_scope && valid_payload {
            Ok(())
        } else {
            Err(RpcFailure {
                code: -32603,
                message: "Internal error",
                data: json!({ "failureClass": "invalid-context-expansion" }),
            })
        }
    }

    pub(super) fn context_subscription_value(
        &mut self,
        request: ContextSubscribeRequest,
    ) -> Result<Value, RpcFailure> {
        if self.context.port.is_none() || request.projections.len() > MAX_CONTEXT_PROJECTION_KINDS {
            return Err(RpcFailure::invalid_params(
                "TraceDecay projection subscription is unavailable or too large",
            ));
        }
        let subscriptions = request.projections.into_iter().collect::<BTreeSet<_>>();
        if subscriptions.len() > MAX_CONTEXT_PROJECTION_KINDS
            || subscriptions.iter().any(|registration| {
                !registration.kind.is_valid()
                    || self
                        .lifecycle
                        .gateway
                        .capabilities()
                        .context_projections
                        .get(&registration.kind)
                        != Some(&registration.revision)
            })
        {
            return Err(RpcFailure::unavailable(
                TRACEDECAY_SUBSCRIBE_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        }
        self.context.subscriptions = subscriptions;
        if let Some(context) = &self.context.port {
            for root in self.lifecycle.gateway.workspace().roots() {
                context.update_subscriptions(root, &self.context.subscriptions);
            }
        }
        Ok(json!({
            "projections": self.context.subscriptions.iter().collect::<Vec<_>>(),
        }))
    }

    pub(super) fn validate_context_envelope(
        &self,
        request: &ContextProjectionRequest,
        revision: u32,
        envelope: &ContextProjectionEnvelope,
    ) -> Result<(), RpcFailure> {
        let routed_root = envelope
            .document_uri
            .as_deref()
            .map_or_else(
                || self.workspace_root(TRACEDECAY_CONTEXT_METHOD),
                |uri| self.document_root(uri),
            )
            .ok();
        let valid_scope = routed_root
            .as_ref()
            .is_some_and(|root| envelope.root_uri == root.uri())
            && is_supported_context_projection(&envelope.kind)
            && envelope.generation > 0
            && envelope.document_uri == request.document_uri
            && envelope.document_uri.as_deref().is_none_or(|uri| {
                routed_root
                    .as_ref()
                    .is_some_and(|root| root.contains_document(uri))
            })
            && valid_context_projection_identity(&envelope.identity)
            && match (
                envelope.document_uri.is_some(),
                envelope.identity.document_content_digest.as_deref(),
            ) {
                (true, Some(digest)) => !digest.is_empty(),
                (false, None) => true,
                _ => false,
            }
            && request
                .document_content_digest
                .as_ref()
                .is_none_or(|expected| {
                    envelope.identity.document_content_digest.as_deref() == Some(expected.as_str())
                });
        let valid_items = envelope.items.len() <= MAX_CONTEXT_PROJECTION_ITEMS
            && envelope.items.iter().all(|item| {
                !item.stable_id.is_empty()
                    && item.stable_id.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES
                    && item.summary.len() <= MAX_CONTEXT_SUMMARY_BYTES
                    && valid_retrieval_handle(item.retrieval_handle.as_deref())
            });
        if valid_scope
            && envelope.kind == request.kind
            && envelope.revision == revision
            && valid_items
            && match envelope.coverage {
                ContextCoverage::Complete => {
                    envelope.freshness == ContextFreshness::Current
                        && envelope.producer_state == ContextProducerState::Complete
                        && envelope.omitted_count == 0
                        && envelope.omission_reasons.is_empty()
                }
                ContextCoverage::Partial => {
                    !envelope.omission_reasons.is_empty()
                        && matches!(
                            envelope.producer_state,
                            ContextProducerState::Partial | ContextProducerState::Indexing
                        )
                }
                ContextCoverage::Unavailable => {
                    envelope.items.is_empty()
                        && !envelope.omission_reasons.is_empty()
                        && matches!(
                            envelope.producer_state,
                            ContextProducerState::Unavailable
                                | ContextProducerState::Cancelled
                                | ContextProducerState::TimedOut
                        )
                }
                ContextCoverage::Failed => {
                    envelope.items.is_empty()
                        && !envelope.omission_reasons.is_empty()
                        && envelope.producer_state == ContextProducerState::Failed
                }
            }
            && valid_retrieval_handle(envelope.retrieval_handle.as_deref())
        {
            Ok(())
        } else {
            Err(RpcFailure {
                code: -32603,
                message: "Internal error",
                data: json!({ "failureClass": "invalid-context-projection" }),
            })
        }
    }

    pub(super) fn flush_context_changes(&mut self) {
        if self.context.subscriptions.is_empty()
            || !self.has_outbound_capacity(MAX_CONTEXT_PROJECTION_BYTES)
        {
            return;
        }
        let Some(context) = self.context.port.clone() else {
            return;
        };
        let changes = self
            .lifecycle
            .gateway
            .workspace()
            .roots()
            .iter()
            .flat_map(|root| context.poll_changes(root, &self.context.subscriptions))
            .collect::<Vec<_>>();
        for mut change in changes.into_iter().take(MAX_CONTEXT_CHANGES_PER_POLL) {
            if !self.valid_context_change(&change) {
                continue;
            }
            let key = (change.kind.clone(), change.document_uri.clone());
            let identity_drift_clear = match self.context.currentness.get(&key) {
                Some(current) if current.generation > change.generation => continue,
                Some(current) if current.generation == change.generation => {
                    if current.identity == change.identity {
                        false
                    } else {
                        change.identity = current.identity.clone();
                        change.freshness = ContextFreshness::Unknown;
                        change.producer_state = ContextProducerState::Unavailable;
                        change.coverage = ContextCoverage::Unavailable;
                        change.retrieval_handle = None;
                        true
                    }
                }
                _ => false,
            };
            if !self.valid_context_change(&change) {
                continue;
            }
            let Ok(params) = serde_json::to_value(&change) else {
                continue;
            };
            let notification = json!({
                "jsonrpc": "2.0",
                "method": TRACEDECAY_CONTEXT_CHANGED_METHOD,
                "params": params,
            });
            if serde_json::to_vec(&notification)
                .map_or(true, |payload| payload.len() > MAX_CONTEXT_PROJECTION_BYTES)
                || !self.enqueue_value(notification)
            {
                break;
            }
            if identity_drift_clear {
                self.context.currentness.remove(&key);
            } else {
                self.context.currentness.insert(
                    key,
                    ContextProjectionCurrentness {
                        generation: change.generation,
                        identity: change.identity,
                    },
                );
            }
        }
    }

    pub(super) fn valid_context_change(&self, change: &ContextProjectionChange) -> bool {
        let routed_root = change
            .document_uri
            .as_deref()
            .map_or_else(
                || self.workspace_root(TRACEDECAY_CONTEXT_CHANGED_METHOD),
                |uri| self.document_root(uri),
            )
            .ok();
        routed_root
            .as_ref()
            .is_some_and(|root| change.root_uri == root.uri())
            && is_supported_context_projection(&change.kind)
            && change.generation > 0
            && change.document_uri.as_deref().is_none_or(|uri| {
                routed_root
                    .as_ref()
                    .is_some_and(|root| root.contains_document(uri))
            })
            && valid_context_projection_identity(&change.identity)
            && match (
                change.document_uri.is_some(),
                change.identity.document_content_digest.as_deref(),
            ) {
                (true, Some(digest)) => !digest.is_empty(),
                (false, None) => true,
                _ => false,
            }
            && self
                .context
                .subscriptions
                .contains(&ContextProjectionRegistration {
                    kind: change.kind.clone(),
                    revision: change.revision,
                })
            && match change.coverage {
                ContextCoverage::Complete => {
                    change.freshness == ContextFreshness::Current
                        && change.producer_state == ContextProducerState::Complete
                }
                ContextCoverage::Partial => matches!(
                    change.producer_state,
                    ContextProducerState::Partial | ContextProducerState::Indexing
                ),
                ContextCoverage::Unavailable => matches!(
                    change.producer_state,
                    ContextProducerState::Unavailable
                        | ContextProducerState::Cancelled
                        | ContextProducerState::TimedOut
                ),
                ContextCoverage::Failed => change.producer_state == ContextProducerState::Failed,
            }
            && valid_retrieval_handle(change.retrieval_handle.as_deref())
    }

    pub(super) fn discard_document_context(&mut self, uri: &str) {
        self.context
            .currentness
            .retain(|(_, document_uri), _| document_uri.as_deref() != Some(uri));
    }
}

fn valid_retrieval_handle(handle: Option<&str>) -> bool {
    handle.is_none_or(|handle| {
        !handle.is_empty()
            && handle.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES
            && handle.bytes().all(|byte| byte.is_ascii_graphic())
    })
}

fn bounded_context_text(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

#[cfg(test)]
mod controller_tests {
    use super::*;

    #[test]
    fn bounded_context_text_preserves_utf8_boundaries() {
        assert_eq!(bounded_context_text("aéz".to_owned(), 2), "a");
        assert_eq!(bounded_context_text("aéz".to_owned(), 3), "aé");
    }
}
