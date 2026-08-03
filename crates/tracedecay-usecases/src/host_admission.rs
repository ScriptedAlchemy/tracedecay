use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tracedecay_domain::{
    BrainId, FactOwnerV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceIdentityV1, ProjectId, RetrievalAnchorId, UserProfileId,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationProjectionStore, ObservationStore, ObservationStoreError,
    ParseOffset, ProjectionPersistOutcome, ProjectionStoreError, StoreShardScopeV1,
    build_scope_resolution_authorization_v1,
};

use crate::anchor_resolution::{EvidenceAnchorReportResolver, EvidenceAnchorResolutionReport};
use crate::memory::{
    EvidenceAnchorResolutionError, EvidenceAnchorResolver, ResolvedEvidenceAnchorV1,
};
use crate::observation::{
    AdvanceNonDurableSourceCursorRequest, CaptureObservationOutcome, CaptureObservationRequest,
    ObservationApplication, ObservationApplicationError, ObservationCancellation,
};
use crate::store::observation::GlobalDbObservationStore;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::privacy::RecordSanitizerV1;
use tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext;

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

    const fn project_authority_unbound() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("project_authority_unbound"),
        )
    }

    const fn project_authority_mismatch() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("project_authority_mismatch"),
        )
    }

    const fn registered_authority_unavailable() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("registered_authority_unavailable"),
        )
    }
}

pub use tracedecay_sessions::admission::HostAdmissionScope;

#[derive(Clone, Default)]
pub struct HostAdmissionAuthorities<'a> {
    project_id: Option<ProjectId>,
    project_registered: Option<&'a RegisteredGlobalDb>,
    brain_id: Option<BrainId>,
    profile_id: Option<UserProfileId>,
    profile_registered: Option<&'a RegisteredGlobalDb>,
    repository_provenance: Option<RepositoryProvenanceAdmissionContext>,
}

impl<'a> HostAdmissionAuthorities<'a> {
    pub fn registered_for_project(
        brain_id: BrainId,
        profile_id: UserProfileId,
        project_id: ProjectId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self {
            project_id: Some(project_id),
            project_registered: Some(registered),
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            profile_registered: None,
            repository_provenance: None,
        }
    }

    pub(crate) fn registered_for_profile(
        brain_id: BrainId,
        profile_id: UserProfileId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self {
            project_id: None,
            project_registered: None,
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            profile_registered: Some(registered),
            repository_provenance: None,
        }
    }

    pub fn for_project(
        brain_id: BrainId,
        profile_id: UserProfileId,
        project_id: ProjectId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self::registered_for_project(brain_id, profile_id, project_id, registered)
    }

    pub fn for_profile(
        brain_id: BrainId,
        profile_id: UserProfileId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self::registered_for_profile(brain_id, profile_id, registered)
    }

    /// Adds the registered profile-session authority to project admission.
    #[must_use]
    pub fn with_profile_registered(
        mut self,
        profile_id: UserProfileId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        self.profile_id = Some(profile_id);
        self.profile_registered = Some(registered);
        self
    }

    /// Admission bound to a project identity with **no** registered database
    /// and no resolved profile identity behind it.
    ///
    /// Standalone callers (a CLI invocation with no daemon-owned registry
    /// mount) still need an admission handle to walk a transcript and count
    /// what it *would* admit. Every capture fails closed; only scope
    /// validation against `project_id` is authoritative.
    pub fn unregistered_for_project(project_id: ProjectId) -> Self {
        Self {
            project_id: Some(project_id),
            project_registered: None,
            brain_id: None,
            profile_id: None,
            profile_registered: None,
            repository_provenance: None,
        }
    }

    /// Profile-scoped counterpart of [`Self::unregistered_for_project`].
    #[must_use]
    pub const fn unregistered_for_profile() -> Self {
        Self {
            project_id: None,
            project_registered: None,
            brain_id: None,
            profile_id: None,
            profile_registered: None,
            repository_provenance: None,
        }
    }

    pub fn unavailable_for_project(
        brain_id: BrainId,
        profile_id: UserProfileId,
        project_id: ProjectId,
    ) -> Self {
        Self {
            project_id: Some(project_id),
            project_registered: None,
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            profile_registered: None,
            repository_provenance: None,
        }
    }

