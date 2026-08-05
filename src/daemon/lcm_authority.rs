//! Daemon ownership of LCM commands and canonical temporal reads.
//!
//! The registered session database is private to this module. API, MCP, hook,
//! and host adapters receive only [`LcmAuthorityPort`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_application::{
    CancellationStage, OperationTermination, RequestAdmission, RequestContext,
};
use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_sessions::runtime::lcm::{
    LcmCompressionRequest, LcmCompressionResponse, LcmError, LcmGcConfig, LcmPreflightRequest,
    LcmPreflightResponse, LcmSessionBoundaryRequest, LcmSessionBoundaryResponse, LcmStatus,
    LcmSummarizerMode, LcmSummaryRequest,
};
use tracedecay_temporal_query::TemporalKernelResult;
use tracedecay_usecases::context::{
    CancellationToken, RequestInterruption, application_observed_at,
    application_request_interruption, run_application_request_interruptible,
};
use tracedecay_usecases::session::SessionRetrievalOutcome;
use tracedecay_usecases::session::lcm::{
    LcmAuthorityFuture, LcmAuthorityInvocation, LcmAuthorityOperation, LcmAuthorityOutcome,
    LcmAuthorityPayload, LcmAuthorityPort, LcmAuthorityRequest, LcmAuthorityResponse,
    LcmAuthorityUnavailableReason, LcmCompactionCommand, LcmCompressionEvidence, LcmHostProtocol,
    LcmStatusQuery, LcmTemporalReadRequest, lcm_authority_operation_identity,
};

use crate::global_db::RegisteredGlobalDb;

mod receipt;
use receipt::{
    temporal_termination, terminal, terminal_failure, terminal_interruption, unavailable,
    unavailable_with_state,
};

type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, LcmError>> + Send + 'a>>;
type CanonicalSummaryFuture<'a> =
    Pin<Box<dyn Future<Output = CanonicalSummaryAdmission> + Send + 'a>>;
type TemporalReadFuture<'a> =
    Pin<Box<dyn Future<Output = SessionRetrievalOutcome<TemporalKernelResult>> + Send + 'a>>;

trait LcmDaemonStore: Send + Sync {
    fn preflight(&self, request: LcmPreflightRequest) -> StoreFuture<'_, LcmPreflightResponse>;
    fn compact(&self, request: LcmCompressionRequest) -> StoreFuture<'_, LcmCompressionResponse>;
    fn session_boundary(
        &self,
        request: LcmSessionBoundaryRequest,
    ) -> StoreFuture<'_, LcmSessionBoundaryResponse>;
    fn status(&self, query: LcmStatusQuery) -> StoreFuture<'_, LcmStatus>;
}

struct RegisteredLcmDaemonStore {
    database: Arc<RegisteredGlobalDb>,
}

impl RegisteredLcmDaemonStore {
    fn new(database: Arc<RegisteredGlobalDb>) -> Self {
        Self { database }
    }
}

impl LcmDaemonStore for RegisteredLcmDaemonStore {
    fn preflight(&self, request: LcmPreflightRequest) -> StoreFuture<'_, LcmPreflightResponse> {
        Box::pin(self.database.lcm_preflight(request))
    }

    fn compact(&self, request: LcmCompressionRequest) -> StoreFuture<'_, LcmCompressionResponse> {
        Box::pin(self.database.lcm_compress(request))
    }

    fn session_boundary(
        &self,
        request: LcmSessionBoundaryRequest,
    ) -> StoreFuture<'_, LcmSessionBoundaryResponse> {
        Box::pin(self.database.lcm_session_boundary(request))
    }

    fn status(&self, query: LcmStatusQuery) -> StoreFuture<'_, LcmStatus> {
        Box::pin(async move {
            self.database
                .lcm_status_with_options(
                    &query.provider,
                    query.session_id.as_deref(),
                    query.deep,
                    &LcmGcConfig::default(),
                )
                .await
        })
    }
}

/// Result of authenticating Claude's native summary and rehydrating every
/// supplied source anchor through the canonical temporal content/redaction
/// authority on one frozen snapshot and relation generation.
pub(crate) enum CanonicalSummaryAdmission {
    Admitted {
        summary_text: String,
        /// Digest of the authenticated event, frozen relation generation,
        /// ordered anchors, and canonically hydrated/redacted source content.
        source_state: ManifestDigest,
    },
    Unavailable(LcmAuthorityUnavailableReason),
}

