use std::sync::{Arc, Mutex};

use serde::Serialize;
use tracedecay_domain::{
    FactOwnerV1, ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceIdentityV1,
    RetrievalAnchorId,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{
    ObservationPersistOutcome, ParseOffset, build_scope_resolution_authorization_v1,
};

use crate::anchor_resolution::{EvidenceAnchorReportResolver, EvidenceAnchorResolutionReport};
use crate::memory::{
    EvidenceAnchorResolutionError, EvidenceAnchorResolver, ResolvedEvidenceAnchorV1,
};
use crate::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};

mod disposition;
mod durability;
mod replay;
mod runtime;
mod schedule;
mod spool;
mod wire;

pub use disposition::{
    HostAdmissionDispositionClass, HostAdmissionStatus, HostAdmissionTelemetryDisposition,
};
pub use durability::{DirectorySyncPolicy, sync_directory};
pub use replay::{ReplayPassDecision, classify_replay_pass, replay_backoff};

pub use runtime::{DurableHostAdmission, HostAdmissionRuntime};
pub type SharedHostAdmissionBroker = Arc<HostAdmissionBroker>;

pub struct HostAdmissionBroker {
    runtime: Arc<Mutex<HostAdmissionRuntime>>,
    replay: tokio::sync::Mutex<()>,
    /// Coalesced wake for daemon-owned profile/project replay workers.
    replay_wake: tokio::sync::Notify,
}

pub struct HostAdmissionReplay<'a> {
    broker: &'a HostAdmissionBroker,
    _guard: tokio::sync::MutexGuard<'a, ()>,
}

impl HostAdmissionBroker {
    pub fn new(runtime: HostAdmissionRuntime) -> Self {
        Self {
            runtime: Arc::new(Mutex::new(runtime)),
            replay: tokio::sync::Mutex::new(()),
            replay_wake: tokio::sync::Notify::new(),
        }
    }

    async fn with_runtime<T, F>(&self, operation: F) -> Result<T, HostAdmissionOutcome>
    where
        T: Send + 'static,
        F: FnOnce(&mut HostAdmissionRuntime) -> Result<T, HostAdmissionOutcome> + Send + 'static,
    {
        let runtime = Arc::clone(&self.runtime);
        tokio::task::spawn_blocking(move || {
            let mut runtime = runtime.lock().map_err(|_| {
                HostAdmissionOutcome::retained_unavailable("spool_runtime_unavailable")
            })?;
            operation(&mut runtime)
        })
        .await
        .unwrap_or_else(|_| {
            Err(HostAdmissionOutcome::retained_unavailable(
                "spool_runtime_unavailable",
            ))
        })
    }

    pub async fn admit(
        &self,
        source: &str,
        payload: &[u8],
    ) -> Result<DurableHostAdmission, HostAdmissionOutcome> {
        let source = source.to_owned();
        let payload = payload.to_vec();
        let admitted = self
            .with_runtime(move |runtime| runtime.admit(&source, &payload))
            .await?;
        self.request_replay();
        Ok(admitted)
    }

    /// Wake any coalesced replay worker without holding client permits.
    pub fn request_replay(&self) {
        // notify_one retains one permit when the worker has not subscribed yet,
        // closing the broker-creation/admission lost-wake window.
        self.replay_wake.notify_one();
    }

    pub async fn wait_for_replay_request(&self) {
        self.replay_wake.notified().await;
    }

    pub async fn pending_replay_count(&self) -> Result<usize, HostAdmissionOutcome> {
        self.with_runtime(|runtime| Ok(runtime.pending_count()))
            .await
    }

    pub async fn has_pending_replay(&self) -> bool {
        self.pending_replay_count()
            .await
            .is_ok_and(|count| count > 0)
    }

    pub async fn begin_replay(&self) -> Result<HostAdmissionReplay<'_>, HostAdmissionOutcome> {
        let guard = self.replay.lock().await;
        self.with_runtime(HostAdmissionRuntime::recover_leases)
            .await?;
        Ok(HostAdmissionReplay {
            broker: self,
            _guard: guard,
        })
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub async fn pending_count(&self) -> usize {
        self.pending_replay_count().await.unwrap_or_default()
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub async fn quarantine_count(&self) -> usize {
        self.with_runtime(|runtime| Ok(runtime.quarantine_count()))
            .await
            .unwrap_or_default()
    }
}

impl HostAdmissionReplay<'_> {
    pub async fn lease_next(&self) -> Result<Option<SpoolRecord>, HostAdmissionOutcome> {
        self.broker
            .with_runtime(HostAdmissionRuntime::try_lease_next)
            .await
    }