    pub fn unavailable_for_profile(brain_id: BrainId, profile_id: UserProfileId) -> Self {
        Self {
            project_id: None,
            project_registered: None,
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            profile_registered: None,
            repository_provenance: None,
        }
    }

    #[must_use]
    pub fn with_repository_provenance(
        mut self,
        repository_provenance: RepositoryProvenanceAdmissionContext,
    ) -> Self {
        self.repository_provenance = Some(repository_provenance);
        self
    }

    fn registered_database(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<Option<&'a RegisteredGlobalDb>, HostAdmissionOutcome> {
        let database = match scope {
            HostAdmissionScope::Project => self.project_registered,
            HostAdmissionScope::Profile => self.profile_registered,
        };
        let Some(database) = database else {
            return Ok(None);
        };
        let shard = &database.binding().shard_id;
        let profile_matches = self.brain_id.as_ref() == Some(&shard.brain_id)
            && self.profile_id.as_ref() == Some(&shard.profile_id);
        let valid = profile_matches
            && match (scope, &shard.scope) {
                (
                    HostAdmissionScope::Project,
                    StoreShardScopeV1::ProjectSessions { project_id },
                ) => self.project_id.as_ref() == Some(project_id),
                (HostAdmissionScope::Profile, StoreShardScopeV1::ProfileSessions) => true,
                _ => false,
            };
        if valid {
            Ok(Some(database))
        } else {
            Err(HostAdmissionOutcome::project_authority_mismatch())
        }
    }

    fn validate_scope(&self, scope: &ObservationScopeV1) -> Result<(), HostAdmissionOutcome> {
        let ObservationScopeV1::Project { project_id } = scope else {
            return Ok(());
        };
        match self.project_id.as_ref() {
            Some(expected) if expected == project_id => Ok(()),
            Some(_) => Err(HostAdmissionOutcome::project_authority_mismatch()),
            None => Err(HostAdmissionOutcome::project_authority_unbound()),
        }
    }
}

pub struct HostAdmissionFacade<'a> {
    authorities: HostAdmissionAuthorities<'a>,
}

impl tracedecay_sessions::admission::HostAdmission for HostAdmissionFacade<'_> {
    fn capture_observation<'a>(
        &'a self,
        request: CaptureObservationRequest,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, CaptureObservationOutcome> {
        Box::pin(async move {
            HostAdmissionFacade::capture_observation(self, request)
                .await
                .map_err(canonical_admission_outcome)
        })
    }

    fn advance_non_durable_source_cursor<'a>(
        &'a self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, CursorAdvanceOutcome> {
        Box::pin(async move {
            HostAdmissionFacade::advance_non_durable_source_cursor(self, advance, cancellation)
                .await
                .map_err(canonical_admission_outcome)
        })
    }

    fn get_source_cursor<'a>(
        &'a self,
        source: &'a ObservationSourceIdentityV1,
        scope: &'a ObservationScopeV1,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Option<ObservationSourceCursorV1>>
    {
        Box::pin(async move {
            HostAdmissionFacade::get_source_cursor(self, source, scope)
                .await
                .map_err(canonical_admission_outcome)
        })
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
        Box::pin(async move {
            HostAdmissionFacade::drain_projection_queue(self, provider, scope, cancellation, max)
                .await
                .map(canonical_projection_drain_outcome)
                .map_err(canonical_admission_outcome)
        })
    }

    fn has_session_message<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        message_id: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, bool> {
        Box::pin(async move {
            HostAdmissionFacade::has_session_message(self, scope, provider, message_id)
                .await
                .map_err(canonical_admission_outcome)
        })
    }

    fn get_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Option<ParseOffset>> {
        Box::pin(async move {
            HostAdmissionFacade::get_parse_offset(self, scope, path)
                .await
                .map_err(canonical_admission_outcome)
        })
    }

    fn advance_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
        offset: ParseOffset,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, ()> {
        Box::pin(async move {
            HostAdmissionFacade::advance_parse_offset(self, scope, path, offset)
                .await
                .map_err(canonical_admission_outcome)
        })
    }
}

