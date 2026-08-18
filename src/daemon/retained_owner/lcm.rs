//! Direct typed retained LCM reads over the mounted daemon authority.

use std::sync::Arc;

use tracedecay_application::retained_surfaces::{
    LcmAuthorityOutcomeV1, LcmConfigStatusV1, LcmDagDepthStatusV1, LcmDagStatusV1,
    LcmDescribeRequestV1, LcmDoctorFindingKindV1, LcmDoctorFindingV1, LcmDoctorHealthStatusV1,
    LcmDoctorHealthV1, LcmDoctorRequestV1, LcmDoctorResultV1, LcmExpandQueryRequestV1,
    LcmExpandRequestV1, LcmGrepRequestV1, LcmLifecycleStatusV1, LcmLoadSessionRequestV1,
    LcmPayloadCoverageStateV1, LcmPayloadCoverageV1, LcmPayloadGcStatusV1, LcmPayloadStatusV1,
    LcmRedactionStatusV1, LcmRoleV1, LcmStatusRequestV1, LcmStatusResultV1, LcmStatusV1,
    LcmStoreStatusV1, LcmStoreTokenCoverageV1, LcmTemporalModeV1, MessageRelationshipScopeV1,
    MessageTypeFilterV1, RetainedOutcomeStatusV1, RetainedSurfaceOperation,
    RetainedSurfaceResultV1, RetainedTimeFilterV1,
};
use tracedecay_application::{
    ApplicationOutcome, CancellationSignal, RequestContext, RetainedLcmExecutionPortV1,
    RetainedLcmRequestV1, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    RetainedSurfaceExecutionFutureV1,
};
use tracedecay_domain::{SessionId, TemporalModeV1, UtcMicros};
use tracedecay_global_db::{
    SessionTemporalHealthFindingKind, SessionTemporalHealthReport, SessionTemporalHealthStatus,
};
use tracedecay_sessions::runtime::lcm::LcmStatus;
use tracedecay_sessions::runtime::lcm::types::LcmPayloadCoverageState;
use tracedecay_sessions::runtime::{SessionMessageType, SessionSearchScope};
use tracedecay_usecases::session::lcm::{
    LcmAuthorityOperation, LcmAuthorityOutcome, LcmAuthorityPayload, LcmAuthorityRequest,
    LcmAuthorityResponse, LcmAuthorityUnavailableReason, LcmDoctorQuery, LcmStatusQuery,
};

use super::receipts::evidence_outcome;
use crate::daemon::lcm_authority::MountedLcmAuthorityPort;
use crate::daemon::session_retrieval::{
    DaemonSessionRetrievalService, LcmDescribeServiceCommand, LcmDescribeServiceFuture,
    LcmExpandServiceCommand, LcmExpandServiceFuture, SessionApplicationRetrievalFutureV1,
    SessionApplicationRetrievalPortV1, SessionRetrievalStoreScope,
};
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::timeutil::SearchTimeBound;
use tracedecay_usecases::context::ResolvedSessionIdentity;
use tracedecay_usecases::session::SessionTemporalQuery;

mod output;
mod retrieval;

enum DirectRetainedLcmAuthority<'a> {
    /// Project-open mounts are already owned by the server and can be held
    /// for the lifetime of the static retained-surface registration.  This
    /// is the only project path; it never discovers or opens another store.
    Project {
        authority: Arc<dyn MountedLcmAuthorityPort>,
        retrieval: Arc<dyn SessionApplicationRetrievalPortV1>,
    },
    /// Profile retained calls borrow the daemon-wide registry and resolve the
    /// already-registered profile shard through its canonical runtime port.
    Profile {
        authority: Option<&'a dyn MountedLcmAuthorityPort>,
        registry: &'a DaemonSessionRuntimeRegistryV1,
        identity: ResolvedSessionIdentity,
    },
}

pub(super) struct DirectRetainedLcmPortV1<'a> {
    authority: DirectRetainedLcmAuthority<'a>,
}

enum ResolvedRetainedLcmAuthority<'a> {
    Borrowed(&'a dyn MountedLcmAuthorityPort),
    Owned(Arc<dyn MountedLcmAuthorityPort>),
}