/// Composition seam owned jointly by host admission and temporal retrieval.
///
/// Implementations must authenticate the exact Claude protocol event before
/// comparing its anchors with `summary_request`, then hydrate only those
/// anchors through canonical content/redaction. Cursor, Codex, Hermes, and
/// generic provider events must return `HostProtocolUnavailable`.
pub(crate) trait LcmCanonicalSummaryAdmissionPort: Send + Sync {
    fn admit<'a>(
        &'a self,
        context: &'a RequestContext,
        cancellation: &'a CancellationToken,
        command: &'a LcmCompactionCommand,
        summary_request: &'a LcmSummaryRequest,
    ) -> CanonicalSummaryFuture<'a>;
}

/// Canonical temporal read seam. The implementation is the existing
/// `SessionRetrievalService` composition; it freezes one registered snapshot,
/// relation generation, page, and cursor before selected-anchor hydration.
pub(crate) trait LcmTemporalReadPort: Send + Sync {
    fn read<'a>(
        &'a self,
        context: &'a RequestContext,
        cancellation: &'a CancellationToken,
        request: LcmTemporalReadRequest,
    ) -> TemporalReadFuture<'a>;
}

/// One daemon-owned LCM authority bound to one registered session shard.
///
/// No database accessor is exposed. Reconstructing this value after daemon
/// restart binds a newly fenced handle to the same durable shard and reads the
/// committed LCM frontier/status from storage.
pub(crate) struct DaemonLcmAuthority {
    store: Option<Arc<dyn LcmDaemonStore>>,
    canonical_summary: Option<Arc<dyn LcmCanonicalSummaryAdmissionPort>>,
    temporal_read: Option<Arc<dyn LcmTemporalReadPort>>,
}

impl DaemonLcmAuthority {
    pub(crate) fn registered(database: Arc<RegisteredGlobalDb>) -> Self {
        Self {
            store: Some(Arc::new(RegisteredLcmDaemonStore::new(database))),
            canonical_summary: None,
            temporal_read: None,
        }
    }

    pub(crate) fn unavailable() -> Self {
        Self {
            store: None,
            canonical_summary: None,
            temporal_read: None,
        }
    }

    pub(crate) fn with_canonical_summary_admission(
        mut self,
        authority: Arc<dyn LcmCanonicalSummaryAdmissionPort>,
    ) -> Self {
        self.canonical_summary = Some(authority);
        self
    }

    pub(crate) fn with_temporal_read(mut self, authority: Arc<dyn LcmTemporalReadPort>) -> Self {
        self.temporal_read = Some(authority);
        self
    }

    #[cfg(test)]
    fn with_store(store: Arc<dyn LcmDaemonStore>) -> Self {
        Self {
            store: Some(store),
            canonical_summary: None,
            temporal_read: None,
        }
    }