const fn canonical_admission_status(
    status: HostAdmissionStatus,
) -> tracedecay_sessions::admission::HostAdmissionStatus {
    match status {
        HostAdmissionStatus::Supported => {
            tracedecay_sessions::admission::HostAdmissionStatus::Supported
        }
        HostAdmissionStatus::Degraded => {
            tracedecay_sessions::admission::HostAdmissionStatus::Degraded
        }
        HostAdmissionStatus::Unavailable => {
            tracedecay_sessions::admission::HostAdmissionStatus::Unavailable
        }
        HostAdmissionStatus::Unknown => {
            tracedecay_sessions::admission::HostAdmissionStatus::Unknown
        }
        HostAdmissionStatus::Backpressured => {
            tracedecay_sessions::admission::HostAdmissionStatus::Backpressured
        }
        HostAdmissionStatus::AcceptedForReplay => {
            tracedecay_sessions::admission::HostAdmissionStatus::AcceptedForReplay
        }
        HostAdmissionStatus::Committed => {
            tracedecay_sessions::admission::HostAdmissionStatus::Committed
        }
        HostAdmissionStatus::ExactDuplicate => {
            tracedecay_sessions::admission::HostAdmissionStatus::ExactDuplicate
        }
    }
}

const fn canonical_admission_outcome(
    outcome: HostAdmissionOutcome,
) -> tracedecay_sessions::admission::HostAdmissionOutcome {
    tracedecay_sessions::admission::HostAdmissionOutcome {
        status: canonical_admission_status(outcome.status),
        retryable: outcome.retryable,
        reason_code: outcome.reason_code,
    }
}

fn canonical_projection_drain_outcome(
    outcome: HostProjectionDrainOutcome,
) -> tracedecay_sessions::admission::HostProjectionDrainOutcome {
    tracedecay_sessions::admission::HostProjectionDrainOutcome {
        projected: outcome.projected,
        projected_outputs: outcome.projected_outputs,
        skipped: outcome.skipped,
        exact_duplicates: outcome.exact_duplicates,
        session_ids: outcome.session_ids,
    }
}

impl<'a> HostAdmissionFacade<'a> {
    pub const fn new(authorities: HostAdmissionAuthorities<'a>) -> Self {
        Self { authorities }
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
        self.authorities
            .validate_scope(&scope)
            .map_err(|_| unavailable())?;
        let database = self
            .authorities
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
        if !supported_provider(provider) {
            return HostAdmissionOutcome::new(
                HostAdmissionStatus::Unknown,
                false,
                Some("unknown_provider"),
            );
        }
        if scope == HostAdmissionScope::Project && self.authorities.project_id.is_none() {
            return HostAdmissionOutcome::project_authority_unbound();
        }
        match self.authorities.registered_database(scope) {
            Ok(Some(_)) => HostAdmissionOutcome::supported(),
            Ok(None) => HostAdmissionOutcome::registered_authority_unavailable(),
            Err(outcome) => outcome,
        }
    }

    pub fn accept_replay(&self, provider: &str, scope: HostAdmissionScope) -> HostAdmissionOutcome {
        let probe = self.probe(provider, scope);
        if probe.status == HostAdmissionStatus::Supported {
            HostAdmissionOutcome::accepted_for_replay()
        } else {
            probe
        }
    }