impl ResolvedRetainedLcmAuthority<'_> {
    fn as_ref(&self) -> &dyn MountedLcmAuthorityPort {
        match self {
            Self::Borrowed(authority) => *authority,
            Self::Owned(authority) => authority.as_ref(),
        }
    }
}

/// The retained LCM projection helpers are shared by profile and project
/// mounts.  They were originally written for the profile route, so this
/// narrow adapter rewrites only the typed store-scope field before delegating
/// to the already-admitted retrieval authority.  Query data, identity, and
/// cancellation remain owned by that authority.
struct ScopedRetrieval<'a> {
    inner: &'a dyn SessionApplicationRetrievalPortV1,
    scope: SessionRetrievalStoreScope,
    cancellation: &'a CancellationSignal,
}

impl<'a> ScopedRetrieval<'a> {
    fn new(
        inner: &'a dyn SessionApplicationRetrievalPortV1,
        scope: SessionRetrievalStoreScope,
        cancellation: &'a CancellationSignal,
    ) -> Self {
        Self {
            inner,
            scope,
            cancellation,
        }
    }
}

impl SessionApplicationRetrievalPortV1 for ScopedRetrieval<'_> {
    fn retrieve_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        query: SessionTemporalQuery,
    ) -> SessionApplicationRetrievalFutureV1<'a> {
        self.inner
            .retrieve_admitted_with_cancellation(context, self.cancellation, query)
    }

    fn describe_lcm_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        _cancellation: &'a CancellationSignal,
        command: LcmDescribeServiceCommand,
    ) -> LcmDescribeServiceFuture<'a> {
        let command = LcmDescribeServiceCommand::new(
            command.provider().to_owned(),
            command.session_id().clone(),
            command.target().clone(),
            command.grain(),
            self.scope,
        );
        self.inner
            .describe_lcm_admitted(context, self.cancellation, command)
    }

    fn expand_lcm_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        _cancellation: &'a CancellationSignal,
        command: LcmExpandServiceCommand,
    ) -> LcmExpandServiceFuture<'a> {
        let command = LcmExpandServiceCommand::new(
            command.provider().to_owned(),
            command.session_id().clone(),
            command.target().clone(),
            command.grain(),
            command.content_slice(),
            command.source_limit(),
            command.cursor().map(str::to_owned),
            self.scope,
        );
        self.inner
            .expand_lcm_admitted(context, self.cancellation, command)
    }
}

enum RetainedLcmRetrieval<'a> {
    Mounted(ScopedRetrieval<'a>),
    Profile {
        service: Box<DaemonSessionRetrievalService>,
        cancellation: &'a CancellationSignal,
    },
}

impl RetainedLcmRetrieval<'_> {
    async fn load_session(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &LcmLoadSessionRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        match self {
            Self::Mounted(service) => {
                retrieval::execute_load_session(Some(service), context, request).await
            }
            Self::Profile {
                service,
                cancellation,
            } => {
                let service = ScopedRetrieval::new(
                    service.as_ref(),
                    SessionRetrievalStoreScope::Profile,
                    cancellation,
                );
                retrieval::execute_load_session(Some(&service), context, request).await
            }
        }
    }

    async fn grep(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &LcmGrepRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        match self {
            Self::Mounted(service) => {
                retrieval::execute_grep(Some(service), context, request).await
            }
            Self::Profile {
                service,
                cancellation,
            } => {
                let service = ScopedRetrieval::new(
                    service.as_ref(),
                    SessionRetrievalStoreScope::Profile,
                    cancellation,
                );
                retrieval::execute_grep(Some(&service), context, request).await
            }
        }
    }

    async fn describe(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &LcmDescribeRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        match self {
            Self::Mounted(service) => {
                retrieval::execute_describe(Some(service), context, request).await
            }
            Self::Profile {
                service,
                cancellation,
            } => {
                let service = ScopedRetrieval::new(
                    service.as_ref(),
                    SessionRetrievalStoreScope::Profile,
                    cancellation,
                );
                retrieval::execute_describe(Some(&service), context, request).await
            }
        }
    }

    async fn expand(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &LcmExpandRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        match self {
            Self::Mounted(service) => {
                retrieval::execute_expand(Some(service), context, request).await
            }
            Self::Profile {
                service,
                cancellation,
            } => {
                let service = ScopedRetrieval::new(
                    service.as_ref(),
                    SessionRetrievalStoreScope::Profile,
                    cancellation,
                );
                retrieval::execute_expand(Some(&service), context, request).await
            }
        }
    }

    async fn expand_query(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &LcmExpandQueryRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        match self {
            Self::Mounted(service) => {
                retrieval::execute_expand_query(Some(service), context, request).await
            }
            Self::Profile {
                service,
                cancellation,
            } => {
                let service = ScopedRetrieval::new(
                    service.as_ref(),
                    SessionRetrievalStoreScope::Profile,
                    cancellation,
                );
                retrieval::execute_expand_query(Some(&service), context, request).await
            }
        }
    }
}