    async fn execute_inner(&self, invocation: LcmAuthorityInvocation) -> LcmAuthorityResponse {
        let started_at = application_observed_at();
        let operation = invocation.request.operation();
        if invocation.cancellation.application_token_id()
            != Some(invocation.context.cancellation().token_id.as_str())
        {
            return terminal(
                &invocation.context,
                operation,
                started_at,
                LcmAuthorityOutcome::Denied,
                OperationTermination::Failed,
                None,
                None,
                None,
            );
        }
        let admission = invocation.context.admission_at(started_at);
        if admission != RequestAdmission::Admitted {
            let interruption = match admission {
                RequestAdmission::Cancelled => RequestInterruption::Cancelled,
                RequestAdmission::TimedOut => RequestInterruption::DeadlineExceeded,
                RequestAdmission::Admitted => {
                    return terminal_failure(
                        &invocation.context,
                        operation,
                        started_at,
                        "LCM request admission changed unexpectedly",
                    );
                }
            };
            return terminal_interruption(
                &invocation.context,
                operation,
                started_at,
                interruption,
                CancellationStage::BeforeAdmission,
                None,
            );
        }
        let Ok((capability, use_case)) = lcm_authority_operation_identity(operation) else {
            return terminal_failure(
                &invocation.context,
                operation,
                started_at,
                "LCM operation identity is unavailable",
            );
        };
        if !invocation.context.allows(&capability, &use_case) {
            return terminal(
                &invocation.context,
                operation,
                started_at,
                LcmAuthorityOutcome::Denied,
                OperationTermination::Failed,
                None,
                None,
                None,
            );
        }
        if let Some(interruption) =
            application_request_interruption(&invocation.context, &invocation.cancellation)
        {
            return terminal_interruption(
                &invocation.context,
                operation,
                started_at,
                interruption,
                CancellationStage::BeforeAdmission,
                None,
            );
        }

        match invocation.request {
            LcmAuthorityRequest::Preflight(request) => {
                self.execute_preflight(
                    &invocation.context,
                    &invocation.cancellation,
                    started_at,
                    request,
                )
                .await
            }
            LcmAuthorityRequest::Compact(command) => {
                self.execute_compaction(
                    &invocation.context,
                    &invocation.cancellation,
                    started_at,
                    command,
                )
                .await
            }
            LcmAuthorityRequest::SessionBoundary(request) => {
                self.execute_boundary(
                    &invocation.context,
                    &invocation.cancellation,
                    started_at,
                    request,
                )
                .await
            }
            LcmAuthorityRequest::Status(query) => {
                self.execute_status(
                    &invocation.context,
                    &invocation.cancellation,
                    started_at,
                    query,
                )
                .await
            }
            LcmAuthorityRequest::TemporalRead(request) => {
                self.execute_temporal_read(
                    &invocation.context,
                    &invocation.cancellation,
                    started_at,
                    request,
                )
                .await
            }
        }
    }

    async fn execute_preflight(
        &self,
        context: &RequestContext,
        cancellation: &CancellationToken,
        started_at: UtcMicros,
        request: LcmPreflightRequest,
    ) -> LcmAuthorityResponse {
        let Some(store) = self.store.as_ref() else {
            return unavailable(
                context,
                LcmAuthorityOperation::Preflight,
                started_at,
                LcmAuthorityUnavailableReason::StoreAuthorityUnavailable,
            );
        };
        let result = run_application_request_interruptible(
            context,
            cancellation,
            store.preflight(request),
            || {},
        )
        .await;
        match result {
            Ok(Ok(response)) => {
                let Ok(state) = canonical_sha256(&response) else {
                    return terminal_failure(
                        context,
                        LcmAuthorityOperation::Preflight,
                        started_at,
                        "LCM preflight receipt could not be encoded",
                    );
                };
                terminal(
                    context,
                    LcmAuthorityOperation::Preflight,
                    started_at,
                    LcmAuthorityOutcome::Ready,
                    OperationTermination::Completed,
                    Some(state),
                    Some(LcmAuthorityPayload::Preflight(response)),
                    None,
                )
            }
            Ok(Err(_)) => terminal_failure(
                context,
                LcmAuthorityOperation::Preflight,
                started_at,
                "LCM preflight failed",
            ),
            Err(interruption) => terminal_interruption(
                context,
                LcmAuthorityOperation::Preflight,
                started_at,
                interruption,
                CancellationStage::EffectInFlight,
                None,
            ),
        }
    }