    pub async fn get_source_cursor(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> Result<Option<ObservationSourceCursorV1>, HostAdmissionOutcome> {
        let store = self.store(source.provider().as_str(), scope)?;
        store
            .get_source_cursor(source, scope)
            .await
            .map_err(|error| classify_error(&ObservationApplicationError::Store(error)))
    }

    pub(crate) async fn get_parse_offset(
        &self,
        scope: &ObservationScopeV1,
        path: &str,
    ) -> Result<Option<tracedecay_global_db::ParseOffset>, HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        let database = self
            .authorities
            .registered_database(host_scope(scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        database
            .get_parse_offset_result(path)
            .await
            .map_err(|error| {
                tracing::warn!(?error, "registered host parse-offset read failed");
                HostAdmissionOutcome::registered_authority_unavailable()
            })
    }

    pub(crate) async fn advance_parse_offset(
        &self,
        scope: &ObservationScopeV1,
        path: &str,
        offset: tracedecay_global_db::ParseOffset,
    ) -> Result<(), HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        let database = self
            .authorities
            .registered_database(host_scope(scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        database
            .advance_parse_offset_result(path, offset)
            .await
            .map_err(|error| {
                tracing::warn!(?error, "registered host parse-offset advance failed");
                HostAdmissionOutcome::registered_authority_unavailable()
            })
    }

    pub(crate) async fn has_session_message(
        &self,
        scope: &ObservationScopeV1,
        provider: &str,
        message_id: &str,
    ) -> Result<bool, HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        let database = self
            .authorities
            .registered_database(host_scope(scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        database
            .has_session_message(provider, message_id)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "registered host session-message lookup failed");
                HostAdmissionOutcome::registered_authority_unavailable()
            })
    }

    pub async fn capture_observation(
        &self,
        request: CaptureObservationRequest,
    ) -> Result<CaptureObservationOutcome, HostAdmissionOutcome> {
        let provider = request.provider().to_owned();
        let scope = request.scope().clone();
        self.authorities.validate_scope(&scope)?;
        let database = self
            .authorities
            .registered_database(host_scope(&scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        let application = self.application(&provider, &scope)?;
        let outcome = application
            .capture_observation(
                request.with_repository_provenance(self.authorities.repository_provenance.clone()),
            )
            .await
            .map_err(|error| classify_error(&error))?;
        if let CaptureObservationOutcome::Persisted { outcome, .. } = &outcome {
            crate::external_source_store::RuntimeExternalSourceStore::new(
                database.runtime().clone(),
                database.authority().clone(),
            )
            .map_err(|error| {
                tracing::warn!(%error, "registered external-source adapter is unavailable");
                HostAdmissionOutcome::registered_authority_unavailable()
            })?
            .capture_host_observation(outcome.receipt())
            .await
            .map_err(|error| {
                tracing::warn!(%error, "registered external-source commit failed");
                HostAdmissionOutcome::retained_unavailable("external_source_commit_failed")
            })?;
        }
        Ok(outcome)
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
        let cursor = advance.next_cursor();
        let application = self.application(cursor.source().provider().as_str(), cursor.scope())?;
        application
            .advance_non_durable_source_cursor(AdvanceNonDurableSourceCursorRequest::new(
                advance,
                cancellation,
            ))
            .await
            .map_err(|error| classify_error(&error))
    }

    pub async fn drain_projection_queue(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
        cancellation: &ObservationCancellation,
        max: usize,
    ) -> Result<HostProjectionDrainOutcome, HostAdmissionOutcome> {
        let store = self.store(provider, scope)?;
        let mut outcome = HostProjectionDrainOutcome::default();
        let mut session_ids = BTreeSet::new();
        for _ in 0..max {
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            let Some(observation_id) = store.next_queued_observation().await.map_err(|error| {
                tracing::warn!(%error, "projection store operation failed during host drain");
                projection_store_unavailable()
            })?
            else {
                break;
            };
            let projected = match store.project_observation(&observation_id).await {
                Ok(projected) => projected,
                Err(ProjectionStoreError::RetryDeferred { .. }) => break,
                Err(error) if error.deterministic_rejection_reason().is_some() => {
                    tracing::warn!(
                        %error,
                        observation = observation_id.as_str(),
                        "deterministic projection rejection committed"
                    );
                    outcome.skipped = outcome.skipped.saturating_add(1);
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%error, "projection store operation failed during host drain");
                    return Err(projection_error_outcome(&error));
                }
            };
            match projected {
                ProjectionPersistOutcome::Projected(projected) => {
                    outcome.projected = outcome.projected.saturating_add(1);
                    outcome.projected_outputs = outcome.projected_outputs.saturating_add(
                        u64::try_from(projected.output_count()).unwrap_or(u64::MAX),
                    );
                    if let Some(observation) = store
                        .get_observation(&observation_id)
                        .await
                        .map_err(|error| {
                    tracing::warn!(%error, "projection store operation failed during host drain");
                    projection_store_unavailable()
                })?
                    {
                        session_ids.insert(
                            observation
                                .observation()
                                .source()
                                .session_id()
                                .as_str()
                                .to_owned(),
                        );
                    }
                }
                ProjectionPersistOutcome::Skipped { .. } => {
                    outcome.skipped = outcome.skipped.saturating_add(1);
                }
                ProjectionPersistOutcome::ExactDuplicate(_) => {
                    outcome.exact_duplicates = outcome.exact_duplicates.saturating_add(1);
                }
            }
        }
        outcome.session_ids = session_ids.into_iter().collect();
        Ok(outcome)
    }

    fn application(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
    ) -> Result<ObservationApplication<GlobalDbObservationStore<'a>>, HostAdmissionOutcome> {
        let store = self.store(provider, scope)?;
        let sanitizer = RecordSanitizerV1::observation_v1().map_err(|_| {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                false,
                Some("sanitizer_unavailable"),
            )
        })?;
        Ok(ObservationApplication::new(store, sanitizer))
    }