impl<'a> DirectRetainedLcmPortV1<'a> {
    pub(super) fn profile(
        registry: &'a DaemonSessionRuntimeRegistryV1,
        identity: ResolvedSessionIdentity,
        authority: Option<&'a dyn MountedLcmAuthorityPort>,
    ) -> Self {
        Self {
            authority: DirectRetainedLcmAuthority::Profile {
                authority,
                registry,
                identity,
            },
        }
    }

    /// Own the two independently mounted project authorities.  The factory
    /// must pass the exact `project_lcm_authority` and
    /// `project_session_application_retrieval_service` from the project
    /// server; no path, registry, or database is accepted here.
    pub(super) fn project(
        authority: Arc<dyn MountedLcmAuthorityPort>,
        retrieval: Arc<dyn SessionApplicationRetrievalPortV1>,
    ) -> Self {
        Self {
            authority: DirectRetainedLcmAuthority::Project {
                authority,
                retrieval,
            },
        }
    }

    async fn lcm_authority<'b>(
        &'b self,
        context: &'b RetainedSurfaceExecutionContextV1<'b>,
    ) -> Result<ResolvedRetainedLcmAuthority<'b>, RetainedSurfaceExecutionErrorV1> {
        match &self.authority {
            DirectRetainedLcmAuthority::Project { authority, .. } => {
                Ok(ResolvedRetainedLcmAuthority::Borrowed(authority.as_ref()))
            }
            DirectRetainedLcmAuthority::Profile {
                authority: Some(authority),
                ..
            } => Ok(ResolvedRetainedLcmAuthority::Borrowed(*authority)),
            DirectRetainedLcmAuthority::Profile {
                authority: None,
                registry,
                identity,
            } => {
                let database =
                    super::bounded_execution(context, registry.profile_sessions()).await?;
                let expected_shard = database.binding().shard_id.clone();
                crate::daemon::lcm_authority::mount_registered_lcm_authority(
                    database,
                    identity.clone(),
                    &expected_shard,
                )
                .map(ResolvedRetainedLcmAuthority::Owned)
                .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)
            }
        }
    }

    async fn retrieval_service<'b>(
        &'b self,
        context: &'b RetainedSurfaceExecutionContextV1<'b>,
    ) -> Result<RetainedLcmRetrieval<'b>, RetainedSurfaceExecutionErrorV1> {
        match &self.authority {
            DirectRetainedLcmAuthority::Project { retrieval, .. } => {
                Ok(RetainedLcmRetrieval::Mounted(ScopedRetrieval::new(
                    retrieval.as_ref(),
                    SessionRetrievalStoreScope::Project,
                    context.cancellation_signal,
                )))
            }
            DirectRetainedLcmAuthority::Profile {
                registry, identity, ..
            } => {
                let database =
                    super::bounded_execution(context, registry.profile_sessions()).await?;
                let service =
                    DaemonSessionRetrievalService::new_admitted_profile(database, identity.clone())
                        .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
                Ok(RetainedLcmRetrieval::Profile {
                    service: Box::new(service),
                    cancellation: context.cancellation_signal,
                })
            }
        }
    }

    async fn execute_status(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &LcmStatusRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        let provider = match request.provider.as_deref() {
            Some(provider) if !provider.trim().is_empty() => provider.trim(),
            Some(_) => return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest),
            None => "all",
        };
        let session_id = request
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty());
        let authority = self.lcm_authority(context).await?;
        let response = authority
            .as_ref()
            .execute_admitted(
                context.request_context,
                context.cancellation_signal,
                LcmAuthorityRequest::Status(LcmStatusQuery {
                    provider: provider.to_owned(),
                    session_id: session_id.map(str::to_owned),
                    deep: request.deep.unwrap_or(false),
                }),
            )
            .await;
        validate_receipt(context, &response, LcmAuthorityOperation::Status)?;
        let authority_outcome = lcm_authority_outcome(response.outcome.clone());
        let status = match (response.outcome, response.payload) {
            (LcmAuthorityOutcome::Ready, Some(LcmAuthorityPayload::Status(status))) => status,
            (outcome, _) => return Err(execution_error(outcome)),
        };
        evidence_outcome(
            context,
            RetainedSurfaceOperation::LcmStatus,
            RetainedSurfaceResultV1::LcmStatus(LcmStatusResultV1 {
                status: RetainedOutcomeStatusV1::Ok,
                authority_outcome: Some(authority_outcome),
                deep: Some(request.deep.unwrap_or(false)),
                lcm: Some(lcm_status(status)),
                message: None,
                provider: Some(provider.to_owned()),
                reason: None,
                session_id: session_id.map(str::to_owned),
            }),
        )
    }

    async fn execute_doctor(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        _request: &LcmDoctorRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        let authority = self.lcm_authority(context).await?;
        let response = authority
            .as_ref()
            .execute_admitted(
                context.request_context,
                context.cancellation_signal,
                LcmAuthorityRequest::Doctor(LcmDoctorQuery),
            )
            .await;
        validate_receipt(context, &response, LcmAuthorityOperation::Doctor)?;
        let authority_outcome = lcm_authority_outcome(response.outcome.clone());
        let report = match (response.outcome, response.payload) {
            (LcmAuthorityOutcome::Ready, Some(LcmAuthorityPayload::Doctor(report))) => report,
            (outcome, _) => return Err(execution_error(outcome)),
        };
        let report = serde_json::from_value(report)
            .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
        let health = lcm_doctor_health(report);
        let status = match health.status {
            LcmDoctorHealthStatusV1::Complete => RetainedOutcomeStatusV1::Complete,
            LcmDoctorHealthStatusV1::Partial => RetainedOutcomeStatusV1::Partial,
            LcmDoctorHealthStatusV1::Unavailable => RetainedOutcomeStatusV1::Unavailable,
            LcmDoctorHealthStatusV1::Locked => RetainedOutcomeStatusV1::Locked,
        };
        evidence_outcome(
            context,
            RetainedSurfaceOperation::LcmDoctor,
            RetainedSurfaceResultV1::LcmDoctor(LcmDoctorResultV1 {
                status,
                authority_outcome,
                health: Some(health),
                reason: None,
            }),
        )
    }
}