    async fn execute_compaction(
        &self,
        context: &RequestContext,
        cancellation: &CancellationToken,
        started_at: UtcMicros,
        command: LcmCompactionCommand,
    ) -> LcmAuthorityResponse {
        let Some(store) = self.store.as_ref() else {
            return unavailable(
                context,
                LcmAuthorityOperation::Compact,
                started_at,
                LcmAuthorityUnavailableReason::StoreAuthorityUnavailable,
            );
        };
        if command.evidence.protocol().provider() != command.preflight.provider {
            return unavailable(
                context,
                LcmAuthorityOperation::Compact,
                started_at,
                LcmAuthorityUnavailableReason::HostProtocolUnavailable,
            );
        }

        match &command.evidence {
            LcmCompressionEvidence::PressureOnly { .. } => {
                let preflight = run_application_request_interruptible(
                    context,
                    cancellation,
                    store.preflight(command.preflight.clone()),
                    || {},
                )
                .await;
                match preflight {
                    Ok(Ok(response)) => {
                        let Ok(state) = canonical_sha256(&(
                            command.evidence.protocol().event_digest(),
                            &response,
                        )) else {
                            return terminal_failure(
                                context,
                                LcmAuthorityOperation::Compact,
                                started_at,
                                "LCM pressure-only preflight receipt could not be encoded",
                            );
                        };
                        unavailable_with_state(
                            context,
                            LcmAuthorityOperation::Compact,
                            started_at,
                            LcmAuthorityUnavailableReason::HostPayloadUnavailable,
                            state,
                        )
                    }
                    Ok(Err(_)) => terminal_failure(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        "LCM compaction preflight failed",
                    ),
                    Err(interruption) => terminal_interruption(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        interruption,
                        CancellationStage::EffectInFlight,
                        None,
                    ),
                }
            }
            LcmCompressionEvidence::ClaudeNativeSummary {
                protocol,
                summary_text,
                source_anchors,
                ..
            } => {
                if !matches!(protocol, LcmHostProtocol::ClaudeCodePreCompact { .. })
                    || summary_text.trim().is_empty()
                    || source_anchors.is_empty()
                {
                    return unavailable(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        LcmAuthorityUnavailableReason::HostProtocolUnavailable,
                    );
                }
                let preparation =
                    storage_compression_request(&command, LcmSummarizerMode::HermesAuxiliary);
                let prepared = run_application_request_interruptible(
                    context,
                    cancellation,
                    store.compact(preparation),
                    || {},
                )
                .await;
                let prepared = match prepared {
                    Ok(Ok(response)) => response,
                    Ok(Err(_)) => {
                        return terminal_failure(
                            context,
                            LcmAuthorityOperation::Compact,
                            started_at,
                            "LCM native compaction preparation failed",
                        );
                    }
                    Err(interruption) => {
                        return terminal_interruption(
                            context,
                            LcmAuthorityOperation::Compact,
                            started_at,
                            interruption,
                            CancellationStage::EffectInFlight,
                            None,
                        );
                    }
                };
                let Ok(prepared_state) =
                    canonical_sha256(&(command.evidence.protocol().event_digest(), &prepared))
                else {
                    return terminal_failure(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        "LCM native compaction preparation receipt could not be encoded",
                    );
                };
                let Some(summary_request) = prepared.summary_request.as_ref() else {
                    return terminal(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        LcmAuthorityOutcome::Ready,
                        OperationTermination::Completed,
                        Some(prepared_state),
                        Some(LcmAuthorityPayload::Compaction(prepared)),
                        None,
                    );
                };
                let Some(canonical_summary) = self.canonical_summary.as_ref() else {
                    return unavailable_with_state(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        LcmAuthorityUnavailableReason::SummaryRelationAuthorityUnavailable,
                        prepared_state,
                    );
                };
                let admission = run_application_request_interruptible(
                    context,
                    cancellation,
                    canonical_summary.admit(context, cancellation, &command, summary_request),
                    || {},
                )
                .await;
                let admission = match admission {
                    Ok(admission) => admission,
                    Err(interruption) => {
                        return terminal_interruption(
                            context,
                            LcmAuthorityOperation::Compact,
                            started_at,
                            interruption,
                            CancellationStage::BeforeEffect,
                            Some(prepared_state),
                        );
                    }
                };
                let CanonicalSummaryAdmission::Admitted {
                    summary_text,
                    source_state,
                } = admission
                else {
                    let CanonicalSummaryAdmission::Unavailable(reason) = admission;
                    return unavailable_with_state(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        reason,
                        prepared_state,
                    );
                };
                let request = storage_compression_request(
                    &command,
                    LcmSummarizerMode::Provided {
                        summary_text,
                        route: Some("claude_native_precompact".to_owned()),
                    },
                );
                let mut response = self
                    .run_compaction(
                        context,
                        cancellation,
                        started_at,
                        store,
                        request,
                        Some(prepared_state),
                    )
                    .await;
                if response.outcome == LcmAuthorityOutcome::Ready
                    && response.receipt.committed_state.is_some()
                {
                    let Ok(state) =
                        canonical_sha256(&(&source_state, &response.receipt.committed_state))
                    else {
                        return terminal_failure(
                            context,
                            LcmAuthorityOperation::Compact,
                            started_at,
                            "LCM canonical source receipt could not be encoded",
                        );
                    };
                    response.receipt.committed_state = Some(state);
                }
                response
            }
        }
    }

