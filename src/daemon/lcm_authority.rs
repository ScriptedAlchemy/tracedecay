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
use tracedecay_domain::{ManifestDigest, RetrievalAnchorId, UtcMicros, canonical_sha256};
use tracedecay_sessions::runtime::lcm::{
    LcmCompressionRequest, LcmCompressionResponse, LcmError, LcmGcConfig, LcmPreflightRequest,
    LcmPreflightResponse, LcmStatus, LcmSummarizerMode,
};
use tracedecay_usecases::context::{
    CancellationToken, RequestInterruption, application_observed_at,
    application_request_interruption, run_application_request_interruptible,
};
use tracedecay_usecases::session::lcm::{
    LcmAuthorityFuture, LcmAuthorityInvocation, LcmAuthorityOperation, LcmAuthorityOutcome,
    LcmAuthorityPayload, LcmAuthorityPort, LcmAuthorityRequest, LcmAuthorityResponse,
    LcmAuthorityTarget, LcmAuthorityUnavailableReason, LcmCompactionCommand,
    LcmCompressionEvidence, LcmDoctorQuery, LcmHostProtocol, LcmStatusQuery,
    LcmTranscriptIngestCommand, lcm_authority_operation_identity,
};

use crate::global_db::RegisteredGlobalDb;

mod canonical_source;
mod mount;
mod receipt;
pub(crate) use mount::{MountedLcmAuthorityPort, mount_registered_lcm_authority};
use receipt::{terminal, terminal_failure, terminal_interruption, unavailable};

type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, LcmError>> + Send + 'a>>;

trait LcmDaemonStore: Send + Sync {
    fn ingest(&self, request: LcmPreflightRequest) -> StoreFuture<'_, LcmPreflightResponse>;
    fn compact(&self, request: LcmCompressionRequest) -> StoreFuture<'_, LcmCompressionResponse>;
    fn status(&self, query: LcmStatusQuery) -> StoreFuture<'_, LcmStatus>;
    fn doctor(&self, query: LcmDoctorQuery) -> StoreFuture<'_, serde_json::Value>;
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
    fn ingest(&self, request: LcmPreflightRequest) -> StoreFuture<'_, LcmPreflightResponse> {
        Box::pin(self.database.lcm_preflight(request))
    }

    fn compact(&self, request: LcmCompressionRequest) -> StoreFuture<'_, LcmCompressionResponse> {
        Box::pin(self.database.lcm_compress_required_summary(request))
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

    fn doctor(&self, _query: LcmDoctorQuery) -> StoreFuture<'_, serde_json::Value> {
        Box::pin(async move {
            serde_json::to_value(
                crate::global_db::session_temporal::session_temporal_doctor_health_at(
                    self.database.db_path(),
                )
                .await,
            )
            .map_err(|error| LcmError::Db(error.to_string()))
        })
    }
}

#[derive(Clone, Debug)]
struct CanonicalCompactionSource {
    messages: Vec<serde_json::Value>,
    anchors: Vec<RetrievalAnchorId>,
    snapshot_state: ManifestDigest,
}

enum CanonicalCompactionSourceOutcome {
    Ready(CanonicalCompactionSource),
    Unavailable,
}

type CanonicalSourceFuture<'a> =
    Pin<Box<dyn Future<Output = CanonicalCompactionSourceOutcome> + Send + 'a>>;

trait LcmCanonicalCompactionSourcePort: Send + Sync {
    fn hydrate<'a>(&'a self, provider: &'a str, session_id: &'a str) -> CanonicalSourceFuture<'a>;
}

fn compression_request(
    preflight: LcmPreflightRequest,
    messages: Vec<serde_json::Value>,
    summary_text: String,
) -> LcmCompressionRequest {
    LcmCompressionRequest {
        provider: preflight.provider,
        session_id: preflight.session_id,
        messages,
        // PostCompact proves the host already crossed its native compaction
        // boundary. Force the storage plan to publish the supplied summary
        // instead of re-evaluating a budget signal that the event does not
        // carry.
        current_tokens: Some(preflight.current_tokens.unwrap_or(2).max(2)),
        focus_topic: None,
        ignore_session_patterns: preflight.ignore_session_patterns,
        stateless_session_patterns: preflight.stateless_session_patterns,
        ignore_message_patterns: preflight.ignore_message_patterns,
        expected_current_frontier_store_id: None,
        threshold_tokens: Some(preflight.threshold_tokens.unwrap_or(1).max(1)),
        max_assembly_tokens: preflight.max_assembly_tokens,
        leaf_chunk_tokens: preflight.leaf_chunk_tokens,
        max_source_messages: preflight.max_source_messages,
        summary_fan_in: preflight.summary_fan_in,
        incremental_max_depth: preflight.incremental_max_depth,
        fresh_tail_count: Some(0),
        dynamic_leaf_chunk_enabled: preflight.dynamic_leaf_chunk_enabled,
        dynamic_leaf_chunk_max: preflight.dynamic_leaf_chunk_max,
        context_length: preflight.context_length,
        reserve_tokens_floor: preflight.reserve_tokens_floor,
        summarizer: LcmSummarizerMode::Provided {
            summary_text,
            route: Some("claude_native_postcompact".to_owned()),
        },
    }
}