impl RetainedLcmExecutionPortV1 for DirectRetainedLcmPortV1<'_> {
    fn execute_lcm<'a>(
        &'a self,
        context: RetainedSurfaceExecutionContextV1<'a>,
        request: RetainedLcmRequestV1<'a>,
    ) -> RetainedSurfaceExecutionFutureV1<'a> {
        Box::pin(async move {
            match request {
                RetainedLcmRequestV1::Status(request) => {
                    self.execute_status(&context, request).await
                }
                RetainedLcmRequestV1::Doctor(request) => {
                    self.execute_doctor(&context, request).await
                }
                RetainedLcmRequestV1::LoadSession(request) => {
                    let service = self.retrieval_service(&context).await?;
                    service.load_session(&context, request).await
                }
                RetainedLcmRequestV1::Grep(request) => {
                    let service = self.retrieval_service(&context).await?;
                    service.grep(&context, request).await
                }
                RetainedLcmRequestV1::Describe(request) => {
                    let service = self.retrieval_service(&context).await?;
                    service.describe(&context, request).await
                }
                RetainedLcmRequestV1::Expand(request) => {
                    let service = self.retrieval_service(&context).await?;
                    service.expand(&context, request).await
                }
                RetainedLcmRequestV1::ExpandQuery(request) => {
                    let service = self.retrieval_service(&context).await?;
                    service.expand_query(&context, request).await
                }
            }
        })
    }
}