    fn store(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
    ) -> Result<GlobalDbObservationStore<'a>, HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        let scope = host_scope(scope);
        let probe = self.probe(provider, scope);
        if probe.status != HostAdmissionStatus::Supported {
            return Err(probe);
        }
        match self.authorities.registered_database(scope)? {
            Some(database) => Ok(GlobalDbObservationStore::with_runtime(
                database.runtime(),
                database.authority(),
            )),
            None => Err(HostAdmissionOutcome::registered_authority_unavailable()),
        }
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
        self.authorities.validate_scope(&scope).map_err(|outcome| {
            EvidenceAnchorResolutionError::Authority {
                operation: "validate evidence anchor authority scope",
                source: Box::new(std::io::Error::other(
                    outcome.reason_code.unwrap_or("authority_unavailable"),
                )),
            }
        })?;
        let authority_scope = host_scope(&scope);
        let record = match self.authorities.registered_database(authority_scope) {
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
        self.authorities.validate_scope(&scope).map_err(|outcome| {
            EvidenceAnchorResolutionError::Authority {
                operation: "validate evidence anchor authority scope",
                source: Box::new(std::io::Error::other(
                    outcome.reason_code.unwrap_or("authority_unavailable"),
                )),
            }
        })?;
        let authority_scope = host_scope(&scope);
        let observed = match self.authorities.registered_database(authority_scope) {
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

const fn projection_store_unavailable() -> HostAdmissionOutcome {
    HostAdmissionOutcome::new(
        HostAdmissionStatus::Unavailable,
        true,
        Some("projection_store_unavailable"),
    )
}

const fn projection_error_outcome(error: &ProjectionStoreError) -> HostAdmissionOutcome {
    match error {
        ProjectionStoreError::Storage { .. } => {
            HostAdmissionOutcome::retained_unavailable("projection_storage_retry_scheduled")
        }
        ProjectionStoreError::RetryDeferred { .. } => {
            HostAdmissionOutcome::retained_backpressured("projection_retry_deferred")
        }
        ProjectionStoreError::Gap { .. }
        | ProjectionStoreError::NotQueued
        | ProjectionStoreError::ObservationNotFound
        | ProjectionStoreError::InvalidRebuildFrontier { .. } => {
            HostAdmissionOutcome::degraded("projection_state_invalid")
        }
        ProjectionStoreError::SequenceOverflow(_)
        | ProjectionStoreError::UnsupportedProvider(_)
        | ProjectionStoreError::OutputCollision { .. }
        | ProjectionStoreError::ProvenanceCollision
        | ProjectionStoreError::Contract(_)
        | ProjectionStoreError::Anchor(_) => HostAdmissionOutcome::degraded("projection_rejected"),
    }
}

fn host_scope(scope: &ObservationScopeV1) -> HostAdmissionScope {
    match scope {
        ObservationScopeV1::Profile => HostAdmissionScope::Profile,
        ObservationScopeV1::Project { .. } => HostAdmissionScope::Project,
    }
}

fn supported_provider(provider: &str) -> bool {
    matches!(provider, "kimi" | "opencode")
        || tracedecay_sessions::runtime::SessionProvider::parse(provider)
            .is_some_and(tracedecay_sessions::runtime::SessionProvider::supports_host_admission)
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

fn classify_error(error: &ObservationApplicationError) -> HostAdmissionOutcome {
    match error {
        ObservationApplicationError::Cancelled => HostAdmissionOutcome::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("admission_cancelled"),
        ),
        ObservationApplicationError::Store(ObservationStoreError::CursorConflict { .. }) => {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Backpressured,
                true,
                Some("cursor_conflict"),
            )
        }
        ObservationApplicationError::Store(ObservationStoreError::Storage { .. }) => {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("authority_write_failed"),
            )
        }
        ObservationApplicationError::Contract(_) => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("invalid_observation_contract"),
        ),
        ObservationApplicationError::Privacy(_) => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("privacy_boundary_failed"),
        ),
        ObservationApplicationError::Store(_)
        | ObservationApplicationError::PersistedObservationUnavailable => {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Degraded,
                false,
                Some("observation_commit_failed"),
            )
        }
    }
}

#[cfg(test)]
#[path = "host_admission_test.rs"]
mod host_admission_test;