    async fn run_compaction(
        &self,
        context: &RequestContext,
        cancellation: &CancellationToken,
        started_at: UtcMicros,
        store: &Arc<dyn LcmDaemonStore>,
        request: LcmCompressionRequest,
        prior_committed_state: Option<ManifestDigest>,
    ) -> LcmAuthorityResponse {
        let result = run_application_request_interruptible(
            context,
            cancellation,
            store.compact(request),
            || {},
        )
        .await;
        match result {
            Ok(Ok(response)) => {
                let Ok(state) = canonical_sha256(&response) else {
                    return terminal_failure(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        "LCM compaction receipt could not be encoded",
                    );
                };
                terminal(
                    context,
                    LcmAuthorityOperation::Compact,
                    started_at,
                    LcmAuthorityOutcome::Ready,
                    OperationTermination::Completed,
                    Some(state),
                    Some(LcmAuthorityPayload::Compaction(response)),
                    None,
                )
            }
            Ok(Err(_)) => terminal(
                context,
                LcmAuthorityOperation::Compact,
                started_at,
                LcmAuthorityOutcome::Failed {
                    diagnostic: "LCM compaction failed".to_owned(),
                },
                OperationTermination::Failed,
                prior_committed_state,
                None,
                None,
            ),
            Err(interruption) => terminal_interruption(
                context,
                LcmAuthorityOperation::Compact,
                started_at,
                interruption,
                CancellationStage::EffectInFlight,
                prior_committed_state,
            ),
        }
    }

    async fn execute_boundary(
        &self,
        context: &RequestContext,
        cancellation: &CancellationToken,
        started_at: UtcMicros,
        request: LcmSessionBoundaryRequest,
    ) -> LcmAuthorityResponse {
        let Some(store) = self.store.as_ref() else {
            return unavailable(
                context,
                LcmAuthorityOperation::SessionBoundary,
                started_at,
                LcmAuthorityUnavailableReason::StoreAuthorityUnavailable,
            );
        };
        let result = run_application_request_interruptible(
            context,
            cancellation,
            store.session_boundary(request),
            || {},
        )
        .await;
        match result {
            Ok(Ok(response)) => {
                let Ok(state) = canonical_sha256(&response) else {
                    return terminal_failure(
                        context,
                        LcmAuthorityOperation::SessionBoundary,
                        started_at,
                        "LCM session boundary receipt could not be encoded",
                    );
                };
                terminal(
                    context,
                    LcmAuthorityOperation::SessionBoundary,
                    started_at,
                    LcmAuthorityOutcome::Ready,
                    OperationTermination::Completed,
                    Some(state),
                    Some(LcmAuthorityPayload::SessionBoundary(response)),
                    None,
                )
            }
            Ok(Err(_)) => terminal_failure(
                context,
                LcmAuthorityOperation::SessionBoundary,
                started_at,
                "LCM session boundary failed",
            ),
            Err(interruption) => terminal_interruption(
                context,
                LcmAuthorityOperation::SessionBoundary,
                started_at,
                interruption,
                CancellationStage::EffectInFlight,
                None,
            ),
        }
    }

    async fn execute_status(
        &self,
        context: &RequestContext,
        cancellation: &CancellationToken,
        started_at: UtcMicros,
        query: LcmStatusQuery,
    ) -> LcmAuthorityResponse {
        let Some(store) = self.store.as_ref() else {
            return unavailable(
                context,
                LcmAuthorityOperation::Status,
                started_at,
                LcmAuthorityUnavailableReason::StoreAuthorityUnavailable,
            );
        };
        let result = run_application_request_interruptible(
            context,
            cancellation,
            store.status(query),
            || {},
        )
        .await;
        match result {
            Ok(Ok(status)) => terminal(
                context,
                LcmAuthorityOperation::Status,
                started_at,
                LcmAuthorityOutcome::Ready,
                OperationTermination::Completed,
                None,
                Some(LcmAuthorityPayload::Status(status)),
                None,
            ),
            Ok(Err(_)) => terminal_failure(
                context,
                LcmAuthorityOperation::Status,
                started_at,
                "LCM status read failed",
            ),
            Err(interruption) => terminal_interruption(
                context,
                LcmAuthorityOperation::Status,
                started_at,
                interruption,
                CancellationStage::DuringRead,
                None,
            ),
        }
    }

