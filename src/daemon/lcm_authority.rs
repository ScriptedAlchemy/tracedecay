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
use tracedecay_domain::{UtcMicros, canonical_sha256};
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
    LcmAuthorityUnavailableReason, LcmCompactionCommand, LcmDoctorQuery, LcmStatusQuery,
    LcmTranscriptIngestCommand, lcm_authority_operation_identity,
};

use crate::global_db::RegisteredGlobalDb;

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
        let database = Arc::clone(&self.database);
        Box::pin(async move {
            // Persist the host-completed turn through the canonical
            // compression ingest route (session upsert + protected raw-message
            // ingest). The no-op summarizer stops before any summary is
            // minted, so ingest commits raw turn content and nothing else.
            let turn = turn_ingest_compression_request(request.clone());
            super::lcm_effects::DaemonLcmEffectService::new(Arc::clone(&database), None, None)
                .compress(turn)
                .await?;
            database.lcm_preflight(request).await
        })
    }

    fn compact(&self, request: LcmCompressionRequest) -> StoreFuture<'_, LcmCompressionResponse> {
        let database = Arc::clone(&self.database);
        Box::pin(async move {
            super::lcm_effects::DaemonLcmEffectService::new(database, None, None)
                .compress(request)
                .await
        })
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
            serde_json::to_value(self.database.session_temporal_doctor_health().await)
                .map_err(|error| LcmError::Db(error.to_string()))
        })
    }
}

/// Compression driven purely by host pressure evidence.
///
/// The daemon compresses already-ingested canonical content: host-supplied
/// messages are never trusted for compaction, and the summary comes from the
/// daemon's authoritative summarization route (native evidence or a provider
/// auxiliary summarizer), never from caller-authored text.
fn pressure_compression_request(preflight: LcmPreflightRequest) -> LcmCompressionRequest {
    compression_request_from_preflight(preflight, Vec::new(), LcmSummarizerMode::HermesAuxiliary)
}

/// Durable ingest of a host-completed turn: the canonical compression route
/// upserts the session and protected raw messages, and the no-op summarizer
/// guarantees ingest never mints summary state.
fn turn_ingest_compression_request(mut preflight: LcmPreflightRequest) -> LcmCompressionRequest {
    let messages = std::mem::take(&mut preflight.messages);
    compression_request_from_preflight(preflight, messages, LcmSummarizerMode::Noop)
}

fn compression_request_from_preflight(
    preflight: LcmPreflightRequest,
    messages: Vec<serde_json::Value>,
    summarizer: LcmSummarizerMode,
) -> LcmCompressionRequest {
    LcmCompressionRequest {
        provider: preflight.provider,
        session_id: preflight.session_id,
        messages,
        current_tokens: preflight.current_tokens,
        focus_topic: None,
        ignore_session_patterns: preflight.ignore_session_patterns,
        stateless_session_patterns: preflight.stateless_session_patterns,
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id: None,
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

/// One daemon-owned LCM authority bound to one registered session shard.
///
/// No database accessor is exposed. Reconstructing this value after daemon
/// restart binds a newly fenced handle to the same durable shard and reads the
/// committed LCM frontier/status from storage.
pub(crate) struct DaemonLcmAuthority {
    store: Option<Arc<dyn LcmDaemonStore>>,
}

impl DaemonLcmAuthority {
    pub(crate) fn registered(database: Arc<RegisteredGlobalDb>) -> Self {
        Self {
            store: Some(Arc::new(RegisteredLcmDaemonStore::new(database))),
        }
    }

    #[cfg(test)]
    fn unavailable() -> Self {
        Self { store: None }
    }

    #[cfg(test)]
    fn with_store(store: Arc<dyn LcmDaemonStore>) -> Self {
        Self { store: Some(store) }
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
        let event_digest = command.evidence.protocol().event_digest().clone();
        let request = pressure_compression_request(command.preflight);
        let result = run_application_request_interruptible(
            context,
            cancellation,
            store.compact(request),
            || {},
        )
        .await;
        match result {
            Ok(Ok(response)) => {
                let Ok(state) = canonical_sha256(&(&event_digest, &response)) else {
                    return terminal_failure(
                        context,
                        LcmAuthorityOperation::Compact,
                        started_at,
                        "compaction receipt could not be encoded",
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
            Ok(Err(LcmError::Cancelled)) => terminal(
                context,
                LcmAuthorityOperation::Compact,
                started_at,
                LcmAuthorityOutcome::Cancelled,
                OperationTermination::Cancelled,
                None,
                None,
                None,
            ),
            Ok(Err(LcmError::DeadlineExceeded)) => terminal(
                context,
                LcmAuthorityOperation::Compact,
                started_at,
                LcmAuthorityOutcome::TimedOut,
                OperationTermination::TimedOut,
                None,
                None,
                None,
            ),
            Ok(Err(_)) => terminal_failure(
                context,
                LcmAuthorityOperation::Compact,
                started_at,
                "daemon compaction failed",
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