fn validate_receipt(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    response: &LcmAuthorityResponse,
    operation: LcmAuthorityOperation,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    let receipt = &response.receipt;
    let request = context.request_context;
    if receipt.operation != operation
        || receipt.request_id != *request.request_id()
        || receipt.grant_id != request.grant().grant_id
        || receipt.grant_revision != request.grant().revision
        || receipt.grant_digest != request.grant().digest
        || receipt.authorized_scope_digest != request.scope().scope_digest
        || receipt.cancellation_token_id != request.cancellation().token_id
        || receipt.execution.effective_deadline != *request.deadline()
    {
        return Err(RetainedSurfaceExecutionErrorV1::Unavailable);
    }
    Ok(())
}

fn execution_error(outcome: LcmAuthorityOutcome) -> RetainedSurfaceExecutionErrorV1 {
    match outcome {
        LcmAuthorityOutcome::Denied => RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized,
        LcmAuthorityOutcome::Cancelled => RetainedSurfaceExecutionErrorV1::Cancelled(
            tracedecay_application::CancellationStage::DuringRead,
        ),
        LcmAuthorityOutcome::TimedOut => RetainedSurfaceExecutionErrorV1::TimedOut(
            tracedecay_application::CancellationStage::DuringRead,
        ),
        LcmAuthorityOutcome::Ready
        | LcmAuthorityOutcome::Unavailable { .. }
        | LcmAuthorityOutcome::Failed { .. } => RetainedSurfaceExecutionErrorV1::Unavailable,
    }
}

fn lcm_authority_outcome(value: LcmAuthorityOutcome) -> LcmAuthorityOutcomeV1 {
    match value {
        LcmAuthorityOutcome::Ready => LcmAuthorityOutcomeV1::Ready,
        LcmAuthorityOutcome::Denied => LcmAuthorityOutcomeV1::Denied,
        LcmAuthorityOutcome::Cancelled => LcmAuthorityOutcomeV1::Cancelled,
        LcmAuthorityOutcome::TimedOut => LcmAuthorityOutcomeV1::TimedOut,
        LcmAuthorityOutcome::Unavailable { reason } => LcmAuthorityOutcomeV1::Unavailable {
            reason: match reason {
                LcmAuthorityUnavailableReason::StoreAuthorityUnavailable => {
                    "store_authority_unavailable"
                }
                LcmAuthorityUnavailableReason::HostProtocolUnavailable => {
                    "host_protocol_unavailable"
                }
                LcmAuthorityUnavailableReason::HostPayloadUnavailable => "host_payload_unavailable",
            }
            .to_owned(),
        },
        LcmAuthorityOutcome::Failed { diagnostic } => LcmAuthorityOutcomeV1::Failed { diagnostic },
    }
}