    async fn execute_temporal_read(
        &self,
        context: &RequestContext,
        cancellation: &CancellationToken,
        started_at: UtcMicros,
        request: LcmTemporalReadRequest,
    ) -> LcmAuthorityResponse {
        if request.binding.cancellation().application_token_id()
            != Some(context.cancellation().token_id.as_str())
        {
            return terminal(
                context,
                LcmAuthorityOperation::TemporalRead,
                started_at,
                LcmAuthorityOutcome::Denied,
                OperationTermination::Failed,
                None,
                None,
                None,
            );
        }
        let Some(temporal) = self.temporal_read.as_ref() else {
            return unavailable(
                context,
                LcmAuthorityOperation::TemporalRead,
                started_at,
                LcmAuthorityUnavailableReason::TemporalAuthorityUnavailable,
            );
        };
        let result = run_application_request_interruptible(
            context,
            cancellation,
            temporal.read(context, cancellation, request),
            || {},
        )
        .await;
        match result {
            Ok(SessionRetrievalOutcome::Cancelled) | Err(RequestInterruption::Cancelled) => {
                terminal_interruption(
                    context,
                    LcmAuthorityOperation::TemporalRead,
                    started_at,
                    RequestInterruption::Cancelled,
                    CancellationStage::DuringRead,
                    None,
                )
            }
            Err(RequestInterruption::DeadlineExceeded) => terminal_interruption(
                context,
                LcmAuthorityOperation::TemporalRead,
                started_at,
                RequestInterruption::DeadlineExceeded,
                CancellationStage::DuringRead,
                None,
            ),
            Ok(SessionRetrievalOutcome::Unavailable) => unavailable(
                context,
                LcmAuthorityOperation::TemporalRead,
                started_at,
                LcmAuthorityUnavailableReason::TemporalAuthorityUnavailable,
            ),
            Ok(SessionRetrievalOutcome::Denied | SessionRetrievalOutcome::WrongScope) => terminal(
                context,
                LcmAuthorityOperation::TemporalRead,
                started_at,
                LcmAuthorityOutcome::Denied,
                OperationTermination::Failed,
                None,
                None,
                None,
            ),
            Ok(outcome) => terminal(
                context,
                LcmAuthorityOperation::TemporalRead,
                started_at,
                LcmAuthorityOutcome::Ready,
                temporal_termination(&outcome),
                None,
                Some(LcmAuthorityPayload::TemporalRead(outcome)),
                None,
            ),
        }
    }
}

impl LcmAuthorityPort for DaemonLcmAuthority {
    fn execute(&self, invocation: LcmAuthorityInvocation) -> LcmAuthorityFuture<'_> {
        Box::pin(self.execute_inner(invocation))
    }
}

fn storage_compression_request(
    command: &LcmCompactionCommand,
    summarizer: LcmSummarizerMode,
) -> LcmCompressionRequest {
    let preflight = &command.preflight;
    LcmCompressionRequest {
        provider: preflight.provider.clone(),
        session_id: preflight.session_id.clone(),
        messages: preflight.messages.clone(),
        current_tokens: preflight.current_tokens,
        focus_topic: command.focus_topic.clone(),
        ignore_session_patterns: preflight.ignore_session_patterns.clone(),
        stateless_session_patterns: preflight.stateless_session_patterns.clone(),
        ignore_message_patterns: preflight.ignore_message_patterns.clone(),
        expected_current_frontier_store_id: command.expected_current_frontier_store_id,
        threshold_tokens: preflight.threshold_tokens,
        max_assembly_tokens: preflight.max_assembly_tokens,
        leaf_chunk_tokens: preflight.leaf_chunk_tokens,
        max_source_messages: preflight.max_source_messages,
        summary_fan_in: preflight.summary_fan_in,
        incremental_max_depth: preflight.incremental_max_depth,
        fresh_tail_count: preflight.fresh_tail_count,
        dynamic_leaf_chunk_enabled: preflight.dynamic_leaf_chunk_enabled,
        dynamic_leaf_chunk_max: preflight.dynamic_leaf_chunk_max,
        context_length: preflight.context_length,
        reserve_tokens_floor: preflight.reserve_tokens_floor,
        summarizer,
    }
}

#[cfg(test)]
mod tests;