    pub async fn defer(&self, seq: u64) -> Result<(), HostAdmissionOutcome> {
        self.broker
            .with_runtime(move |runtime| runtime.defer(seq))
            .await
    }

    pub async fn commit(&self, seq: u64) -> Result<usize, HostAdmissionOutcome> {
        self.broker
            .with_runtime(move |runtime| runtime.commit(seq))
            .await
    }

    pub async fn quarantine(
        &self,
        seq: u64,
        reason: TerminalReason,
    ) -> Result<usize, HostAdmissionOutcome> {
        self.broker
            .with_runtime(move |runtime| runtime.quarantine(seq, reason))
            .await
    }
}

pub(crate) use schedule::{FairEnqueueOutcome, FairScheduleBounds, FairSourceScheduler};
#[allow(unused_imports)]
pub(crate) use spool::{
    DEFAULT_MAX_RECORD_BYTES, DEFAULT_MAX_RECORDS, DEFAULT_MAX_SOURCE_BYTES,
    DEFAULT_MAX_SPOOL_BYTES, HostAdmissionSpool, SpoolError, SpoolIntegrity,
    SpoolOverflowDisposition,
};
pub use spool::{SpoolBounds, SpoolOpenReport, SpoolRecord, TerminalReason};
pub use wire::{
    MAX_MCP_JSONRPC_FRAME_BYTES, MAX_WIRE_MESSAGE_BYTES, MCP_OVERSIZE_ID_INSPECT_BYTES,
    WIRE_RECORD_TOO_LARGE, WireReadOutcome, is_wire_oversized_io_error, read_bounded_mcp_line,
    read_bounded_to_string, wire_oversized_inspect_prefix, wire_oversized_io_error,
    wire_oversized_io_error_with_prefix,
};
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub use wire::{line_outcome_to_io, read_bounded_line};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HostAdmissionOutcome {
    pub status: HostAdmissionStatus,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<&'static str>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostProjectionDrainOutcome {
    pub projected: u64,
    pub projected_outputs: u64,
    pub skipped: u64,
    pub exact_duplicates: u64,
    pub session_ids: Vec<String>,
}

impl HostAdmissionOutcome {
    const fn new(
        status: HostAdmissionStatus,
        retryable: bool,
        reason_code: Option<&'static str>,
    ) -> Self {
        Self {
            status,
            retryable,
            reason_code,
        }
    }

    pub const fn supported() -> Self {
        Self::new(HostAdmissionStatus::Supported, false, None)
    }

    pub const fn accepted_for_replay() -> Self {
        Self::new(HostAdmissionStatus::AcceptedForReplay, false, None)
    }

    pub const fn retained_backpressured(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Backpressured, true, Some(reason_code))
    }

    pub const fn retained_unavailable(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Unavailable, true, Some(reason_code))
    }

    pub const fn degraded(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Degraded, false, Some(reason_code))
    }

    pub const fn replay_completed(changed: bool, exact_duplicate: bool) -> Self {
        if changed {
            Self::new(HostAdmissionStatus::Committed, false, None)
        } else if exact_duplicate {
            Self::new(HostAdmissionStatus::ExactDuplicate, false, None)
        } else {
            Self::accepted_for_replay()
        }
    }

    pub const fn spool_overflow() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_overflow"),
        )
    }

    pub const fn spool_record_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("spool_record_too_large"),
        )
    }

    /// Host-event wire or MCP/daemon JSON-RPC frame exceeded its respective
    /// bound ([`wire::MAX_WIRE_MESSAGE_BYTES`] or
    /// [`wire::MAX_MCP_JSONRPC_FRAME_BYTES`]) before durable retention.
    /// Non-retryable; full payload is not retained.
    pub const fn wire_record_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some(wire::WIRE_RECORD_TOO_LARGE),
        )
    }

    pub const fn spool_source_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("spool_source_too_large"),
        )
    }

    pub const fn spool_corrupted() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("spool_corrupted"),
        )
    }

    pub const fn spool_unsupported_version() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("spool_unsupported_version"),
        )
    }

    pub const fn durable_payload_unsupported_version() -> Self {
        Self::retained_unavailable("host_event_payload_unsupported_version")
    }

    pub const fn durable_payload_malformed() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("host_event_payload_malformed"),
        )
    }

    pub const fn spool_ack_conflict() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_ack_conflict"),
        )
    }

    pub(crate) const fn spool_recovery_required() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("spool_recovery_required"),
        )
    }

    pub const fn quarantine_full() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_quarantine_full"),
        )
    }

    pub(crate) const fn quarantine_corrupted() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("spool_quarantine_corrupted"),
        )
    }

    pub(crate) const fn quarantine_recovery_required() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("spool_quarantine_recovery_required"),
        )
    }
}