fn lcm_status(value: LcmStatus) -> LcmStatusV1 {
    LcmStatusV1 {
        schema_version: value.schema_version,
        raw_message_count: value.raw_message_count,
        summary_node_count: value.summary_node_count,
        external_payload_count: value.external_payload_count,
        missing_payload_count: value.missing_payload_count,
        unreferenced_payload_count: value.unreferenced_payload_count,
        maintenance_debt_count: value.maintenance_debt_count,
        store: LcmStoreStatusV1 {
            messages: value.store.messages,
            estimated_tokens: value.store.estimated_tokens,
            token_estimate: LcmStoreTokenCoverageV1 {
                complete: value.store.token_estimate.complete,
                scanned_messages: value.store.token_estimate.scanned_messages,
                next_after_store_id: value.store.token_estimate.next_after_store_id,
            },
        },
        dag: LcmDagStatusV1 {
            total_nodes: value.dag.total_nodes,
            total_tokens: value.dag.total_tokens,
            total_source_tokens: value.dag.total_source_tokens,
            compression_ratio: value.dag.compression_ratio,
            depths: value
                .dag
                .depths
                .into_iter()
                .map(|(depth, status)| {
                    (
                        depth,
                        LcmDagDepthStatusV1 {
                            count: status.count,
                            tokens: status.tokens,
                            source_tokens: status.source_tokens,
                        },
                    )
                })
                .collect(),
        },
        config: LcmConfigStatusV1 {
            fresh_tail_count: value.config.fresh_tail_count,
            summary_fan_in: value.config.summary_fan_in,
            compression_boundary_cooldown_seconds: value
                .config
                .compression_boundary_cooldown_seconds,
        },
        payload: LcmPayloadStatusV1 {
            coverage: LcmPayloadCoverageV1 {
                state: match value.payload.coverage.state {
                    LcmPayloadCoverageState::Complete => LcmPayloadCoverageStateV1::Complete,
                    LcmPayloadCoverageState::Partial => LcmPayloadCoverageStateV1::Partial,
                },
                scanned_metadata_refs: value.payload.coverage.scanned_metadata_refs,
                scanned_files: value.payload.coverage.scanned_files,
                reason: value.payload.coverage.reason,
            },
            externalized_count: value.payload.externalized_count,
            missing_count: value.payload.missing_count,
            unreferenced_count: value.payload.unreferenced_count,
            placeholder_ref_count: value.payload.placeholder_ref_count,
            missing_placeholder_metadata_count: value.payload.missing_placeholder_metadata_count,
            missing_placeholder_file_count: value.payload.missing_placeholder_file_count,
            gc_candidate_count: value.payload.gc_candidate_count,
            root_contained: value.payload.root_contained,
            orphan_file_count: value.payload.orphan_file_count,
            tombstoned_count: value.payload.tombstoned_count,
            referenced_count: value.payload.referenced_count,
            total_bytes: value.payload.total_bytes,
            referenced_bytes: value.payload.referenced_bytes,
            orphan_file_bytes: value.payload.orphan_file_bytes,
            reclaimable_bytes: value.payload.reclaimable_bytes,
            reclaimable_bytes_after_grace: value.payload.reclaimable_bytes_after_grace,
            integrity_mismatch_count: value.payload.integrity_mismatch_count,
        },
        payload_gc: LcmPayloadGcStatusV1 {
            last_gc_at: value.payload_gc.last_gc_at,
            last_gc_duration_ms: value.payload_gc.last_gc_duration_ms,
            last_gc_status: value.payload_gc.last_gc_status,
            last_gc_error: value.payload_gc.last_gc_error,
            last_reaped_refs: value.payload_gc.last_reaped_refs,
            last_reaped_bytes: value.payload_gc.last_reaped_bytes,
            grace_seconds: value.payload_gc.grace_seconds,
            reap_missing_metadata_after_seconds: value
                .payload_gc
                .reap_missing_metadata_after_seconds,
            next_run_eligible_at: value.payload_gc.next_run_eligible_at,
        },
        lifecycle: LcmLifecycleStatusV1 {
            lifecycle_state_count: value.lifecycle.lifecycle_state_count,
            frontier_count: value.lifecycle.frontier_count,
            maintenance_debt_count: value.lifecycle.maintenance_debt_count,
            current_session_id: value.lifecycle.current_session_id,
            current_frontier_store_id: value.lifecycle.current_frontier_store_id,
            last_finalized_session_id: value.lifecycle.last_finalized_session_id,
            last_finalized_frontier_store_id: value.lifecycle.last_finalized_frontier_store_id,
        },
        redaction: LcmRedactionStatusV1 {
            enabled: value.redaction.enabled,
            lossy_records: value.redaction.lossy_records,
            legacy_truncated_count: value.redaction.legacy_truncated_count,
        },
    }
}