/// One daemon-owned LCM authority bound to one registered session shard.
///
/// No database accessor is exposed. Reconstructing this value after daemon
/// restart binds a newly fenced handle to the same durable shard and reads the
/// committed LCM frontier/status from storage.
pub(crate) struct DaemonLcmAuthority {
    store: Option<Arc<dyn LcmDaemonStore>>,
    canonical_source: Option<Arc<dyn LcmCanonicalCompactionSourcePort>>,
}

impl DaemonLcmAuthority {
    pub(crate) fn registered(database: Arc<RegisteredGlobalDb>) -> Self {
        Self {
            store: Some(Arc::new(RegisteredLcmDaemonStore::new(database))),
            canonical_source: None,
        }
    }

    pub(crate) fn unavailable() -> Self {
        Self {
            store: None,
            canonical_source: None,
        }
    }

    fn with_canonical_source(
        mut self,
        canonical_source: Arc<dyn LcmCanonicalCompactionSourcePort>,
    ) -> Self {
        self.canonical_source = Some(canonical_source);
        self
    }

    #[cfg(test)]
    fn with_store(store: Arc<dyn LcmDaemonStore>) -> Self {
        Self {
            store: Some(store),
            canonical_source: None,
        }
    }

    async fn execute_inner(&self, invocation: LcmAuthorityInvocation) -> LcmAuthorityResponse {
        let started_at = application_observed_at();
        let operation = invocation.request.operation();
        if invocation.target != invocation.request.authority_target()
            || invocation
                .binding
                .validate_context(&invocation.context)
                .is_err()
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
        if !mount::binding_matches_target(&invocation.binding, &capability, &invocation.target) {
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
            LcmAuthorityRequest::Ingest(command) => {
                self.execute_ingest(
                    &invocation.context,
                    &invocation.cancellation,
                    started_at,
                    command,
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
            LcmAuthorityRequest::Status(query) => {
                self.execute_status(
                    &invocation.context,
                    &invocation.cancellation,
                    started_at,
                    query,
                )
                .await
            }
            LcmAuthorityRequest::Doctor(query) => {
                self.execute_doctor(
                    &invocation.context,
                    &invocation.cancellation,
                    started_at,
                    query,
                )
                .await
            }
        }
    }

    async fn execute_ingest(
        &self,
        context: &RequestContext,
        cancellation: &CancellationToken,
        started_at: UtcMicros,
        command: LcmTranscriptIngestCommand,
    ) -> LcmAuthorityResponse {
        let Some(store) = self.store.as_ref() else {
            return unavailable(
                context,
                LcmAuthorityOperation::Ingest,
                started_at,
                LcmAuthorityUnavailableReason::StoreAuthorityUnavailable,
            );
        };
        if command.protocol_revision != "hermes.turn-completed.v1"
            || command.preflight.provider != "hermes"
        {
            return unavailable(
                context,
                LcmAuthorityOperation::Ingest,
                started_at,
                LcmAuthorityUnavailableReason::HostProtocolUnavailable,
            );
        }
        if command.preflight.messages.is_empty() {
            return unavailable(
                context,
                LcmAuthorityOperation::Ingest,
                started_at,
                LcmAuthorityUnavailableReason::HostPayloadUnavailable,
            );
        }
        let Ok(expected_digest) = canonical_sha256(&(
            &command.preflight.provider,
            &command.preflight.session_id,
            &command.preflight.messages,
        )) else {
            return terminal_failure(
                context,
                LcmAuthorityOperation::Ingest,
                started_at,
                "Hermes turn payload could not be encoded",
            );
        };
        if expected_digest != command.event_digest {
            return unavailable(
                context,
                LcmAuthorityOperation::Ingest,
                started_at,
                LcmAuthorityUnavailableReason::HostProtocolUnavailable,
            );
        }
        let event_digest = command.event_digest;
        let result = run_application_request_interruptible(
            context,
            cancellation,
            store.ingest(command.preflight),
            || {},
        )
        .await;
        match result {
            Ok(Ok(response)) => {
                let Ok(state) = canonical_sha256(&(&event_digest, &response)) else {
                    return terminal_failure(
                        context,
                        LcmAuthorityOperation::Ingest,
                        started_at,
                        "Hermes turn ingest receipt could not be encoded",
                    );
                };
                terminal(
                    context,
                    LcmAuthorityOperation::Ingest,
                    started_at,
                    LcmAuthorityOutcome::Ready,
                    OperationTermination::Completed,
                    Some(state),
                    Some(LcmAuthorityPayload::Ingest(response)),
                    None,
                )
            }
            Ok(Err(_)) => terminal_failure(
                context,
                LcmAuthorityOperation::Ingest,
                started_at,
                "Hermes turn ingest failed",
            ),
            Err(interruption) => terminal_interruption(
                context,
                LcmAuthorityOperation::Ingest,
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
        match command.evidence {
            LcmCompressionEvidence::PressureOnly { .. } => unavailable(
                context,
                LcmAuthorityOperation::Compact,
                started_at,
                LcmAuthorityUnavailableReason::HostPayloadUnavailable,
            ),
            LcmCompressionEvidence::ClaudeNativeSummary {
                protocol,
                summary_text,
            } => {
                let LcmHostProtocol::ClaudeCodePostCompact {
                    protocol_revision,
                    event_digest,
                } = protocol
                else {
                    return unavailable(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        LcmAuthorityUnavailableReason::HostProtocolUnavailable,
                    );
                };
                if protocol_revision != "claude.postcompact.v1"
                    || summary_text.trim().is_empty()
                    || !command.preflight.messages.is_empty()
                {
                    return unavailable(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        LcmAuthorityUnavailableReason::HostProtocolUnavailable,
                    );
                }
                let Some(canonical_source) = self.canonical_source.as_ref() else {
                    return unavailable(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        LcmAuthorityUnavailableReason::HostPayloadUnavailable,
                    );
                };
                let hydrated = run_application_request_interruptible(
                    context,
                    cancellation,
                    canonical_source
                        .hydrate(&command.preflight.provider, &command.preflight.session_id),
                    || {},
                )
                .await;
                let source = match hydrated {
                    Ok(CanonicalCompactionSourceOutcome::Ready(source))
                        if !source.messages.is_empty() && !source.anchors.is_empty() =>
                    {
                        source
                    }
                    Ok(_) => {
                        return unavailable(
                            context,
                            LcmAuthorityOperation::Compact,
                            started_at,
                            LcmAuthorityUnavailableReason::HostPayloadUnavailable,
                        );
                    }
                    Err(interruption) => {
                        return terminal_interruption(
                            context,
                            LcmAuthorityOperation::Compact,
                            started_at,
                            interruption,
                            CancellationStage::DuringRead,
                            None,
                        );
                    }
                };
                let source_state = source.snapshot_state.clone();
                let source_anchors = source.anchors.clone();
                let request = compression_request(command.preflight, source.messages, summary_text);
                let compacted = run_application_request_interruptible(
                    context,
                    cancellation,
                    store.compact(request),
                    || {},
                )
                .await;
                match compacted {
                    Ok(Ok(response)) => {
                        let Ok(state) = canonical_sha256(&(
                            event_digest,
                            source_state,
                            source_anchors,
                            &response,
                        )) else {
                            return terminal_failure(
                                context,
                                LcmAuthorityOperation::Compact,
                                started_at,
                                "LCM atomic compaction receipt could not be encoded",
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
                    Ok(Err(_)) => terminal_failure(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        "LCM atomic compaction failed",
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

    async fn execute_doctor(
        &self,
        context: &RequestContext,
        cancellation: &CancellationToken,
        started_at: UtcMicros,
        query: LcmDoctorQuery,
    ) -> LcmAuthorityResponse {
        let Some(store) = self.store.as_ref() else {
            return unavailable(
                context,
                LcmAuthorityOperation::Doctor,
                started_at,
                LcmAuthorityUnavailableReason::StoreAuthorityUnavailable,
            );
        };
        let result = run_application_request_interruptible(
            context,
            cancellation,
            store.doctor(query),
            || {},
        )
        .await;
        match result {
            Ok(Ok(report)) => {
                let Ok(state) = canonical_sha256(&report) else {
                    return terminal_failure(
                        context,
                        LcmAuthorityOperation::Doctor,
                        started_at,
                        "LCM Doctor receipt could not be encoded",
                    );
                };
                terminal(
                    context,
                    LcmAuthorityOperation::Doctor,
                    started_at,
                    LcmAuthorityOutcome::Ready,
                    OperationTermination::Completed,
                    Some(state),
                    Some(LcmAuthorityPayload::Doctor(report)),
                    None,
                )
            }
            Ok(Err(_)) => terminal_failure(
                context,
                LcmAuthorityOperation::Doctor,
                started_at,
                "LCM Doctor read failed",
            ),
            Err(interruption) => terminal_interruption(
                context,
                LcmAuthorityOperation::Doctor,
                started_at,
                interruption,
                CancellationStage::EffectInFlight,
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

#[cfg(test)]
mod tests;