pub use tracedecay_global_db::HostAdmissionAuthorities;
pub use tracedecay_sessions::admission::HostAdmissionScope;

pub struct HostAdmissionFacade<'a> {
    core: tracedecay_global_db::HostAdmissionFacade<'a>,
}

impl tracedecay_sessions::admission::HostAdmission for HostAdmissionFacade<'_> {
    fn capture_observation<'a>(
        &'a self,
        request: CaptureObservationRequest,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, CaptureObservationOutcome> {
        tracedecay_sessions::admission::HostAdmission::capture_observation(&self.core, request)
    }

    fn advance_non_durable_source_cursor<'a>(
        &'a self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, CursorAdvanceOutcome> {
        tracedecay_sessions::admission::HostAdmission::advance_non_durable_source_cursor(
            &self.core,
            advance,
            cancellation,
        )
    }

    fn get_source_cursor<'a>(
        &'a self,
        source: &'a ObservationSourceIdentityV1,
        scope: &'a ObservationScopeV1,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Option<ObservationSourceCursorV1>>
    {
        tracedecay_sessions::admission::HostAdmission::get_source_cursor(&self.core, source, scope)
    }

    fn drain_projection_queue<'a>(
        &'a self,
        provider: &'a str,
        scope: &'a ObservationScopeV1,
        cancellation: &'a ObservationCancellation,
        max: usize,
    ) -> tracedecay_sessions::admission::AdmissionFuture<
        'a,
        tracedecay_sessions::admission::HostProjectionDrainOutcome,
    > {
        tracedecay_sessions::admission::HostAdmission::drain_projection_queue(
            &self.core,
            provider,
            scope,
            cancellation,
            max,
        )
    }

    fn has_session_message<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        message_id: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, bool> {
        tracedecay_sessions::admission::HostAdmission::has_session_message(
            &self.core, scope, provider, message_id,
        )
    }

    fn get_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Option<ParseOffset>> {
        tracedecay_sessions::admission::HostAdmission::get_parse_offset(&self.core, scope, path)
    }

    fn advance_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
        offset: ParseOffset,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, ()> {
        tracedecay_sessions::admission::HostAdmission::advance_parse_offset(
            &self.core, scope, path, offset,
        )
    }
}

const fn product_admission_outcome(
    outcome: tracedecay_sessions::admission::HostAdmissionOutcome,
) -> HostAdmissionOutcome {
    HostAdmissionOutcome {
        status: outcome.status,
        retryable: outcome.retryable,
        reason_code: outcome.reason_code,
    }
}

fn product_projection_drain_outcome(
    outcome: tracedecay_sessions::admission::HostProjectionDrainOutcome,
) -> HostProjectionDrainOutcome {
    HostProjectionDrainOutcome {
        projected: outcome.projected,
        projected_outputs: outcome.projected_outputs,
        skipped: outcome.skipped,
        exact_duplicates: outcome.exact_duplicates,
        session_ids: outcome.session_ids,
    }
}

impl<'a> HostAdmissionFacade<'a> {
    pub fn new(authorities: HostAdmissionAuthorities<'a>) -> Self {
        Self {
            core: tracedecay_global_db::HostAdmissionFacade::new(authorities),
        }
    }