fn lcm_doctor_health(value: SessionTemporalHealthReport) -> LcmDoctorHealthV1 {
    LcmDoctorHealthV1 {
        status: match value.status() {
            SessionTemporalHealthStatus::Complete => LcmDoctorHealthStatusV1::Complete,
            SessionTemporalHealthStatus::Partial => LcmDoctorHealthStatusV1::Partial,
            SessionTemporalHealthStatus::Unavailable => LcmDoctorHealthStatusV1::Unavailable,
            SessionTemporalHealthStatus::Locked => LcmDoctorHealthStatusV1::Locked,
        },
        findings: value
            .findings()
            .iter()
            .map(|finding| LcmDoctorFindingV1 {
                kind: lcm_doctor_finding_kind(finding.kind()),
                count: finding.count(),
            })
            .collect(),
        reason: value.reason().map(str::to_owned),
    }
}

const fn lcm_doctor_finding_kind(
    value: SessionTemporalHealthFindingKind,
) -> LcmDoctorFindingKindV1 {
    match value {
        SessionTemporalHealthFindingKind::TriggerAuditDrift => {
            LcmDoctorFindingKindV1::TriggerAuditDrift
        }
        SessionTemporalHealthFindingKind::OccurrenceFtsCorruption => {
            LcmDoctorFindingKindV1::OccurrenceFtsCorruption
        }
        SessionTemporalHealthFindingKind::SummaryFtsCorruption => {
            LcmDoctorFindingKindV1::SummaryFtsCorruption
        }
        SessionTemporalHealthFindingKind::MissingAnchor => LcmDoctorFindingKindV1::MissingAnchor,
        SessionTemporalHealthFindingKind::MissingReceipt => LcmDoctorFindingKindV1::MissingReceipt,
        SessionTemporalHealthFindingKind::InvalidGeneration => {
            LcmDoctorFindingKindV1::InvalidGeneration
        }
        SessionTemporalHealthFindingKind::MultiActiveGeneration => {
            LcmDoctorFindingKindV1::MultiActiveGeneration
        }
        SessionTemporalHealthFindingKind::CursorChainAbsent => {
            LcmDoctorFindingKindV1::CursorChainAbsent
        }
        SessionTemporalHealthFindingKind::CursorKeyAbsent => {
            LcmDoctorFindingKindV1::CursorKeyAbsent
        }
        SessionTemporalHealthFindingKind::OwnershipDrift => LcmDoctorFindingKindV1::OwnershipDrift,
        SessionTemporalHealthFindingKind::StuckRefresh => LcmDoctorFindingKindV1::StuckRefresh,
        SessionTemporalHealthFindingKind::StuckBinding => LcmDoctorFindingKindV1::StuckBinding,
        SessionTemporalHealthFindingKind::StuckProgress => LcmDoctorFindingKindV1::StuckProgress,
        SessionTemporalHealthFindingKind::StuckReceipt => LcmDoctorFindingKindV1::StuckReceipt,
        SessionTemporalHealthFindingKind::MigrationGap => LcmDoctorFindingKindV1::MigrationGap,
        SessionTemporalHealthFindingKind::CompatibilityDrift => {
            LcmDoctorFindingKindV1::CompatibilityDrift
        }
        SessionTemporalHealthFindingKind::RelationGraphUnavailable => {
            LcmDoctorFindingKindV1::RelationGraphUnavailable
        }
        SessionTemporalHealthFindingKind::RelationGraphCorruption => {
            LcmDoctorFindingKindV1::RelationGraphCorruption
        }
        SessionTemporalHealthFindingKind::RelationGraphCycle => {
            LcmDoctorFindingKindV1::RelationGraphCycle
        }
        SessionTemporalHealthFindingKind::StaleSummaryClosure => {
            LcmDoctorFindingKindV1::StaleSummaryClosure
        }
    }
}

fn required(value: &str) -> Result<&str, RetainedSurfaceExecutionErrorV1> {
    let value = value.trim();
    if value.is_empty() {
        Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
    } else {
        Ok(value)
    }
}

fn bounded_text(value: &str, max: usize) -> Result<&str, RetainedSurfaceExecutionErrorV1> {
    let value = required(value)?;
    if value.chars().count() > max {
        Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
    } else {
        Ok(value)
    }
}

fn trimmed(value: Option<&str>) -> Result<Option<&str>, RetainedSurfaceExecutionErrorV1> {
    value.map(required).transpose()
}