    fn authorities(&self) -> &HostAdmissionAuthorities<'a> {
        self.core.authorities()
    }

    #[allow(dead_code)] // evidence-assembly admission port — preserve authority surface
    pub(crate) async fn resolve_evidence_assembly_anchor(
        &self,
        context: &tracedecay_application::RequestContext,
        owner: &tracedecay_store::EvidenceAssemblyOwnerV1,
        anchor_id: &RetrievalAnchorId,
    ) -> tracedecay_store::EvidenceAssemblyStoreResult<
        crate::evidence_assembly::EvidenceAssemblyAnchorResolutionV1,
    > {
        let unavailable = || tracedecay_store::EvidenceAssemblyStoreError::Unavailable;
        let project_id = owner.owner.project_id().ok_or_else(unavailable)?;
        if project_id != &context.scope().project_id {
            return Err(unavailable());
        }
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        self.authorities()
            .validate_scope(&scope)
            .map_err(|_| unavailable())?;
        let database = self
            .authorities()
            .registered_database(HostAdmissionScope::Project)
            .map_err(|_| unavailable())?
            .ok_or_else(unavailable)?;
        crate::evidence_assembly::RuntimeEvidenceAssemblyStore::new(
            database.binding().shard_id.profile_id.clone(),
            database.runtime().clone(),
            database.authority().clone(),
        )?
        .resolve_anchor(context, owner, anchor_id)
        .await
    }

    pub fn probe(&self, provider: &str, scope: HostAdmissionScope) -> HostAdmissionOutcome {
        product_admission_outcome(self.core.probe(provider, scope))
    }

    pub fn accept_replay(&self, provider: &str, scope: HostAdmissionScope) -> HostAdmissionOutcome {
        product_admission_outcome(self.core.accept_replay(provider, scope))
    }

    pub async fn get_source_cursor(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> Result<Option<ObservationSourceCursorV1>, HostAdmissionOutcome> {
        tracedecay_sessions::admission::HostAdmission::get_source_cursor(&self.core, source, scope)
            .await
            .map_err(product_admission_outcome)
    }

    pub async fn capture_observation(
        &self,
        request: CaptureObservationRequest,
    ) -> Result<CaptureObservationOutcome, HostAdmissionOutcome> {
        tracedecay_sessions::admission::HostAdmission::capture_observation(&self.core, request)
            .await
            .map_err(product_admission_outcome)
    }

    pub async fn capture(&self, request: CaptureObservationRequest) -> HostAdmissionOutcome {
        match self.capture_observation(request).await {
            Ok(outcome) => classify_capture(outcome),
            Err(outcome) => outcome,
        }
    }

    pub async fn advance_non_durable_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> Result<CursorAdvanceOutcome, HostAdmissionOutcome> {
        tracedecay_sessions::admission::HostAdmission::advance_non_durable_source_cursor(
            &self.core,
            advance,
            cancellation,
        )
        .await
        .map_err(product_admission_outcome)
    }

    pub async fn drain_projection_queue(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
        cancellation: &ObservationCancellation,
        max: usize,
    ) -> Result<HostProjectionDrainOutcome, HostAdmissionOutcome> {
        tracedecay_sessions::admission::HostAdmission::drain_projection_queue(
            &self.core,
            provider,
            scope,
            cancellation,
            max,
        )
        .await
        .map(product_projection_drain_outcome)
        .map_err(product_admission_outcome)
    }
}

impl EvidenceAnchorResolver for HostAdmissionFacade<'_> {
    async fn resolve_evidence_anchor(
        &self,
        owner: FactOwnerV1,
        anchor_id: RetrievalAnchorId,
    ) -> Result<ResolvedEvidenceAnchorV1, EvidenceAnchorResolutionError> {
        owner
            .validate()
            .map_err(|error| EvidenceAnchorResolutionError::Authority {
                operation: "validate evidence anchor owner",
                source: Box::new(error),
            })?;
        anchor_id
            .validate()
            .map_err(|error| EvidenceAnchorResolutionError::Authority {
                operation: "validate evidence anchor identifier",
                source: Box::new(error),
            })?;
        let scope = ObservationScopeV1::from(owner.clone());
        self.authorities()
            .validate_scope(&scope)
            .map_err(|outcome| EvidenceAnchorResolutionError::Authority {
                operation: "validate evidence anchor authority scope",
                source: Box::new(std::io::Error::other(
                    outcome.reason_code.unwrap_or("authority_unavailable"),
                )),
            })?;
        let authority_scope = host_scope(&scope);
        let record = match self.authorities().registered_database(authority_scope) {
            Ok(Some(registered)) => registered
                .resolve_observation_evidence_anchor(&scope, &anchor_id)
                .await
                .map_err(|error| EvidenceAnchorResolutionError::Authority {
                    operation: "resolve registered observation evidence anchor",
                    source: Box::new(error),
                })?
                .ok_or_else(|| EvidenceAnchorResolutionError::Unavailable {
                    anchor_id: anchor_id.clone(),
                })?,
            Ok(None) => {
                return Err(EvidenceAnchorResolutionError::Authority {
                    operation: "resolve registered observation evidence anchor",
                    source: Box::new(std::io::Error::other("registered_authority_unavailable")),
                });
            }
            Err(outcome) => {
                return Err(EvidenceAnchorResolutionError::Authority {
                    operation: "resolve registered observation evidence anchor",
                    source: Box::new(std::io::Error::other(
                        outcome.reason_code.unwrap_or("authority_unavailable"),
                    )),
                });
            }
        };
        ResolvedEvidenceAnchorV1::new(record).map_err(|error| {
            EvidenceAnchorResolutionError::Authority {
                operation: "validate resolved observation evidence anchor",
                source: Box::new(error),
            }
        })
    }
}