fn specific_provider(value: &str) -> Result<&str, RetainedSurfaceExecutionErrorV1> {
    let value = required(value)?;
    if value == "all" {
        Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
    } else {
        Ok(value)
    }
}

fn optional_provider(value: Option<&str>) -> Result<Option<&str>, RetainedSurfaceExecutionErrorV1> {
    Ok(match trimmed(value)? {
        Some("all") | None => None,
        value => value,
    })
}

fn session_id(value: &str) -> Result<SessionId, RetainedSurfaceExecutionErrorV1> {
    SessionId::new(required(value)?).map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)
}

fn cursor(value: Option<&str>) -> Result<Option<String>, RetainedSurfaceExecutionErrorV1> {
    Ok(trimmed(value)?.map(str::to_owned))
}

fn optional_usize(value: Option<u64>) -> Result<Option<usize>, RetainedSurfaceExecutionErrorV1> {
    value
        .map(usize::try_from)
        .transpose()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)
}

fn unsigned_i64(value: Option<u64>) -> Result<Option<i64>, RetainedSurfaceExecutionErrorV1> {
    value
        .map(i64::try_from)
        .transpose()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)
}

fn temporal_mode(
    mode: Option<LcmTemporalModeV1>,
    as_of: Option<u64>,
    default: TemporalModeV1,
) -> Result<TemporalModeV1, RetainedSurfaceExecutionErrorV1> {
    match mode {
        None => Ok(default),
        Some(LcmTemporalModeV1::Current) => Ok(TemporalModeV1::Current),
        Some(LcmTemporalModeV1::Evolution) => Ok(TemporalModeV1::Evolution),
        Some(LcmTemporalModeV1::Forensic) => Ok(TemporalModeV1::Forensic),
        Some(LcmTemporalModeV1::AsOf) => Ok(TemporalModeV1::AsOf {
            cutoff: UtcMicros(
                i64::try_from(as_of.ok_or(RetainedSurfaceExecutionErrorV1::InvalidRequest)?)
                    .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
            ),
        }),
    }
}

fn relationship_scope(value: Option<MessageRelationshipScopeV1>) -> SessionSearchScope {
    match value.unwrap_or(MessageRelationshipScopeV1::All) {
        MessageRelationshipScopeV1::All => SessionSearchScope::All,
        MessageRelationshipScopeV1::ParentsOnly => SessionSearchScope::ParentsOnly,
        MessageRelationshipScopeV1::SubagentsOnly => SessionSearchScope::SubagentsOnly,
    }
}

fn message_type(value: Option<MessageTypeFilterV1>) -> SessionMessageType {
    match value.unwrap_or(MessageTypeFilterV1::All) {
        MessageTypeFilterV1::All => SessionMessageType::All,
        MessageTypeFilterV1::DirectUser => SessionMessageType::DirectUser,
        MessageTypeFilterV1::ToolResult => SessionMessageType::ToolResult,
    }
}

const fn role_name(value: LcmRoleV1) -> &'static str {
    match value {
        LcmRoleV1::System => "system",
        LcmRoleV1::User => "user",
        LcmRoleV1::Assistant => "assistant",
        LcmRoleV1::Tool => "tool",
        LcmRoleV1::Unknown => "unknown",
    }
}

fn time_filter(
    value: Option<&RetainedTimeFilterV1>,
    bound: SearchTimeBound,
) -> Result<Option<i64>, RetainedSurfaceExecutionErrorV1> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = match value {
        RetainedTimeFilterV1::Micros(value) => i64::try_from(*value).ok(),
        RetainedTimeFilterV1::Expression(value) => {
            let value = value.trim();
            value
                .parse::<i64>()
                .ok()
                .filter(|value| *value >= 0)
                .or_else(|| {
                    crate::timeutil::parse_search_time_filter_bound(
                        value,
                        crate::tracedecay::current_timestamp(),
                        bound,
                    )
                })
        }
    };
    parsed
        .map(Some)
        .ok_or(RetainedSurfaceExecutionErrorV1::InvalidRequest)
}