/// Authority namespace stamped into caller-bound authorization snapshots for
/// record-less anchor resolutions (absent or ambiguous bindings).
const EVIDENCE_ANCHOR_RESOLUTION_NAMESPACE: &str = "observation-resolution.v1";

impl EvidenceAnchorReportResolver for HostAdmissionFacade<'_> {
    async fn resolve_evidence_anchor_report(
        &self,
        owner: FactOwnerV1,
        anchor_id: RetrievalAnchorId,
    ) -> Result<EvidenceAnchorResolutionReport, EvidenceAnchorResolutionError> {
        owner
            .validate()
            .map_err(|error| EvidenceAnchorResolutionError::Authority {
                operation: "validate evidence anchor owner",
                source: Box::new(error),
            })?;
        anchor_id
            .validate()
            .map_err(|error| EvidenceAnchorResolutionError::Authority {
                operation: "validate evidence anchor identifier",
                source: Box::new(error),
            })?;
        let scope = ObservationScopeV1::from(owner.clone());
        self.authorities()
            .validate_scope(&scope)
            .map_err(|outcome| EvidenceAnchorResolutionError::Authority {
                operation: "validate evidence anchor authority scope",
                source: Box::new(std::io::Error::other(
                    outcome.reason_code.unwrap_or("authority_unavailable"),
                )),
            })?;
        let authority_scope = host_scope(&scope);
        let observed = match self.authorities().registered_database(authority_scope) {
            Ok(Some(registered)) => registered
                .resolve_observation_evidence_anchor_report(&scope, &anchor_id)
                .await
                .map_err(|error| EvidenceAnchorResolutionError::Authority {
                    operation: "resolve registered observation evidence anchor report",
                    source: Box::new(error),
                })?,
            Ok(None) => {
                return Err(EvidenceAnchorResolutionError::Authority {
                    operation: "resolve registered observation evidence anchor report",
                    source: Box::new(std::io::Error::other("registered_authority_unavailable")),
                });
            }
            Err(outcome) => {
                return Err(EvidenceAnchorResolutionError::Authority {
                    operation: "resolve registered observation evidence anchor report",
                    source: Box::new(std::io::Error::other(
                        outcome.reason_code.unwrap_or("authority_unavailable"),
                    )),
                });
            }
        };
        let authorization = build_scope_resolution_authorization_v1(
            &scope,
            &anchor_id,
            EVIDENCE_ANCHOR_RESOLUTION_NAMESPACE,
        )
        .map_err(|error| EvidenceAnchorResolutionError::Authority {
            operation: "derive evidence anchor resolution authorization",
            source: Box::new(error),
        })?;
        EvidenceAnchorResolutionReport::from_observation(anchor_id, observed, authorization)
            .map_err(|error| EvidenceAnchorResolutionError::Authority {
                operation: "validate observation evidence anchor report",
                source: Box::new(error),
            })
    }
}

fn host_scope(scope: &ObservationScopeV1) -> HostAdmissionScope {
    match scope {
        ObservationScopeV1::Profile => HostAdmissionScope::Profile,
        ObservationScopeV1::Project { .. } => HostAdmissionScope::Project,
    }
}

fn classify_capture(outcome: CaptureObservationOutcome) -> HostAdmissionOutcome {
    match outcome {
        CaptureObservationOutcome::Persisted { outcome, .. } => match *outcome {
            ObservationPersistOutcome::Committed(_) => {
                HostAdmissionOutcome::new(HostAdmissionStatus::Committed, false, None)
            }
            ObservationPersistOutcome::ExactDuplicate(_) => {
                HostAdmissionOutcome::new(HostAdmissionStatus::ExactDuplicate, false, None)
            }
            ObservationPersistOutcome::CoveredDuplicate(_) => HostAdmissionOutcome::new(
                HostAdmissionStatus::Committed,
                false,
                Some("duplicate_coverage_committed"),
            ),
        },
        CaptureObservationOutcome::Rejected { .. } => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("sanitizer_rejected"),
        ),
        CaptureObservationOutcome::Quarantined { .. } => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("sanitizer_quarantined"),
        ),
    }
}

#[cfg(test)]
#[path = "host_admission_test.rs"]
mod host_admission_test;
