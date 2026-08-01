use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(any(test, feature = "test-transport"))]
use rusqlite::{Connection as RusqliteConnection, OpenFlags, types::ValueRef};
use serde::Serialize;
#[cfg(any(test, feature = "test-transport"))]
use sha2::{Digest as _, Sha256};
use tracedecay_domain::{
    BrainId, FactOwnerV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceIdentityV1, ProjectId, RetrievalAnchorId, UserProfileId,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationProjectionStore, ObservationReplayRequest,
    ObservationStore, ObservationStoreError, ObservationStoreResult, ParseOffset,
    ProjectionPersistOutcome, StoreShardScopeV1, StoredObservation,
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

pub(crate) use disposition::is_bounded_reason_code;
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

    #[cfg(any(test, feature = "test-transport"))]
    pub async fn pending_count(&self) -> usize {
        self.pending_replay_count().await.unwrap_or_default()
    }

    #[cfg(any(test, feature = "test-transport"))]
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
#[cfg(any(test, feature = "test-transport"))]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAdmissionScope {
    Project,
    Profile,
}

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

    pub(crate) fn unregistered_for_project(project_id: ProjectId) -> Self {
        Self {
            project_id: Some(project_id),
            project_registered: None,
            brain_id: None,
            profile_id: None,
            profile_registered: None,
            repository_provenance: None,
        }
    }

    pub(crate) const fn unregistered_for_profile() -> Self {
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
    #[allow(dead_code)] // staged admission builder — preserve authority surface
    pub(crate) fn with_profile_identity(
        mut self,
        brain_id: BrainId,
        profile_id: UserProfileId,
    ) -> Self {
        self.brain_id = Some(brain_id);
        self.profile_id = Some(profile_id);
        self
    }

    #[must_use]
    #[allow(dead_code)] // staged admission builder — preserve authority surface
    pub(crate) const fn with_project_registered(mut self, runtime: &'a RegisteredGlobalDb) -> Self {
        self.project_registered = Some(runtime);
        self
    }

    #[must_use]
    pub(crate) fn with_profile_registered(
        mut self,
        profile_id: UserProfileId,
        runtime: &'a RegisteredGlobalDb,
    ) -> Self {
        self.profile_id = Some(profile_id);
        self.profile_registered = Some(runtime);
        self
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

pub(crate) trait ObservationCaptureAdmissionPort {
    fn capture_observation(
        &self,
        request: CaptureObservationRequest,
    ) -> impl Future<Output = Result<CaptureObservationOutcome, HostAdmissionOutcome>> + Send;

    fn advance_non_durable_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> impl Future<Output = Result<CursorAdvanceOutcome, HostAdmissionOutcome>> + Send;

    fn get_source_cursor<'a>(
        &'a self,
        source: &'a ObservationSourceIdentityV1,
        scope: &'a ObservationScopeV1,
    ) -> impl Future<Output = Result<Option<ObservationSourceCursorV1>, HostAdmissionOutcome>> + Send + 'a;

    fn drain_projection_queue<'a>(
        &'a self,
        provider: &'a str,
        scope: &'a ObservationScopeV1,
        cancellation: &'a ObservationCancellation,
        max: usize,
    ) -> impl Future<Output = Result<HostProjectionDrainOutcome, HostAdmissionOutcome>> + Send + 'a;
}

pub(crate) trait TranscriptCursorAdmissionPort {
    fn get_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
    ) -> impl Future<Output = Result<Option<ParseOffset>, HostAdmissionOutcome>> + Send + 'a;

    fn advance_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
        offset: ParseOffset,
    ) -> impl Future<Output = Result<(), HostAdmissionOutcome>> + Send + 'a;
}

impl ObservationCaptureAdmissionPort for HostAdmissionFacade<'_> {
    fn capture_observation(
        &self,
        request: CaptureObservationRequest,
    ) -> impl Future<Output = Result<CaptureObservationOutcome, HostAdmissionOutcome>> + Send {
        HostAdmissionFacade::capture_observation(self, request)
    }

    fn advance_non_durable_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> impl Future<Output = Result<CursorAdvanceOutcome, HostAdmissionOutcome>> + Send {
        HostAdmissionFacade::advance_non_durable_source_cursor(self, advance, cancellation)
    }

    fn get_source_cursor<'a>(
        &'a self,
        source: &'a ObservationSourceIdentityV1,
        scope: &'a ObservationScopeV1,
    ) -> impl Future<Output = Result<Option<ObservationSourceCursorV1>, HostAdmissionOutcome>> + Send + 'a
    {
        HostAdmissionFacade::get_source_cursor(self, source, scope)
    }

    fn drain_projection_queue<'a>(
        &'a self,
        provider: &'a str,
        scope: &'a ObservationScopeV1,
        cancellation: &'a ObservationCancellation,
        max: usize,
    ) -> impl Future<Output = Result<HostProjectionDrainOutcome, HostAdmissionOutcome>> + Send + 'a
    {
        HostAdmissionFacade::drain_projection_queue(self, provider, scope, cancellation, max)
    }
}

impl TranscriptCursorAdmissionPort for HostAdmissionFacade<'_> {
    fn get_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
    ) -> impl Future<Output = Result<Option<ParseOffset>, HostAdmissionOutcome>> + Send + 'a {
        HostAdmissionFacade::get_parse_offset(self, scope, path)
    }

    fn advance_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
        offset: ParseOffset,
    ) -> impl Future<Output = Result<(), HostAdmissionOutcome>> + Send + 'a {
        HostAdmissionFacade::advance_parse_offset(self, scope, path, offset)
    }
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
            match store
                .project_observation(&observation_id)
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "projection store operation failed during host drain");
                    projection_store_unavailable()
                })? {
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

    fn observation_store(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<GlobalDbObservationStore<'a>, HostAdmissionOutcome> {
        match self.authorities.registered_database(scope)? {
            Some(database) => Ok(GlobalDbObservationStore::with_runtime(
                database.runtime(),
                database.authority(),
            )),
            None => Err(HostAdmissionOutcome::registered_authority_unavailable()),
        }
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

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTemporalFixtureCountV1 {
    ProjectionReceipts,
    Occurrences,
    LogicalCopyEdges,
    Assertions,
    RefreshReceipts,
    RefreshProgress,
    RefreshOperations,
    RefreshBindings,
    RefreshBatchBindings,
    TemporalGenerations,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LcmLineageFaultForTest {
    CorruptCompatibilitySummaryText {
        node_id: String,
        text: String,
    },
    ShiftRawMessageTimestamp {
        store_id: i64,
        delta: i64,
    },
    DeleteGeneration {
        session_id: String,
        generation: i64,
    },
    ReplaceGenerationWatermarks {
        session_id: String,
        generation: i64,
        json: String,
    },
    DeleteAvailability {
        session_id: String,
        generation: i64,
        summary_id: String,
    },
    ReplaceAvailabilityHorizon {
        session_id: String,
        generation: i64,
        summary_id: String,
        source_horizon_json: String,
    },
    SetAvailability {
        session_id: String,
        generation: i64,
        summary_id: String,
        availability: String,
        reason: Option<String>,
    },
    SetGenerationFailed {
        session_id: String,
        generation: i64,
    },
    CorruptRetrievalAnchorOwner {
        summary_id: String,
    },
    ReplaceSummarySourceWithSummary {
        summary_id: String,
        ordinal: i64,
        source_summary_id: String,
    },
}

#[doc(hidden)]
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LcmLineageCountsForTest {
    pub active_generations: i64,
    pub total_generations: i64,
    pub summary_nodes: i64,
    pub summary_sources: i64,
    pub summary_successors: i64,
    pub cursor_keys: i64,
}

#[doc(hidden)]
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LcmExternalPayloadManifestTestRecord {
    pub payload_ref: String,
    pub session_id: String,
    pub payload_digest: String,
    pub manifest_json: String,
    pub receipt_id: String,
    pub created_at: i64,
    pub external_created_at: i64,
}

/// Opaque registered-runtime support for external integration tests.
///
/// This assembles the same durable profile identity, canonical session
/// registry, and actor-time database authority used by the daemon. It exposes
/// neither raw admission authorities nor registered database handles.
#[doc(hidden)]
#[cfg(test)]
#[allow(dead_code)] // integration/unit-test fixture; methods reached outside lib-only builds
pub struct HostAdmissionTestRuntimeV1 {
    brain_id: BrainId,
    profile_id: UserProfileId,
    profile_root: PathBuf,
    project_id: Option<ProjectId>,
    profile_database: Arc<RegisteredGlobalDb>,
    profile_registered: Arc<RegisteredGlobalDb>,
    project_registered: Option<Arc<RegisteredGlobalDb>>,
    _session_registry:
        Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>,
    _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
}

#[doc(hidden)]
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostAdmissionDatabaseIdentityV1([u8; 32]);

#[cfg(any(test, feature = "test-transport"))]
fn canonical_session_domain_sha256(
    path: &Path,
) -> tracedecay_runtime_core::errors::Result<[u8; 32]> {
    let connection = RusqliteConnection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| session_domain_digest_error("open session database", error))?;
    let mut table_statement = connection
        .prepare(
            "SELECT name
             FROM sqlite_schema
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
               AND name <> 'analytics_events'
             ORDER BY name",
        )
        .map_err(|error| session_domain_digest_error("prepare session table inventory", error))?;
    let tables = table_statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| session_domain_digest_error("query session table inventory", error))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| session_domain_digest_error("read session table inventory", error))?;
    drop(table_statement);

    let mut digest = Sha256::new();
    digest.update(b"tracedecay.session-domain-state.v1\0");
    for table in tables {
        digest_len_prefixed(&mut digest, table.as_bytes());
        let escaped = table.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{escaped}\""))
            .map_err(|error| session_domain_digest_error("prepare session table read", error))?;
        let column_count = statement.column_count();
        let order = (1..=column_count)
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT * FROM \"{escaped}\" ORDER BY {order}");
        drop(statement);
        statement = connection
            .prepare(&sql)
            .map_err(|error| session_domain_digest_error("prepare ordered session read", error))?;
        let mut rows = statement
            .query([])
            .map_err(|error| session_domain_digest_error("query session table", error))?;
        while let Some(row) = rows
            .next()
            .map_err(|error| session_domain_digest_error("read session table row", error))?
        {
            digest.update(b"row\0");
            for index in 0..column_count {
                match row.get_ref(index).map_err(|error| {
                    session_domain_digest_error("decode session table value", error)
                })? {
                    ValueRef::Null => digest.update([0]),
                    ValueRef::Integer(value) => {
                        digest.update([1]);
                        digest.update(value.to_le_bytes());
                    }
                    ValueRef::Real(value) => {
                        digest.update([2]);
                        digest.update(value.to_bits().to_le_bytes());
                    }
                    ValueRef::Text(value) => {
                        digest.update([3]);
                        digest_len_prefixed(&mut digest, value);
                    }
                    ValueRef::Blob(value) => {
                        digest.update([4]);
                        digest_len_prefixed(&mut digest, value);
                    }
                }
            }
        }
    }
    Ok(digest.finalize().into())
}

#[cfg(any(test, feature = "test-transport"))]
fn digest_len_prefixed(digest: &mut Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(value);
}

#[cfg(any(test, feature = "test-transport"))]
fn session_domain_digest_error(
    operation: &str,
    error: rusqlite::Error,
) -> tracedecay_runtime_core::errors::TraceDecayError {
    tracedecay_runtime_core::errors::TraceDecayError::Database {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}

/// A [`HostAdmissionTestRuntimeV1`] statically known to carry project scope.
///
/// Seams that mount a project graph or a project-session authority need both
/// `project_id` and a registered `ProjectSessions` mount, which only
/// [`HostAdmissionTestRuntimeV1::project`] establishes. Taking this type
/// instead of the bare runtime makes a profile-scoped runtime unrepresentable
/// at those seams, so the mismatch is a compile error at the call site rather
/// than a panic deep inside construction.
#[cfg(test)]
#[doc(hidden)]
#[derive(Clone)]
pub struct ProjectScopedTestRuntimeV1(Arc<HostAdmissionTestRuntimeV1>);

#[cfg(test)]
impl ProjectScopedTestRuntimeV1 {
    /// Checked promotion for runtimes whose scope is not known statically,
    /// such as one recovered from an already-open graph.
    #[doc(hidden)]
    pub fn new(
        runtime: impl Into<Arc<HostAdmissionTestRuntimeV1>>,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        let runtime = runtime.into();
        if runtime.project_id.is_none() || runtime.project_registered.is_none() {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: format!(
                    "test runtime for profile '{}' is profile-scoped; project-scoped authority \
                     requires HostAdmissionTestRuntimeV1::project",
                    runtime.profile_root.display()
                ),
            });
        }
        Ok(Self(runtime))
    }

    #[doc(hidden)]
    pub fn into_runtime(self) -> Arc<HostAdmissionTestRuntimeV1> {
        self.0
    }
}

#[cfg(test)]
impl std::ops::Deref for ProjectScopedTestRuntimeV1 {
    type Target = HostAdmissionTestRuntimeV1;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[allow(dead_code)] // integration/unit-test fixture; methods reached outside lib-only builds
#[cfg(test)]
impl HostAdmissionTestRuntimeV1 {
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub fn profile_root_for_test(&self) -> &Path {
        &self.profile_root
    }

    #[doc(hidden)]
    pub async fn call_user_lcm_tool_for_test(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
        profile_root: &Path,
    ) -> tracedecay_runtime_core::errors::Result<crate::mcp::tools::ToolResult> {
        crate::mcp::tools::handle_user_lcm_tool_with_retained_authority(
            tool_name,
            arguments,
            profile_root,
            &self.profile_registered,
            None,
        )
        .await
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn call_mcp_tool_for_test(
        &self,
        cg: &crate::tracedecay::TraceDecay,
        tool_name: &str,
        arguments: serde_json::Value,
        server_stats: Option<serde_json::Value>,
        scope_prefix: Option<&str>,
    ) -> tracedecay_runtime_core::errors::Result<crate::mcp::ToolResult> {
        let project_registry_reads = crate::mcp::server::DaemonProjectRegistryReadService::new(
            Arc::clone(&self.profile_database),
        );
        let workflow_index_reads = self.project_registered.as_ref().map(|database| {
            crate::mcp::server::DaemonWorkflowIndexReadService::new(Arc::clone(database))
        });
        crate::mcp::tools::handle_tool_call_with_registry_and_implicit_project(
            cg,
            tool_name,
            arguments,
            server_stats,
            scope_prefix,
            crate::mcp::tools::ToolCallRegistryOptions {
                global_db: Some(self.profile_database.as_ref()),
                accounting_db: Some(self.profile_database.as_ref()),
                project_registry_reads: Some(&project_registry_reads),
                workflow_index_reads: workflow_index_reads
                    .as_ref()
                    .map(|service| service as &dyn tracedecay_sessions::WorkflowIndexReadPort),
                profile_root: Some(&self.profile_root),
                implicit_project_path: Some(cg.project_root()),
                session_authorities: self.mcp_session_authorities(),
                ..Default::default()
            },
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn retire_applied_input_manifests_for_test(
        &self,
        profile_root: &Path,
    ) -> tracedecay_migrate::consolidate::ManifestRetirementReport {
        tracedecay_migrate::consolidate::retire_applied_input_manifests(
            profile_root,
            self.profile_database.as_ref(),
        )
        .await
    }

    #[doc(hidden)]
    pub fn canonical_project_key(project_path: &Path) -> String {
        RegisteredGlobalDb::canonical_project_key(project_path)
    }

    pub async fn profile(
        profile_root: impl AsRef<Path>,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        Self::open(profile_root.as_ref().to_path_buf(), None).await
    }

    pub async fn project(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        Self::open(
            profile_root.as_ref().to_path_buf(),
            Some((project_root.as_ref().to_path_buf(), project_id)),
        )
        .await
    }

    /// [`Self::project`] returning the scope proof that project-graph and
    /// project-session seams require.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn project_scoped(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> tracedecay_runtime_core::errors::Result<ProjectScopedTestRuntimeV1> {
        ProjectScopedTestRuntimeV1::new(
            Self::project(profile_root, project_root, project_id).await?,
        )
    }

    async fn open(
        profile_root: PathBuf,
        project: Option<(PathBuf, ProjectId)>,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        prepare_host_admission_test_profile_root(&profile_root)?;
        if let Some((project_root, project_id)) = project.as_ref() {
            prepare_host_admission_test_project_root(project_root, project_id)?;
        }
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)?;
        let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            identity.profile_root(),
            1,
            "host-admission-test-runtime",
        )?;
        let session_registry = Arc::new(
            crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                identity.clone(),
            )
            .await?,
        );
        let profile_database = session_registry.profile_database().await?;
        let profile_registered = session_registry.profile_sessions().await?;

        let (project_id, project_registered) = if let Some((project_root, project_id)) = project {
            let registered = session_registry
                .project_sessions(project_id.clone(), [project_root])
                .await?;
            (Some(project_id), Some(registered))
        } else {
            (None, None)
        };

        Ok(Self {
            brain_id: identity.brain_id().clone(),
            profile_id: identity.profile_id().clone(),
            profile_root,
            project_id,
            profile_database,
            profile_registered,
            project_registered,
            _session_registry: session_registry,
            _database_scope: database_scope,
        })
    }

    /// Initializes a project graph through this retained registered runtime.
    ///
    /// Integration tests use this instead of the intentionally unavailable
    /// standalone `TraceDecay::init` path, preserving the same project-session
    /// authority required by daemon-owned production opens.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn initialize_project_graph_for_test(
        &self,
        project_root: &Path,
        open_options: crate::tracedecay::TraceDecayOpenOptions,
    ) -> tracedecay_runtime_core::errors::Result<crate::tracedecay::TraceDecay> {
        let project_id = self.project_id.as_ref().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: "project graph initialization requires project-scoped test authority"
                    .to_owned(),
            }
        })?;
        let project_database = self.project_registered.clone().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: "project graph initialization requires a registered project session"
                    .to_owned(),
            }
        })?;
        let store_layout = crate::tracedecay::TraceDecay::resolve_registered_configuration_layout(
            project_root,
            &open_options,
            self.profile_database.as_ref(),
            true,
        )
        .await?;
        if store_layout.identity.project_id.as_deref() != Some(project_id.as_str()) {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: "project graph identity differs from registered test authority".to_owned(),
            });
        }
        crate::tracedecay::TraceDecay::init_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            project_database,
            Arc::clone(&self.profile_database),
            Arc::clone(&self._session_registry),
        )
        .await
    }

    /// Reopens an existing project graph through this retained registered runtime.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn open_project_graph_for_test(
        &self,
        project_root: &Path,
        open_options: crate::tracedecay::TraceDecayOpenOptions,
    ) -> tracedecay_runtime_core::errors::Result<crate::tracedecay::TraceDecay> {
        let (store_layout, project_database) = self
            .registered_project_open_inputs(project_root, &open_options)
            .await?;
        crate::tracedecay::TraceDecay::open_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            project_database,
            Arc::clone(&self.profile_database),
            Arc::clone(&self._session_registry),
        )
        .await
    }

    #[cfg(any(test, feature = "test-transport"))]
    async fn registered_project_open_inputs(
        &self,
        project_root: &Path,
        open_options: &crate::tracedecay::TraceDecayOpenOptions,
    ) -> tracedecay_runtime_core::errors::Result<(
        tracedecay_runtime_core::storage::StoreLayout,
        Arc<RegisteredGlobalDb>,
    )> {
        let project_id = self.project_id.as_ref().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: "project graph open requires project-scoped test authority".to_owned(),
            }
        })?;
        let project_database = self.project_registered.clone().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: "project graph open requires a registered project session".to_owned(),
            }
        })?;
        let store_layout = crate::tracedecay::TraceDecay::resolve_registered_configuration_layout(
            project_root,
            open_options,
            self.profile_database.as_ref(),
            true,
        )
        .await?;
        if store_layout.identity.project_id.as_deref() != Some(project_id.as_str()) {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: "project graph identity differs from registered test authority".to_owned(),
            });
        }
        Ok((store_layout, project_database))
    }

    /// Opens one tracked branch through this retained registered runtime.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn open_project_branch_for_test(
        &self,
        project_root: &Path,
        branch_name: &str,
        open_options: crate::tracedecay::TraceDecayOpenOptions,
    ) -> tracedecay_runtime_core::errors::Result<crate::tracedecay::TraceDecay> {
        let project_id = self.project_id.as_ref().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: "project branch open requires project-scoped test authority".to_owned(),
            }
        })?;
        let project_database = self.project_registered.clone().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: "project branch open requires a registered project session".to_owned(),
            }
        })?;
        let store_layout = crate::tracedecay::TraceDecay::resolve_registered_configuration_layout(
            project_root,
            &open_options,
            self.profile_database.as_ref(),
            true,
        )
        .await?;
        if store_layout.identity.project_id.as_deref() != Some(project_id.as_str()) {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: "project branch identity differs from registered test authority"
                    .to_owned(),
            });
        }
        crate::tracedecay::TraceDecay::open_branch_with_registered_configuration(
            project_root,
            branch_name,
            open_options,
            store_layout,
            project_database,
            Arc::clone(&self.profile_database),
            Arc::clone(&self._session_registry),
        )
        .await
    }

    /// Reopens an existing project graph read-only through the retained
    /// registered runtime without inferring configuration authority.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn open_project_graph_read_only_for_test(
        &self,
        project_root: &Path,
        open_options: crate::tracedecay::TraceDecayOpenOptions,
    ) -> tracedecay_runtime_core::errors::Result<crate::tracedecay::TraceDecay> {
        let (store_layout, project_database) = self
            .registered_project_open_inputs(project_root, &open_options)
            .await?;
        crate::tracedecay::TraceDecay::open_read_only_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            project_database,
            Arc::clone(&self.profile_database),
            Arc::clone(&self._session_registry),
        )
        .await
    }

    pub fn facade(&self) -> HostAdmissionFacade<'_> {
        match (self.project_id.as_ref(), self.project_registered.as_ref()) {
            (Some(project_id), Some(project_registered)) => HostAdmissionFacade::new(
                HostAdmissionAuthorities::registered_for_project(
                    self.brain_id.clone(),
                    self.profile_id.clone(),
                    project_id.clone(),
                    project_registered,
                )
                .with_profile_registered(self.profile_id.clone(), self.profile_registered.as_ref()),
            ),
            _ => HostAdmissionFacade::new(HostAdmissionAuthorities::registered_for_profile(
                self.brain_id.clone(),
                self.profile_id.clone(),
                self.profile_registered.as_ref(),
            )),
        }
    }

    pub fn database_path(&self, scope: HostAdmissionScope) -> Option<&Path> {
        match scope {
            HostAdmissionScope::Project => self
                .project_registered
                .as_ref()
                .map(|database| database.db_path()),
            HostAdmissionScope::Profile => Some(self.profile_registered.db_path()),
        }
    }

    #[doc(hidden)]
    pub fn session_temporal_store_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<
        crate::store::session::GlobalDbSessionTemporalStore<'_>,
    > {
        Ok(crate::store::session::GlobalDbSessionTemporalStore::new(
            self.session_database_for_test(scope)?,
        ))
    }

    #[doc(hidden)]
    pub async fn ensure_session_cursor_key_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<tracedecay_domain::SignedCursorKeyRefV1> {
        self.session_database_for_test(scope)?
            .ensure_active_session_cursor_key_result()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "provision test session cursor authentication key".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn session_temporal_fixture_count_for_test(
        &self,
        scope: HostAdmissionScope,
        kind: SessionTemporalFixtureCountV1,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        let table = match kind {
            SessionTemporalFixtureCountV1::ProjectionReceipts => {
                "session_temporal_projection_receipts"
            }
            SessionTemporalFixtureCountV1::Occurrences => "session_occurrences",
            SessionTemporalFixtureCountV1::LogicalCopyEdges => "session_logical_copy_edges",
            SessionTemporalFixtureCountV1::Assertions => "session_assertions",
            SessionTemporalFixtureCountV1::RefreshReceipts => "session_refresh_receipts",
            SessionTemporalFixtureCountV1::RefreshProgress => "session_refresh_progress",
            SessionTemporalFixtureCountV1::RefreshOperations => "session_refresh_operations",
            SessionTemporalFixtureCountV1::RefreshBindings => "session_refresh_bindings",
            SessionTemporalFixtureCountV1::RefreshBatchBindings => "session_refresh_batch_bindings",
            SessionTemporalFixtureCountV1::TemporalGenerations => "session_temporal_generations",
        };
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open session-temporal fixture count snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query session-temporal fixture count".to_string(),
                    message: error.to_string(),
                },
            )?;
        let row = rows
            .next()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read session-temporal fixture count".to_string(),
                    message: error.to_string(),
                },
            )?
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read session-temporal fixture count".to_string(),
                    message: "count query returned no row".to_string(),
                },
            )?;
        row.get::<i64>(0).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "decode session-temporal fixture count".to_string(),
                message: error.to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub async fn session_temporal_copy_edge_for_test(
        &self,
        scope: HostAdmissionScope,
        session_id: &tracedecay_domain::SessionId,
    ) -> tracedecay_runtime_core::errors::Result<Option<(i64, tracedecay_domain::TemporalValidityV1)>>
    {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open session-temporal copy edge snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(
                "SELECT knowledge_at, valid_time_json
                 FROM session_logical_copy_edges
                 WHERE session_id = ?1",
                [session_id.as_str()],
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query session-temporal copy edge fixture".to_string(),
                    message: error.to_string(),
                },
            )?;
        let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read session-temporal copy edge fixture".to_string(),
                message: error.to_string(),
            }
        })?
        else {
            return Ok(None);
        };
        let knowledge_at = row.get::<i64>(0).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "decode session-temporal copy edge knowledge time".to_string(),
                message: error.to_string(),
            }
        })?;
        let valid_time_json = row.get::<String>(1).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "decode session-temporal copy edge valid time".to_string(),
                message: error.to_string(),
            }
        })?;
        let valid_time = serde_json::from_str(&valid_time_json).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "parse session-temporal copy edge valid time".to_string(),
                message: error.to_string(),
            }
        })?;
        Ok(Some((knowledge_at, valid_time)))
    }

    #[doc(hidden)]
    pub async fn session_temporal_generation_for_test(
        &self,
        scope: HostAdmissionScope,
        session_id: &tracedecay_domain::SessionId,
        state: &str,
    ) -> tracedecay_runtime_core::errors::Result<Option<u64>> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open session-temporal generation snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(
                "SELECT generation FROM session_temporal_generations
                 WHERE session_id = ?1 AND state = ?2",
                tracedecay_runtime_core::db::engine::params![session_id.as_str(), state],
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query session-temporal generation fixture".to_string(),
                    message: error.to_string(),
                },
            )?;
        let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read session-temporal generation fixture".to_string(),
                message: error.to_string(),
            }
        })?
        else {
            return Ok(None);
        };
        let generation = row.get::<i64>(0).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "decode session-temporal generation fixture".to_string(),
                message: error.to_string(),
            }
        })?;
        u64::try_from(generation).map(Some).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "validate session-temporal generation fixture".to_string(),
                message: error.to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub async fn checkpoint_profile_database_for_test(&self) {
        self.profile_database.checkpoint().await;
    }

    #[doc(hidden)]
    pub async fn checkpoint_session_database_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.session_database_for_test(scope)?.checkpoint().await;
        Ok(())
    }

    #[doc(hidden)]
    pub async fn snapshot_profile_database_for_test(
        &self,
        destination: &Path,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.profile_database.snapshot_to(destination).await
    }

    #[doc(hidden)]
    pub async fn snapshot_session_database_for_test(
        &self,
        scope: HostAdmissionScope,
        destination: &Path,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.session_database_for_test(scope)?
            .snapshot_to(destination)
            .await
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn session_database_sha256_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<[u8; 32]> {
        self.checkpoint_session_database_for_test(scope).await?;
        let bytes = std::fs::read(self.session_database_for_test(scope)?.db_path())?;
        Ok(Sha256::digest(bytes).into())
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn session_domain_sha256_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<[u8; 32]> {
        self.checkpoint_session_database_for_test(scope).await?;
        canonical_session_domain_sha256(self.session_database_for_test(scope)?.db_path())
    }

    #[doc(hidden)]
    pub async fn validate_profile_registry_schema_contract_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.profile_database
            .validate_registry_schema_contract_for_test()
            .await
    }

    #[doc(hidden)]
    pub async fn upsert(&self, project_path: &Path, tokens_saved: u64) {
        self.profile_database
            .upsert(project_path, tokens_saved)
            .await;
    }

    /// Fails the calling test loudly: a read this runtime could not perform is
    /// not a token total of zero.
    #[doc(hidden)]
    pub async fn get_project_tokens(&self, project_path: &Path) -> u64 {
        self.profile_database
            .try_get_project_tokens(project_path)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "could not read project tokens for '{}': {error}",
                    project_path.display()
                )
            })
    }

    #[doc(hidden)]
    pub async fn global_tokens_saved(&self) -> Option<u64> {
        self.profile_database.global_tokens_saved().await
    }

    #[doc(hidden)]
    pub async fn record_savings_for_test(
        &self,
        project: &str,
        tool: &str,
        before: u64,
        after: u64,
        timestamp: i64,
    ) {
        self.profile_database
            .record_savings(project, tool, before, after, timestamp)
            .await;
    }

    #[doc(hidden)]
    pub async fn sum_savings_for_test(
        &self,
        project: Option<&str>,
        since: i64,
    ) -> tracedecay_global_db::SavingsTotal {
        self.profile_database.sum_savings(project, since).await
    }

    #[doc(hidden)]
    pub async fn savings_history_for_test(
        &self,
        project: Option<&str>,
        since: i64,
    ) -> Vec<tracedecay_global_db::SavingsDay> {
        self.profile_database.savings_history(project, since).await
    }

    #[doc(hidden)]
    pub async fn delete_project(&self, project_path: &Path) {
        self.profile_database.delete_project(project_path).await;
    }

    #[doc(hidden)]
    pub async fn delete_projects(&self, project_paths: &[String]) -> usize {
        self.profile_database.delete_projects(project_paths).await
    }

    #[doc(hidden)]
    pub async fn list_project_paths_compat(&self) -> Vec<String> {
        self.profile_database.list_project_paths_compat().await
    }

    #[doc(hidden)]
    pub async fn registered_project_paths_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<Vec<PathBuf>> {
        self.profile_database
            .try_list_code_project_paths(usize::MAX)
            .await
    }

    #[doc(hidden)]
    pub async fn registered_project_roots_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<Vec<PathBuf>> {
        tracedecay_sessions::runtime::registered_project_roots_from(self.profile_database.as_ref())
            .await
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "list registered project roots for test".to_string(),
                    message: "project registry unavailable".to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub fn profile_relative_path_for_test(
        &self,
        path: &Path,
    ) -> tracedecay_runtime_core::errors::Result<PathBuf> {
        let profile_root = self.profile_database.db_path().parent().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "resolve test profile-relative path".to_string(),
                message: "profile database has no parent directory".to_string(),
            }
        })?;
        path.strip_prefix(profile_root)
            .map(Path::to_path_buf)
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "resolve test profile-relative path".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn append_profile_analytics_event_for_test(
        &self,
        event: &tracedecay_global_db::AnalyticsEventInsert,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        self.append_analytics_event_for_test(HostAdmissionScope::Profile, event)
            .await
    }

    #[doc(hidden)]
    pub async fn append_analytics_event_for_test(
        &self,
        scope: HostAdmissionScope,
        event: &tracedecay_global_db::AnalyticsEventInsert,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        let database = match scope {
            HostAdmissionScope::Project => self.project_database_for_test()?,
            HostAdmissionScope::Profile => self.profile_database.as_ref(),
        };
        database
            .append_analytics_event(event)
            .await
            .map_err(
                |message| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "append registered analytics event".to_string(),
                    message,
                },
            )
    }

    #[doc(hidden)]
    pub async fn append_profile_analytics_events_for_test(
        &self,
        events: &[tracedecay_global_db::AnalyticsEventInsert],
    ) -> tracedecay_runtime_core::errors::Result<Vec<i64>> {
        self.append_analytics_events_for_test(HostAdmissionScope::Profile, events)
            .await
    }

    #[doc(hidden)]
    pub async fn append_analytics_events_for_test(
        &self,
        scope: HostAdmissionScope,
        events: &[tracedecay_global_db::AnalyticsEventInsert],
    ) -> tracedecay_runtime_core::errors::Result<Vec<i64>> {
        let database = match scope {
            HostAdmissionScope::Project => self.project_database_for_test()?,
            HostAdmissionScope::Profile => self.profile_database.as_ref(),
        };
        database
            .append_analytics_events(events)
            .await
            .map_err(
                |message| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "append registered analytics event batch".to_string(),
                    message,
                },
            )
    }

    #[doc(hidden)]
    pub async fn insert_turn_for_test(
        &self,
        turn: &tracedecay_runtime_core::types::CostTurn,
    ) -> bool {
        self.profile_database.insert_turn(turn).await
    }

    #[doc(hidden)]
    pub async fn insert_turns_for_test(
        &self,
        turns: &[tracedecay_runtime_core::types::CostTurn],
    ) -> usize {
        self.profile_database.insert_turns(turns).await
    }

    #[doc(hidden)]
    pub async fn query_profile_analytics_events_for_test(
        &self,
        query: &tracedecay_global_db::AnalyticsEventQuery,
    ) -> tracedecay_runtime_core::errors::Result<Vec<tracedecay_global_db::AnalyticsEventRecord>>
    {
        self.profile_database
            .query_analytics_events(query)
            .await
            .map_err(
                |message| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query registered profile analytics events".to_string(),
                    message,
                },
            )
    }

    #[doc(hidden)]
    pub async fn profile_analytics_indexes_present_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        let snapshot = self.profile_database.read_snapshot().await?;
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index'
                   AND name IN (
                       'idx_analytics_events_project_time',
                       'idx_analytics_events_timestamp'
                   )",
                (),
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query registered profile analytics indexes".to_string(),
                    message: error.to_string(),
                },
            )?;
        let row = rows
            .next()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read registered profile analytics indexes".to_string(),
                    message: error.to_string(),
                },
            )?
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read registered profile analytics indexes".to_string(),
                    message: "count query returned no row".to_string(),
                },
            )?;
        row.get(0).map_err(
            |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "decode registered profile analytics indexes".to_string(),
                message: error.to_string(),
            },
        )
    }

    #[doc(hidden)]
    pub async fn import_profile_hook_analytics_for_test(
        &self,
        sources: &[crate::analytics_bridge::HookImportSource],
    ) -> crate::analytics_bridge::HookImportOutcome {
        crate::analytics_bridge::import_hook_analytics(self.profile_database.as_ref(), sources)
            .await
    }

    #[doc(hidden)]
    pub(crate) async fn correlate_hint_outcomes_for_test(
        &self,
        scope: HostAdmissionScope,
        project_id: &str,
        now: i64,
    ) -> crate::hooks::hint_outcomes::HintOutcomeStats {
        let Ok(session_database) = self.session_database_for_test(scope) else {
            return crate::hooks::hint_outcomes::HintOutcomeStats::default();
        };
        crate::hooks::hint_outcomes::correlate_hint_outcomes(
            self.profile_database.as_ref(),
            session_database,
            project_id,
            now,
        )
        .await
    }

    #[doc(hidden)]
    pub async fn upsert_code_project(
        &self,
        project_id: &str,
        project_root: &Path,
        git_common_dir: Option<&Path>,
        git_remote_url: Option<&str>,
        default_branch: Option<&str>,
    ) -> Option<tracedecay_global_db::CodeProjectRecord> {
        self.profile_database
            .upsert_code_project(
                project_id,
                project_root,
                git_common_dir,
                git_remote_url,
                default_branch,
            )
            .await
    }

    #[doc(hidden)]
    pub async fn upsert_project_alias(
        &self,
        alias_path: &Path,
        project_id: &str,
    ) -> Option<tracedecay_global_db::ProjectAliasRecord> {
        self.profile_database
            .upsert_project_alias(alias_path, project_id)
            .await
    }

    #[doc(hidden)]
    pub async fn clear_project_aliases_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<u64> {
        let transaction = self.profile_database.begin_write_transaction().await?;
        let deleted = transaction
            .execute("DELETE FROM project_aliases", ())
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "clear registered project aliases for test".to_string(),
                    message: error.to_string(),
                },
            )?;
        transaction.commit().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "commit registered project alias cleanup for test".to_string(),
                message: error.to_string(),
            }
        })?;
        Ok(deleted)
    }

    #[doc(hidden)]
    pub async fn upsert_store_instance(
        &self,
        upsert: tracedecay_global_db::StoreInstanceUpsert,
    ) -> Option<tracedecay_global_db::StoreInstanceRecord> {
        self.profile_database.upsert_store_instance(upsert).await
    }

    #[doc(hidden)]
    pub async fn upsert_graph_scope(
        &self,
        upsert: tracedecay_global_db::GraphScopeUpsert,
    ) -> Option<tracedecay_global_db::GraphScopeRecord> {
        self.profile_database.upsert_graph_scope(upsert).await
    }

    #[doc(hidden)]
    pub async fn upsert_store_artifact(
        &self,
        upsert: tracedecay_global_db::StoreArtifactUpsert,
    ) -> Option<tracedecay_global_db::StoreArtifactRecord> {
        self.profile_database.upsert_store_artifact(upsert).await
    }

    #[doc(hidden)]
    pub async fn delete_code_projects(&self, project_ids: &[String]) -> usize {
        self.profile_database
            .delete_code_projects(project_ids)
            .await
    }

    #[doc(hidden)]
    pub async fn get_code_project(
        &self,
        project_id: &str,
    ) -> Option<tracedecay_global_db::CodeProjectRecord> {
        self.profile_database.get_code_project(project_id).await
    }

    #[doc(hidden)]
    pub async fn plan_registry_reap(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<tracedecay_global_db::RegistryReapPlan> {
        self.profile_database.plan_registry_reap().await
    }

    #[doc(hidden)]
    pub async fn apply_registry_reap(
        &self,
        plan: &tracedecay_global_db::RegistryReapPlan,
    ) -> tracedecay_runtime_core::errors::Result<usize> {
        self.profile_database.apply_registry_reap(plan).await
    }

    #[doc(hidden)]
    pub async fn list_code_projects(
        &self,
        limit: usize,
    ) -> Vec<tracedecay_global_db::CodeProjectRecord> {
        self.profile_database
            .list_code_projects(limit)
            .await
            .unwrap_or_default()
    }

    #[doc(hidden)]
    pub async fn search_code_projects(
        &self,
        query: &str,
        limit: usize,
    ) -> Vec<tracedecay_global_db::CodeProjectRecord> {
        self.profile_database
            .search_code_projects(query, limit)
            .await
    }

    #[doc(hidden)]
    pub async fn project_registry_context_by_id(
        &self,
        project_id: &str,
    ) -> Option<tracedecay_global_db::ProjectRegistryContext> {
        self.profile_database
            .project_registry_context_by_id(project_id)
            .await
            .ok()
            .flatten()
    }

    #[doc(hidden)]
    pub async fn project_registry_context_by_alias(
        &self,
        alias_path: &Path,
    ) -> tracedecay_runtime_core::errors::Result<Option<tracedecay_global_db::ProjectRegistryContext>>
    {
        self.profile_database
            .project_registry_context_by_alias(alias_path)
            .await
    }

    #[doc(hidden)]
    pub async fn project_registry_context_by_identity(
        &self,
        project_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> tracedecay_runtime_core::errors::Result<Option<tracedecay_global_db::ProjectRegistryContext>>
    {
        self.profile_database
            .project_registry_context_by_identity(project_root, git_common_dir)
            .await
    }

    #[doc(hidden)]
    pub async fn resolve_project_store_by_alias(
        &self,
        alias_path: &Path,
    ) -> Option<tracedecay_global_db::ProjectStoreResolution> {
        self.profile_database
            .resolve_project_store_by_alias(alias_path)
            .await
    }

    #[doc(hidden)]
    pub async fn resolve_project_store_by_identity(
        &self,
        project_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> tracedecay_runtime_core::errors::Result<Option<tracedecay_global_db::ProjectStoreResolution>>
    {
        self.profile_database
            .resolve_project_store_by_identity(project_root, git_common_dir)
            .await
    }

    #[doc(hidden)]
    pub async fn resolve_unique_project_store_by_git_remote(
        &self,
        git_remote_url: &str,
    ) -> Option<tracedecay_global_db::ProjectStoreResolution> {
        self.profile_database
            .resolve_unique_project_store_by_git_remote(git_remote_url)
            .await
    }

    #[doc(hidden)]
    pub async fn resolve_project_observation_store(
        &self,
        project_root: &Path,
    ) -> Result<
        tracedecay_global_db::ProjectObservationStoreResolution,
        tracedecay_global_db::ProjectObservationStoreError,
    > {
        self.profile_database
            .resolve_project_observation_store(project_root)
            .await
    }

    #[doc(hidden)]
    pub async fn apply_registry_reconstruction_report(
        &self,
        report: &tracedecay_migrate::registry::RegistryReconstructionReport,
    ) -> Result<tracedecay_migrate::registry::RegistryReconstructionApplyReport, Vec<String>> {
        tracedecay_migrate::registry::apply_registry_reconstruction_report(
            self.profile_database.as_ref(),
            report,
        )
        .await
    }

    #[doc(hidden)]
    pub async fn apply_single_registry_reconstruction_report(
        &self,
        report: &tracedecay_migrate::registry::RegistryReconstructionReport,
    ) -> Result<tracedecay_migrate::registry::RegistryReconstructionApplyReport, Vec<String>> {
        tracedecay_migrate::registry::apply_single_registry_reconstruction_report(
            self.profile_database.as_ref(),
            report,
        )
        .await
    }

    /// Retained registered session authorities for end-to-end MCP tests.
    #[doc(hidden)]
    pub fn mcp_session_authorities(&self) -> crate::mcp::tools::SessionAuthorities<'_> {
        crate::mcp::tools::SessionAuthorities::new(
            self.project_registered.as_ref(),
            Some(&self.profile_registered),
        )
        .with_registered_databases(
            self.project_registered.as_deref(),
            Some(self.profile_registered.as_ref()),
        )
    }

    #[doc(hidden)]
    pub(crate) fn unregistered_mcp_session_authorities_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> crate::mcp::tools::SessionAuthorities<'_> {
        match scope {
            HostAdmissionScope::Project => {
                crate::mcp::tools::SessionAuthorities::new(self.project_registered.as_ref(), None)
            }
            HostAdmissionScope::Profile => {
                crate::mcp::tools::SessionAuthorities::new(None, Some(&self.profile_registered))
            }
        }
    }

    #[doc(hidden)]
    pub(crate) fn host_admission_broker_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<SharedHostAdmissionBroker> {
        let database = self.session_database_for_test(scope)?;
        let (runtime, _) =
            HostAdmissionRuntime::open_for_database(database.db_path()).map_err(|outcome| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open registered host-admission test broker".to_string(),
                    message: outcome
                        .reason_code
                        .unwrap_or("spool_runtime_unavailable")
                        .to_string(),
                }
            })?;
        Ok(Arc::new(HostAdmissionBroker::new(runtime)))
    }

    /// Runs workflow ingestion through this runtime's exact ProjectSessions mount.
    #[doc(hidden)]
    pub async fn ingest_workflows_for_test(
        &self,
        project_root: &Path,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_sessions::runtime::workflow_ingest::WorkflowIngestStats,
    > {
        let project_id = self.project_id.as_ref().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "ingest workflow test fixture".to_string(),
                message: "project session authority is unavailable".to_string(),
            }
        })?;
        let database = self.project_registered.as_deref().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "ingest workflow test fixture".to_string(),
                message: "registered ProjectSessions mount is unavailable".to_string(),
            }
        })?;
        Ok(crate::store::GlobalDbWorkflowStore::new(database)
            .ingest_workflow_runs(project_id, project_root)
            .await)
    }

    /// Records one git span through this runtime's retained ProjectSessions authority.
    #[doc(hidden)]
    pub async fn record_project_span_for_test(
        &self,
        observation: &tracedecay_sessions::runtime::git_correlation::SpanObservation,
        merge_gap_secs: i64,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        let database = self.project_registered.as_deref().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "record workflow test git span".to_string(),
                message: "registered ProjectSessions mount is unavailable".to_string(),
            }
        })?;
        let transaction = database.begin_write_transaction().await?;
        let span_id =
            tracedecay_sessions::runtime::git_correlation::record_span_observation_in_transaction(
                &transaction,
                observation,
                merge_gap_secs,
            )
            .await
            .map_err(|error| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "record workflow test git span".to_string(),
                    message: error.to_string(),
                }
            })?;
        transaction.commit().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "commit workflow test git span".to_string(),
                message: error.to_string(),
            }
        })?;
        Ok(span_id)
    }

    #[cfg(test)]
    pub(crate) async fn read_snapshot(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::db::engine::Result<
        tracedecay_runtime_core::db::engine::ReadSnapshot,
    > {
        match scope {
            HostAdmissionScope::Project => self
                .project_registered
                .as_ref()
                .ok_or_else(|| {
                    tracedecay_runtime_core::db::engine::Error::invalid_operation(
                        "registered project test runtime unavailable",
                    )
                })?
                .read_snapshot()
                .await
                .map_err(|error| {
                    tracedecay_runtime_core::db::engine::Error::invalid_operation(error.to_string())
                }),
            HostAdmissionScope::Profile => {
                self.profile_registered
                    .read_snapshot()
                    .await
                    .map_err(|error| {
                        tracedecay_runtime_core::db::engine::Error::invalid_operation(
                            error.to_string(),
                        )
                    })
            }
        }
    }

    pub(crate) fn registered_database(
        &self,
        scope: HostAdmissionScope,
    ) -> Option<&RegisteredGlobalDb> {
        match scope {
            HostAdmissionScope::Project => self.project_registered.as_deref(),
            HostAdmissionScope::Profile => Some(self.profile_registered.as_ref()),
        }
    }

    #[cfg(test)]
    pub(crate) fn registered_database_arc(
        &self,
        scope: HostAdmissionScope,
    ) -> Option<Arc<RegisteredGlobalDb>> {
        match scope {
            HostAdmissionScope::Project => self.project_registered.clone(),
            HostAdmissionScope::Profile => Some(Arc::clone(&self.profile_registered)),
        }
    }

    pub(crate) async fn remount_profile_database_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<Arc<RegisteredGlobalDb>> {
        self._session_registry.profile_sessions().await
    }

    #[doc(hidden)]
    pub fn observation_store(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<GlobalDbObservationStore<'_>, HostAdmissionOutcome> {
        self.facade().observation_store(scope)
    }

    #[doc(hidden)]
    pub fn session_temporal_store(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<crate::store::session::GlobalDbSessionTemporalStore<'_>, HostAdmissionOutcome> {
        let database = self.registered_database(scope).ok_or_else(|| {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("registered_authority_unavailable"),
            )
        })?;
        Ok(crate::store::session::GlobalDbSessionTemporalStore::new(
            database,
        ))
    }

    fn project_database_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<&RegisteredGlobalDb> {
        self.project_registered.as_deref().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind registered project session test runtime".to_string(),
                message: "registered ProjectSessions mount is unavailable".to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub(crate) fn project_observation_database_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<&RegisteredGlobalDb> {
        self.project_database_for_test()
    }

    #[doc(hidden)]
    pub(crate) fn project_observation_database_arc_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<Arc<RegisteredGlobalDb>> {
        self.project_registered.clone().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind registered project Work test runtime".to_string(),
                message: "registered ProjectSessions mount is unavailable".to_string(),
            }
        })
    }

    fn session_database_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<&RegisteredGlobalDb> {
        match scope {
            HostAdmissionScope::Project => self.project_database_for_test(),
            HostAdmissionScope::Profile => Ok(self.profile_registered.as_ref()),
        }
    }

    #[doc(hidden)]
    pub fn session_database_identity_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<HostAdmissionDatabaseIdentityV1> {
        use sha2::{Digest as _, Sha256};

        let database = self.session_database_for_test(scope)?;
        let digest: [u8; 32] =
            Sha256::digest(database.db_path().as_os_str().as_encoded_bytes()).into();
        Ok(HostAdmissionDatabaseIdentityV1(digest))
    }

    #[doc(hidden)]
    pub fn session_database_storage_bytes_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<u64> {
        let database = self.session_database_for_test(scope)?;
        let mut total = 0u64;
        for suffix in ["", "-wal", "-shm"] {
            let mut path = database.db_path().as_os_str().to_os_string();
            path.push(suffix);
            match std::fs::metadata(PathBuf::from(path)) {
                Ok(metadata) => total = total.saturating_add(metadata.len()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(tracedecay_runtime_core::errors::TraceDecayError::Database {
                        operation: "read retained session database storage bytes".to_string(),
                        message: error.to_string(),
                    });
                }
            }
        }
        Ok(total)
    }

    #[doc(hidden)]
    pub async fn session_activity_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<crate::automation::scheduler::SessionActivity>
    {
        Ok(crate::automation::scheduler::load_session_activity(
            self.session_database_for_test(scope)?,
        )
        .await)
    }

    fn primary_session_database_for_test(&self) -> &RegisteredGlobalDb {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
    }

    #[cfg(test)]
    pub(crate) async fn ensure_runtime_configuration_for_test(
        &self,
        project_root: &Path,
        layout: &tracedecay_runtime_core::storage::StoreLayout,
    ) -> tracedecay_runtime_core::errors::Result<crate::config::PinnedRuntimeConfiguration> {
        crate::config::ensure_runtime_configuration_for_registered_database(
            project_root,
            layout,
            self.project_registered.clone().ok_or_else(|| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "bind configuration test project sessions".to_string(),
                    message: "registered ProjectSessions mount is unavailable".to_string(),
                }
            })?,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn resolve_runtime_configuration_for_test(
        &self,
        project_root: &Path,
        layout: &tracedecay_runtime_core::storage::StoreLayout,
    ) -> tracedecay_runtime_core::errors::Result<crate::config::PinnedRuntimeConfiguration> {
        crate::config::resolve_runtime_configuration_for_registered_database(
            project_root,
            layout,
            self.project_registered.clone().ok_or_else(|| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "bind configuration test project sessions".to_string(),
                    message: "registered ProjectSessions mount is unavailable".to_string(),
                }
            })?,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn load_runtime_configuration_read_only_for_test(
        &self,
        project_root: &Path,
        layout: &tracedecay_runtime_core::storage::StoreLayout,
    ) -> tracedecay_runtime_core::errors::Result<crate::config::PinnedRuntimeConfiguration> {
        crate::config::load_runtime_configuration_for_registered_database_read_only(
            project_root,
            layout,
            self.project_registered.clone().ok_or_else(|| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "bind read-only configuration test project sessions".to_string(),
                    message: "registered ProjectSessions mount is unavailable".to_string(),
                }
            })?,
        )
        .await
    }

    pub(crate) fn project_configuration_control_store_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_global_db::configuration::OwnedGlobalDbConfigurationControlStore,
    > {
        let database = self.project_registered.clone().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind configuration control test project sessions".to_string(),
                message: "registered ProjectSessions mount is unavailable".to_string(),
            }
        })?;
        Ok(
            tracedecay_global_db::configuration::OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(database),
        )
    }

    #[cfg(test)]
    pub(crate) fn into_session_temporal_refresh_test_authority(
        self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<
        crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshTestAuthority,
    > {
        let database = match scope {
            HostAdmissionScope::Project => self.project_registered.clone(),
            HostAdmissionScope::Profile => Some(Arc::clone(&self.profile_registered)),
        }
        .ok_or_else(
            || tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind session temporal refresh test authority".to_string(),
                message: "registered session database mount is unavailable".to_string(),
            },
        )?;
        Ok(
            crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshTestAuthority::new(
                self, database,
            ),
        )
    }

    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub(crate) fn into_mcp_server_context_for_test(
        self,
        cg: crate::tracedecay::TraceDecay,
        scope_prefix: Option<String>,
    ) -> tracedecay_runtime_core::errors::Result<crate::mcp::server::McpServerConstructionContext>
    {
        Arc::new(self).mcp_server_context_for_test(cg, scope_prefix)
    }

    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub(crate) fn mcp_server_context_for_test(
        self: Arc<Self>,
        cg: crate::tracedecay::TraceDecay,
        scope_prefix: Option<String>,
    ) -> tracedecay_runtime_core::errors::Result<crate::mcp::server::McpServerConstructionContext>
    {
        let profile_root = self.profile_root.clone();
        let transcript_source_home =
            profile_root
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(
                    || tracedecay_runtime_core::errors::TraceDecayError::Config {
                        message: format!(
                            "test profile '{}' has no isolated transcript-source home",
                            profile_root.display()
                        ),
                    },
                )?;
        let project_sessions = self.project_registered.clone().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind MCP test project sessions".to_string(),
                message: "registered ProjectSessions mount is unavailable".to_string(),
            }
        })?;
        let profile_database = Arc::clone(&self.profile_database);
        let profile_sessions = Arc::clone(&self.profile_registered);
        let profile_identity = crate::daemon::profile_identity::load_or_create(&profile_root)?;
        let mut context =
            crate::mcp::server::McpServerConstructionContext::direct(cg, scope_prefix)
                .with_direct_databases(
                    Some(Arc::clone(&profile_database)),
                    Some(profile_database),
                    Some(project_sessions),
                    Some(profile_sessions),
                );
        context.profile_root = Some(profile_root);
        context.profile_identity = Some(profile_identity);
        context.transcript_source_home = Some(transcript_source_home);
        context.host_admission_test_runtime = Some(self);
        Ok(context)
    }

    #[doc(hidden)]
    pub(crate) fn dashboard_test_authority(
        self: &Arc<Self>,
    ) -> tracedecay_runtime_core::errors::Result<
        crate::dashboard::DashboardHostAdmissionTestAuthorityV1,
    > {
        let project_sessions = self.project_registered.clone().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind dashboard test project sessions".to_string(),
                message: "registered ProjectSessions mount is unavailable".to_string(),
            }
        })?;
        Ok(
            crate::dashboard::DashboardHostAdmissionTestAuthorityV1::new(
                Arc::clone(self),
                Arc::clone(&self.profile_database),
                project_sessions,
            ),
        )
    }

    #[doc(hidden)]
    pub fn transcript_store_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<crate::store::GlobalDbTranscriptStore<'_>> {
        Ok(crate::store::GlobalDbTranscriptStore::new(
            self.session_database_for_test(scope)?,
        ))
    }

    #[doc(hidden)]
    pub async fn parse_offset_for_test(
        &self,
        scope: HostAdmissionScope,
        path: &str,
    ) -> tracedecay_runtime_core::errors::Result<Option<tracedecay_global_db::ParseOffset>> {
        Ok(self
            .session_database_for_test(scope)?
            .get_parse_offset(path)
            .await)
    }

    #[doc(hidden)]
    pub async fn set_parse_offset_for_test(
        &self,
        scope: HostAdmissionScope,
        path: &str,
        offset: tracedecay_global_db::ParseOffset,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.session_database_for_test(scope)?
            .set_parse_offset(path, offset)
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "set retained test parse offset".to_string(),
                    message: error.clone(),
                },
            )?;
        Ok(())
    }

    #[doc(hidden)]
    pub async fn session_message_count_for_test(
        &self,
        scope: HostAdmissionScope,
        project_key: Option<&str>,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        let database = self.session_database_for_test(scope)?;
        let result = match project_key {
            Some(project_key) => {
                database
                    .session_message_count_for_project(project_key)
                    .await
            }
            None => database.session_message_count().await,
        };
        result.map_err(
            |message| tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "count registered session messages".to_string(),
                message,
            },
        )
    }

    #[doc(hidden)]
    pub async fn session_ingest_health_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: Option<&str>,
    ) -> tracedecay_runtime_core::errors::Result<tracedecay_global_db::SessionIngestHealth> {
        let database = self.session_database_for_test(scope)?;
        database
            .session_ingest_health_for_provider(provider)
            .await
            .map_err(
                |message| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read registered session ingest health".to_string(),
                    message,
                },
            )
    }

    #[doc(hidden)]
    pub async fn session_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        session_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<Option<tracedecay_sessions::runtime::SessionRecord>>
    {
        Ok(self
            .session_database_for_test(scope)?
            .get_session(provider, session_id)
            .await)
    }

    #[doc(hidden)]
    pub async fn session_message_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<
        Option<tracedecay_sessions::runtime::SessionMessageRecord>,
    > {
        Ok(self
            .session_database_for_test(scope)?
            .get_session_message(provider, message_id)
            .await)
    }

    #[doc(hidden)]
    pub async fn transcript_store_counts_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        session_id: &str,
        transcript_path: &Path,
    ) -> tracedecay_runtime_core::errors::Result<(i64, i64, i64, i64, i64, i64, i64)> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open registered transcript store count snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(
                "SELECT
                    (SELECT COUNT(*) FROM sessions
                     WHERE provider = ?1 AND session_id = ?2),
                    (SELECT COUNT(*) FROM session_messages
                     WHERE provider = ?1 AND session_id = ?2),
                    (SELECT COUNT(*) FROM lcm_raw_messages
                     WHERE provider = ?1 AND session_id = ?2),
                    (SELECT COUNT(*) FROM lcm_raw_messages_fts
                     JOIN lcm_raw_messages raw
                       ON raw.store_id = lcm_raw_messages_fts.rowid
                     WHERE raw.provider = ?1 AND raw.session_id = ?2),
                    (SELECT COUNT(*) FROM lcm_raw_messages_fts),
                    (SELECT COUNT(*) FROM lcm_summary_nodes
                     WHERE provider = ?1 AND session_id = ?2),
                    (SELECT COUNT(*) FROM parse_offsets
                     WHERE file_path = ?3)",
                tracedecay_runtime_core::db::engine::params![
                    provider,
                    session_id,
                    transcript_path.to_string_lossy().as_ref()
                ],
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query registered transcript store counts".to_string(),
                    message: error.to_string(),
                },
            )?;
        let row = rows
            .next()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read registered transcript store counts".to_string(),
                    message: error.to_string(),
                },
            )?
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read registered transcript store counts".to_string(),
                    message: "count query returned no row".to_string(),
                },
            )?;
        let value = |index| {
            row.get::<i64>(index).map_err(|error| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "decode registered transcript store counts".to_string(),
                    message: error.to_string(),
                }
            })
        };
        Ok((
            value(0)?,
            value(1)?,
            value(2)?,
            value(3)?,
            value(4)?,
            value(5)?,
            value(6)?,
        ))
    }

    #[doc(hidden)]
    pub async fn set_parse_offset_insert_failure_for_test(
        &self,
        scope: HostAdmissionScope,
        enabled: bool,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let statement = if enabled {
            "CREATE TRIGGER fail_parse_offset_insert
             BEFORE INSERT ON parse_offsets
             BEGIN
                SELECT RAISE(ABORT, 'late parse offset failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS fail_parse_offset_insert;"
        };
        self.session_database_for_test(scope)?
            .writer_connection()?
            .execute_batch(statement)
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "configure registered parse-offset failure".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn inject_lcm_orphan_summary_source_for_test(
        &self,
        scope: HostAdmissionScope,
        store_id: i64,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let database = self.session_database_for_test(scope)?;
        let connection = rusqlite::Connection::open(database.db_path()).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "open out-of-band orphan summary fixture".to_string(),
                message: error.to_string(),
            }
        })?;
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "disable foreign keys out of band for orphan summary fixture"
                        .to_string(),
                    message: error.to_string(),
                },
            )?;
        connection
            .execute(
                "INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                 VALUES ('missing-summary-owner', 'raw_message', ?1, 0)",
                [store_id.to_string()],
            )
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "insert orphan summary source fixture".to_string(),
                    message: error.to_string(),
                },
            )?;
        Ok(())
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn inject_lcm_foreign_orphan_debt_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let database = self.session_database_for_test(scope)?;
        let connection = rusqlite::Connection::open(database.db_path()).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "open out-of-band orphan debt fixture".to_string(),
                message: error.to_string(),
            }
        })?;
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "disable foreign keys out of band for orphan debt fixture"
                        .to_string(),
                    message: error.to_string(),
                },
            )?;
        connection
            .execute(
                "INSERT INTO lcm_maintenance_debt(
                    provider, conversation_id, debt_id, debt_kind, from_store_id, to_store_id
                 )
                 VALUES ('cursor', 'lcm-doctor-debt-other', 'orphan-debt', 'raw_backlog', 1, 2)",
                (),
            )
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "insert foreign orphan debt fixture".to_string(),
                    message: error.to_string(),
                },
            )?;
        Ok(())
    }

    #[doc(hidden)]
    pub async fn clear_lcm_schema_migration_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.session_database_for_test(scope)?
            .writer_connection()?
            .execute(
                "DELETE FROM session_schema_migrations WHERE name = 'lcm'",
                (),
            )
            .await
            .map(|_| ())
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "clear lcm schema migration fixture".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn poison_lcm_raw_projection_for_test(
        &self,
        scope: HostAdmissionScope,
        store_id: i64,
        poison: &str,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.session_database_for_test(scope)?
            .writer_connection()?
            .execute(
                "UPDATE lcm_raw_messages
                 SET content = ?2, snippet_text = ?2, index_text = ?2
                 WHERE store_id = ?1",
                tracedecay_runtime_core::db::engine::params![store_id, poison],
            )
            .await
            .map(|_| ())
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "poison lcm raw projection fixture".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn set_lcm_schema_migration_version_for_test(
        &self,
        scope: HostAdmissionScope,
        version: i64,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.session_database_for_test(scope)?
            .writer_connection()?
            .execute(
                "UPDATE session_schema_migrations SET version = ?1 WHERE name = 'lcm'",
                [version],
            )
            .await
            .map(|_| ())
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "set lcm schema migration fixture version".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn lcm_schema_migration_version_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<Option<i64>> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT version FROM session_schema_migrations WHERE name = 'lcm'",
                (),
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query lcm schema migration fixture version".to_string(),
                    message: error.to_string(),
                },
            )?;
        let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read lcm schema migration fixture version".to_string(),
                message: error.to_string(),
            }
        })?
        else {
            return Ok(None);
        };
        row.get::<i64>(0).map(Some).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "decode lcm schema migration fixture version".to_string(),
                message: error.to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub async fn set_lcm_schema_migration_applied_at_for_test(
        &self,
        scope: HostAdmissionScope,
        applied_at: i64,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.session_database_for_test(scope)?
            .writer_connection()?
            .execute(
                "UPDATE session_schema_migrations
                 SET applied_at = ?1
                 WHERE name = 'lcm'",
                [applied_at],
            )
            .await
            .map(|_| ())
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "set lcm schema migration fixture applied_at".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn lcm_schema_migration_applied_at_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<Option<i64>> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT applied_at
                 FROM session_schema_migrations
                 WHERE name = 'lcm'",
                (),
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query lcm schema migration fixture applied_at".to_string(),
                    message: error.to_string(),
                },
            )?;
        let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read lcm schema migration fixture applied_at".to_string(),
                message: error.to_string(),
            }
        })?
        else {
            return Ok(None);
        };
        row.get::<i64>(0).map(Some).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "decode lcm schema migration fixture applied_at".to_string(),
                message: error.to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub async fn seed_transcript_backfill_for_test(
        &self,
        scope: HostAdmissionScope,
        session: &tracedecay_sessions::runtime::SessionRecord,
        messages: &[tracedecay_sessions::runtime::SessionMessageRecord],
    ) -> tracedecay_runtime_core::errors::Result<()> {
        if !self.upsert_session_for_test(scope, session).await? {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "seed transcript backfill session fixture".to_string(),
                message: "registered session fixture write failed".to_string(),
            });
        }
        for message in messages {
            if !self.upsert_session_message_for_test(scope, message).await? {
                return Err(tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "seed transcript backfill message fixture".to_string(),
                    message: format!(
                        "registered message fixture write failed for {}/{}",
                        message.provider, message.message_id
                    ),
                });
            }
        }
        self.session_database_for_test(scope)?
            .writer_connection()?
            .execute(
                "DELETE FROM session_schema_migrations
                 WHERE name = 'transcript_facts_backfill'",
                (),
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "clear transcript backfill fixture marker".to_string(),
                    message: error.to_string(),
                },
            )?;
        Ok(())
    }

    #[doc(hidden)]
    pub async fn transcript_backfill_marker_version_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<Option<i64>> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open transcript backfill marker snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(
                "SELECT version FROM session_schema_migrations
                 WHERE name = 'transcript_facts_backfill'",
                (),
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query transcript backfill marker".to_string(),
                    message: error.to_string(),
                },
            )?;
        let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read transcript backfill marker".to_string(),
                message: error.to_string(),
            }
        })?
        else {
            return Ok(None);
        };
        row.get::<i64>(0).map(Some).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "decode transcript backfill marker".to_string(),
                message: error.to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub async fn set_lcm_compression_debt_insert_failure_for_test(
        &self,
        scope: HostAdmissionScope,
        enabled: bool,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let statement = if enabled {
            "CREATE TRIGGER abort_compression_debt_insert
             BEFORE INSERT ON lcm_maintenance_debt
             BEGIN
                SELECT RAISE(ABORT, 'forced maintenance debt failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS abort_compression_debt_insert;"
        };
        self.session_database_for_test(scope)?
            .writer_connection()?
            .execute_batch(statement)
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "configure LCM compression-debt failure".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn set_lcm_late_summary_projection_failure_for_test(
        &self,
        scope: HostAdmissionScope,
        enabled: bool,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let statement = if enabled {
            "CREATE TRIGGER abort_late_summary_projection
             BEFORE INSERT ON lcm_summary_nodes
             BEGIN
                SELECT RAISE(ABORT, 'forced late summary projection failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS abort_late_summary_projection;"
        };
        self.session_database_for_test(scope)?
            .writer_connection()?
            .execute_batch(statement)
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "configure late LCM summary projection failure".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn replace_lcm_summary_source_for_test(
        &self,
        scope: HostAdmissionScope,
        node_id: &str,
        source_node_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let writer = self.session_database_for_test(scope)?.writer_connection()?;
        writer
            .execute(
                "DELETE FROM lcm_summary_sources WHERE node_id = ?1",
                (node_id,),
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "delete LCM summary source fixture".to_string(),
                    message: error.to_string(),
                },
            )?;
        writer
            .execute(
                "INSERT INTO lcm_summary_sources (node_id, source_kind, source_id, ordinal)
                 VALUES (?1, 'summary_node', ?2, 0)",
                (node_id, source_node_id),
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "replace LCM summary source fixture".to_string(),
                    message: error.to_string(),
                },
            )?;
        Ok(())
    }

    async fn lcm_session_row_count_for_test(
        &self,
        scope: HostAdmissionScope,
        table: &'static str,
        session_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open LCM session row-count snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(
                &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
                (session_id,),
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query LCM session row count".to_string(),
                    message: error.to_string(),
                },
            )?;
        let row = rows
            .next()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read LCM session row count".to_string(),
                    message: error.to_string(),
                },
            )?
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read LCM session row count".to_string(),
                    message: "count query returned no row".to_string(),
                },
            )?;
        row.get::<i64>(0).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "decode LCM session row count".to_string(),
                message: error.to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub async fn session_summary_node_count_for_test(
        &self,
        scope: HostAdmissionScope,
        session_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        self.lcm_session_row_count_for_test(scope, "session_summary_nodes", session_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_raw_message_count_for_test(
        &self,
        scope: HostAdmissionScope,
        session_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        self.lcm_session_row_count_for_test(scope, "lcm_raw_messages", session_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_summary_node_count_for_test(
        &self,
        scope: HostAdmissionScope,
        session_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        self.lcm_session_row_count_for_test(scope, "lcm_summary_nodes", session_id)
            .await
    }

    #[doc(hidden)]
    pub async fn wipe_lcm_raw_fts_for_test(
        &self,
        scope: HostAdmissionScope,
        message_id: Option<&str>,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let writer = self.session_database_for_test(scope)?.writer_connection()?;
        let result = match message_id {
            Some(message_id) => {
                writer
                    .execute(
                        "DELETE FROM lcm_raw_messages_fts
                         WHERE rowid = (
                             SELECT store_id FROM lcm_raw_messages
                             WHERE provider = 'cursor' AND message_id = ?1
                         )",
                        [message_id],
                    )
                    .await
            }
            None => writer.execute("DELETE FROM lcm_raw_messages_fts", ()).await,
        };
        result.map(|_| ()).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "wipe registered LCM raw-message FTS fixture".to_string(),
                message: error.to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub async fn drop_project_workflow_schema_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.project_database_for_test()?
            .writer_connection()?
            .execute_batch(
                "DROP TABLE workflow_agents;
                 DROP TABLE workflow_runs;",
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "drop registered project workflow schema fixture".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn set_project_parse_offset_for_test(
        &self,
        path: &str,
        offset: tracedecay_global_db::ParseOffset,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.project_database_for_test()?
            .advance_parse_offset_result(path, offset)
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "write registered project parse offset test seed".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn upsert_session_for_test(
        &self,
        scope: HostAdmissionScope,
        session: &tracedecay_sessions::runtime::SessionRecord,
    ) -> tracedecay_runtime_core::errors::Result<bool> {
        Ok(self
            .session_database_for_test(scope)?
            .upsert_session(session)
            .await)
    }

    #[doc(hidden)]
    pub async fn upsert_session_message_for_test(
        &self,
        scope: HostAdmissionScope,
        message: &tracedecay_sessions::runtime::SessionMessageRecord,
    ) -> tracedecay_runtime_core::errors::Result<bool> {
        let database = self.session_database_for_test(scope)?;
        let session = database
            .get_session(&message.provider, &message.session_id)
            .await
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "seed registered session message fixture".to_string(),
                    message: format!(
                        "session {}/{} is unavailable",
                        message.provider, message.session_id
                    ),
                },
            )?;
        let source = format!(
            "host-admission-test-message:{}:{}",
            message.provider, message.message_id
        );
        Ok(database
            .upsert_transcript_batch(
                &session,
                std::slice::from_ref(message),
                &source,
                tracedecay_global_db::ParseOffset::default(),
            )
            .await)
    }

    #[doc(hidden)]
    pub async fn ingest_profile_transcript_source_for_test(
        &self,
        source: &dyn tracedecay_sessions::runtime::source::TranscriptSource,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> tracedecay_sessions::runtime::source::TranscriptIngestResult<
        tracedecay_sessions::runtime::shared::TranscriptIngestStats,
    > {
        let database = self
            .session_database_for_test(HostAdmissionScope::Profile)
            .map_err(|error| {
                tracedecay_sessions::runtime::source::TranscriptIngestError::ScanIo {
                    operation: "bind registered profile session test runtime",
                    path: project_root.to_path_buf(),
                    source: std::io::Error::other(error.to_string()),
                }
            })?;
        let store = crate::store::GlobalDbTranscriptStore::new(database);
        tracedecay_sessions::runtime::source::try_ingest_source(
            &store,
            source,
            project_root,
            max_new_bytes,
        )
        .await
    }

    #[doc(hidden)]
    pub async fn search_session_messages_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> tracedecay_runtime_core::errors::Result<
        Vec<tracedecay_sessions::runtime::SessionMessageSearchResult>,
    > {
        Ok(self
            .session_database_for_test(scope)?
            .search_session_messages(provider, project_key, query, limit)
            .await)
    }

    #[doc(hidden)]
    pub async fn search_session_messages_filtered_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: tracedecay_sessions::runtime::SessionSearchFilters<'_>,
    ) -> tracedecay_runtime_core::errors::Result<
        Vec<tracedecay_sessions::runtime::SessionMessageSearchResult>,
    > {
        let fetch_limit = limit.saturating_mul(16).max(limit);
        let mut results = self
            .session_database_for_test(scope)?
            .search_session_messages(provider, project_key, query, fetch_limit)
            .await;
        results.retain(|result| {
            let scope_matches = match filters.scope {
                tracedecay_sessions::runtime::SessionSearchScope::All => true,
                tracedecay_sessions::runtime::SessionSearchScope::ParentsOnly => {
                    !result.session.is_subagent
                }
                tracedecay_sessions::runtime::SessionSearchScope::SubagentsOnly => {
                    result.session.is_subagent
                }
            };
            let tool_result = result.message.role == "tool"
                || matches!(
                    result.message.kind.as_deref(),
                    Some("tool_result" | "tool_output")
                )
                || result
                    .message
                    .metadata_json
                    .as_deref()
                    .and_then(|metadata| serde_json::from_str::<serde_json::Value>(metadata).ok())
                    .and_then(|metadata| metadata.get("tool_events").cloned())
                    .and_then(|events| events.as_array().cloned())
                    .is_some_and(|events| {
                        events.iter().any(|event| {
                            event.get("type").and_then(serde_json::Value::as_str)
                                == Some("tool_result")
                        })
                    });
            let message_type_matches = match filters.message_type {
                tracedecay_sessions::runtime::SessionMessageType::All => true,
                tracedecay_sessions::runtime::SessionMessageType::DirectUser => {
                    result.message.role == "user" && !tool_result
                }
                tracedecay_sessions::runtime::SessionMessageType::ToolResult => tool_result,
            };
            let parent_matches = filters
                .parent_session_id
                .is_none_or(|parent| result.session.parent_session_id.as_deref() == Some(parent));
            let time_matches =
                filters.time_range.start_time.is_none_or(|start| {
                    result.message.timestamp.is_some_and(|value| value >= start)
                }) && filters
                    .time_range
                    .end_time
                    .is_none_or(|end| result.message.timestamp.is_some_and(|value| value <= end));
            scope_matches && message_type_matches && parent_matches && time_matches
        });
        results.truncate(limit);
        Ok(results)
    }

    #[doc(hidden)]
    pub async fn search_session_messages_git_scoped_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: Option<&str>,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: tracedecay_sessions::runtime::SessionSearchFilters<'_>,
        git_filter: &tracedecay_sessions::runtime::git_correlation::GitScopeFilter,
    ) -> tracedecay_runtime_core::errors::Result<
        Vec<tracedecay_sessions::runtime::SessionMessageSearchResult>,
    > {
        let provider =
            provider.ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "search registered git-scoped session messages".to_string(),
                    message: "test facade requires an exact provider".to_string(),
                },
            )?;
        let database = self.session_database_for_test(scope)?;
        let snapshot = database.read_snapshot().await?;
        let scoped_ids = tracedecay_sessions::runtime::git_correlation::session_ids_for_scope(
            &snapshot, git_filter,
        )
        .await
        .map_err(
            |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "resolve registered git-scoped sessions".to_string(),
                message: error.to_string(),
            },
        )?;
        drop(snapshot);
        let mut results = self
            .search_session_messages_filtered_for_test(
                scope,
                provider,
                project_key,
                query,
                limit.saturating_mul(16).max(limit),
                filters,
            )
            .await?;
        if let Some(scoped_ids) = scoped_ids {
            results.retain(|result| {
                scoped_ids.iter().any(|(candidate_provider, session_id)| {
                    (candidate_provider.is_empty()
                        || candidate_provider == &result.session.provider)
                        && session_id == &result.session.session_id
                })
            });
        }
        results.truncate(limit);
        Ok(results)
    }

    #[doc(hidden)]
    pub async fn record_session_span_for_test(
        &self,
        scope: HostAdmissionScope,
        observation: &tracedecay_sessions::runtime::git_correlation::SpanObservation,
        merge_gap_secs: i64,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        crate::store::GlobalDbGitCorrelationStore::new(self.session_database_for_test(scope)?)
            .record_span_observation(observation, merge_gap_secs)
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "record registered session span".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn set_session_message_projection_failure_for_test(
        &self,
        scope: HostAdmissionScope,
        enabled: bool,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let writer = self.session_database_for_test(scope)?.writer_connection()?;
        let sql = if enabled {
            "CREATE TRIGGER fail_session_message_projection
             BEFORE INSERT ON session_messages
             BEGIN
                SELECT RAISE(ABORT, 'projection failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS fail_session_message_projection;"
        };
        writer.execute_batch(sql).await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "set registered session projection failure fixture".to_string(),
                message: error.to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub async fn delete_session_message_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<u64> {
        let transaction = self
            .session_database_for_test(scope)?
            .begin_write_transaction()
            .await?;
        let deleted = transaction
            .execute(
                "DELETE FROM session_messages WHERE provider = ?1 AND message_id = ?2",
                tracedecay_runtime_core::db::engine::params![provider, message_id],
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "delete registered session message fixture".to_string(),
                    message: error.to_string(),
                },
            )?;
        transaction.commit().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "commit registered session message fixture deletion".to_string(),
                message: error.to_string(),
            }
        })?;
        Ok(deleted)
    }

    #[doc(hidden)]
    pub async fn upsert_transcript_batch_for_test(
        &self,
        scope: HostAdmissionScope,
        session: &tracedecay_sessions::runtime::SessionRecord,
        messages: &[tracedecay_sessions::runtime::SessionMessageRecord],
        source: &str,
        offset: tracedecay_global_db::ParseOffset,
    ) -> tracedecay_runtime_core::errors::Result<Vec<i64>> {
        let database = self.session_database_for_test(scope)?;
        if !database
            .upsert_transcript_batch(session, messages, source, offset)
            .await
        {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "seed registered transcript batch fixture".to_string(),
                message: "registered transcript batch write failed".to_string(),
            });
        }
        let mut store_ids = Vec::with_capacity(messages.len());
        for message in messages {
            let raw = database
                .lcm_load_raw_message(&message.provider, &message.message_id)
                .await
                .ok_or_else(
                    || tracedecay_runtime_core::errors::TraceDecayError::Database {
                        operation: "read registered transcript fixture store id".to_string(),
                        message: format!(
                            "LCM raw message {}/{} is unavailable after insert",
                            message.provider, message.message_id
                        ),
                    },
                )?;
            store_ids.push(raw.store_id);
        }
        Ok(store_ids)
    }

    #[doc(hidden)]
    pub async fn lcm_ingest_raw_message_for_test(
        &self,
        scope: HostAdmissionScope,
        message: &tracedecay_sessions::runtime::SessionMessageRecord,
    ) -> Result<(), tracedecay_sessions::runtime::lcm::LcmError> {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let storage_root = database.db_path().parent().ok_or_else(|| {
            tracedecay_sessions::runtime::lcm::LcmError::Db(
                "registered session database has no storage root".to_string(),
            )
        })?;
        database.lcm_ingest_raw_message(storage_root, message).await
    }

    #[doc(hidden)]
    pub async fn lcm_raw_fts_match_count_for_test(
        &self,
        scope: HostAdmissionScope,
        query: &str,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open registered lcm fts count snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*) FROM lcm_raw_messages_fts
                 WHERE lcm_raw_messages_fts MATCH ?1",
                [query],
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query registered lcm fts count".to_string(),
                    message: error.to_string(),
                },
            )?;
        let row = rows
            .next()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read registered lcm fts count".to_string(),
                    message: error.to_string(),
                },
            )?
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read registered lcm fts count".to_string(),
                    message: "count query returned no row".to_string(),
                },
            )?;
        row.get::<i64>(0).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "decode registered lcm fts count".to_string(),
                message: error.to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub async fn lcm_raw_store_id_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<Option<i64>> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open registered lcm raw store id snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(
                "SELECT store_id FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2",
                tracedecay_runtime_core::db::engine::params![provider, message_id],
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query registered lcm raw store id".to_string(),
                    message: error.to_string(),
                },
            )?;
        let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read registered lcm raw store id".to_string(),
                message: error.to_string(),
            }
        })?
        else {
            return Ok(None);
        };
        row.get::<i64>(0).map(Some).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "decode registered lcm raw store id".to_string(),
                message: error.to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub async fn lcm_insert_summary_node_for_test(
        &self,
        scope: HostAdmissionScope,
        draft: tracedecay_sessions::runtime::lcm::LcmSummaryNodeDraft,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmSummaryNode,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let transaction = database
            .begin_write_transaction()
            .await
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let publisher =
            tracedecay_global_db::session_temporal_operations::GlobalDbLcmSummaryPublication::new(
                &transaction,
            );
        let summary =
            tracedecay_sessions::runtime::lcm::dag::insert_summary_node(&publisher, draft).await?;
        transaction.commit().await?;
        Ok(summary)
    }

    #[doc(hidden)]
    pub async fn seed_lcm_render_fixture_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let database = self.session_database_for_test(scope)?;
        let session = tracedecay_sessions::runtime::SessionRecord {
            provider: "codex".to_string(),
            session_id: "session-a".to_string(),
            project_key: "project-a".to_string(),
            project_path: "/project-a".to_string(),
            title: Some("Canonical render fixture".to_string()),
            started_at: Some(10),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };
        if !database.upsert_session(&session).await {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "seed canonical lcm render session".to_string(),
                message: "session upsert failed".to_string(),
            });
        }

        let external_content = "canonical external payload";
        let external_hash =
            tracedecay_sessions::runtime::lcm::util::sha256_hex(external_content.as_bytes());
        let payload_dir = database
            .db_path()
            .parent()
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "seed canonical lcm render payload".to_string(),
                    message: "registered session database has no storage root".to_string(),
                },
            )?
            .join("lcm-payloads");
        std::fs::create_dir_all(&payload_dir).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "create canonical lcm render payload directory".to_string(),
                message: error.to_string(),
            }
        })?;
        std::fs::write(payload_dir.join("payload-a"), external_content).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "write canonical lcm render payload".to_string(),
                message: error.to_string(),
            }
        })?;

        database
            .writer_connection()?
            .execute_batch(&format!(
                "INSERT INTO lcm_external_payloads(
                    payload_ref, provider, session_id, message_id, kind, content_hash,
                    byte_count, char_count, created_at, metadata_json
                 ) VALUES (
                    'payload-a', 'codex', 'session-a', 'message-b', 'tool_result',
                    '{external_hash}', {byte_count}, {char_count}, 12, NULL
                 );
                 INSERT INTO lcm_raw_messages(
                    provider, message_id, session_id, store_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref, snippet_text,
                    index_text, legacy_source, legacy_truncated, metadata_json
                 ) VALUES (
                    'codex', 'message-a', 'session-a', 11, 'assistant', 0, 11,
                    'canonical raw message', 'raw-hash-a', 'inline', NULL,
                    'canonical raw message', 'canonical raw message', 0, 0, NULL
                 );
                 INSERT INTO lcm_raw_messages(
                    provider, message_id, session_id, store_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref, snippet_text,
                    index_text, legacy_source, legacy_truncated, metadata_json
                 ) VALUES (
                    'codex', 'message-b', 'session-a', 12, 'tool', 1, 12,
                    NULL, '{external_hash}', 'external', 'payload-a',
                    'canonical external payload', 'canonical external payload', 0, 0, NULL
                 );
                 INSERT INTO lcm_summary_nodes(
                    node_id, provider, conversation_id, session_id, depth, summary_text,
                    summary_hash, summary_token_count, source_token_count,
                    source_time_start, source_time_end, expand_hint, metadata_json, created_at
                 ) VALUES (
                    'summary-child', 'codex', 'session-a', 'session-a', 0,
                    'canonical child summary', 'summary-child-hash', 3, 3,
                    11, 11, NULL, NULL, 13
                 );
                 INSERT INTO lcm_summary_nodes(
                    node_id, provider, conversation_id, session_id, depth, summary_text,
                    summary_hash, summary_token_count, source_token_count,
                    source_time_start, source_time_end, expand_hint, metadata_json, created_at
                 ) VALUES (
                    'summary-parent', 'codex', 'session-a', 'session-a', 1,
                    'canonical parent summary', 'summary-parent-hash', 3, 6,
                    11, 12, NULL, NULL, 14
                 );
                 INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                 VALUES ('summary-child', 'raw_message', '11', 0);
                 INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                 VALUES ('summary-parent', 'summary_node', 'summary-child', 0);
                 INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                 VALUES ('summary-parent', 'raw_message', '12', 1);",
                byte_count = external_content.len(),
                char_count = external_content.chars().count(),
            ))
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "seed canonical lcm render rows".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn lcm_update_lifecycle_for_test(
        &self,
        scope: HostAdmissionScope,
        update: tracedecay_sessions::runtime::lcm::LcmLifecycleUpdate,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmLifecycleState,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let transaction = database
            .begin_write_transaction()
            .await
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let state =
            tracedecay_sessions::runtime::lcm::compression::update_lifecycle(&transaction, update)
                .await?;
        transaction.commit().await?;
        Ok(state)
    }

    #[doc(hidden)]
    pub async fn lcm_publish_immutable_summary_for_test(
        &self,
        scope: HostAdmissionScope,
        publication: tracedecay_sessions::runtime::lcm::types::LcmImmutableSummaryPublication,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::types::LcmSummaryPublicationReceipt,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let transaction = database
            .begin_write_transaction()
            .await
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let receipt = tracedecay_global_db::session_temporal_operations::publish_immutable_summary(
            &transaction,
            publication,
        )
        .await?;
        transaction.commit().await?;
        Ok(receipt)
    }

    #[doc(hidden)]
    pub async fn apply_lcm_lineage_fault_for_test(
        &self,
        fault: LcmLineageFaultForTest,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let connection = self
            .primary_session_database_for_test()
            .writer_connection()?;
        let result = match fault {
            LcmLineageFaultForTest::CorruptCompatibilitySummaryText { node_id, text } => {
                connection
                    .execute(
                        "UPDATE lcm_summary_nodes SET summary_text = ?2 WHERE node_id = ?1",
                        tracedecay_runtime_core::db::engine::params![node_id, text],
                    )
                    .await
            }
            LcmLineageFaultForTest::ShiftRawMessageTimestamp { store_id, delta } => {
                connection
                    .execute(
                        "UPDATE lcm_raw_messages
                         SET timestamp = timestamp + ?2
                         WHERE store_id = ?1",
                        tracedecay_runtime_core::db::engine::params![store_id, delta],
                    )
                    .await
            }
            LcmLineageFaultForTest::DeleteGeneration {
                session_id,
                generation,
            } => {
                connection
                    .execute_batch(
                        "DROP TRIGGER IF EXISTS session_temporal_generations_delete_guard_v1;
                         PRAGMA foreign_keys = OFF;",
                    )
                    .await
                    .map_err(|error| {
                        tracedecay_runtime_core::errors::TraceDecayError::Database {
                            operation: "prepare missing lcm generation fixture".to_string(),
                            message: error.to_string(),
                        }
                    })?;
                connection
                    .execute(
                        "DELETE FROM session_temporal_generations
                         WHERE session_id = ?1 AND generation = ?2",
                        tracedecay_runtime_core::db::engine::params![session_id, generation],
                    )
                    .await
            }
            LcmLineageFaultForTest::ReplaceGenerationWatermarks {
                session_id,
                generation,
                json,
            } => {
                connection
                    .execute(
                        "DROP TRIGGER IF EXISTS session_temporal_generations_state_guard_v1",
                        (),
                    )
                    .await
                    .map_err(|error| {
                        tracedecay_runtime_core::errors::TraceDecayError::Database {
                            operation: "prepare changed lcm watermarks fixture".to_string(),
                            message: error.to_string(),
                        }
                    })?;
                connection
                    .execute(
                        "UPDATE session_temporal_generations
                         SET frozen_watermarks_json = ?3
                         WHERE session_id = ?1 AND generation = ?2",
                        tracedecay_runtime_core::db::engine::params![session_id, generation, json],
                    )
                    .await
            }
            LcmLineageFaultForTest::DeleteAvailability {
                session_id,
                generation,
                summary_id,
            } => {
                connection
                    .execute(
                        "DELETE FROM session_summary_availability
                         WHERE session_id = ?1 AND generation = ?2 AND summary_id = ?3",
                        tracedecay_runtime_core::db::engine::params![
                            session_id, generation, summary_id
                        ],
                    )
                    .await
            }
            LcmLineageFaultForTest::ReplaceAvailabilityHorizon {
                session_id,
                generation,
                summary_id,
                source_horizon_json,
            } => {
                connection
                    .execute(
                        "UPDATE session_summary_availability
                         SET source_horizon_json = ?4
                         WHERE session_id = ?1 AND generation = ?2 AND summary_id = ?3",
                        tracedecay_runtime_core::db::engine::params![
                            session_id,
                            generation,
                            summary_id,
                            source_horizon_json
                        ],
                    )
                    .await
            }
            LcmLineageFaultForTest::SetAvailability {
                session_id,
                generation,
                summary_id,
                availability,
                reason,
            } => {
                connection
                    .execute(
                        "UPDATE session_summary_availability
                         SET source_horizon_json = (
                                SELECT source_horizon_json FROM session_summary_nodes
                                WHERE summary_id = ?3
                             ),
                             availability = ?4,
                             reason = ?5
                         WHERE session_id = ?1 AND generation = ?2 AND summary_id = ?3",
                        tracedecay_runtime_core::db::engine::params![
                            session_id,
                            generation,
                            summary_id,
                            availability,
                            reason
                        ],
                    )
                    .await
            }
            LcmLineageFaultForTest::SetGenerationFailed {
                session_id,
                generation,
            } => {
                connection
                    .execute(
                        "DROP TRIGGER IF EXISTS session_temporal_generations_state_guard_v1",
                        (),
                    )
                    .await
                    .map_err(|error| {
                        tracedecay_runtime_core::errors::TraceDecayError::Database {
                            operation: "prepare failed lcm generation fixture".to_string(),
                            message: error.to_string(),
                        }
                    })?;
                connection
                    .execute(
                        "UPDATE session_temporal_generations
                         SET state = 'failed',
                             completed_at = COALESCE(completed_at, activated_at, ready_at, created_at)
                         WHERE session_id = ?1 AND generation = ?2",
                        tracedecay_runtime_core::db::engine::params![session_id, generation],
                    )
                    .await
            }
            LcmLineageFaultForTest::CorruptRetrievalAnchorOwner { summary_id } => {
                connection
                    .execute_batch(
                        "DROP TRIGGER IF EXISTS retrieval_anchors_immutable_update;
                         DROP TRIGGER IF EXISTS observation_retrieval_anchors_immutable_update;",
                    )
                    .await
                    .map_err(|error| {
                        tracedecay_runtime_core::errors::TraceDecayError::Database {
                            operation: "prepare corrupt lcm retrieval owner fixture".to_string(),
                            message: error.to_string(),
                        }
                    })?;
                connection
                    .execute(
                        "UPDATE retrieval_anchors
                         SET owner_json =
                               '{\"kind\":\"session\",\"provider\":\"wrong\",\"session_id\":\"wrong\"}',
                             anchor_json = json_set(
                                 anchor_json,
                                 '$.owner',
                                 json(
                                     '{\"kind\":\"session\",\"provider\":\"wrong\",\"session_id\":\"wrong\"}'
                                 )
                             )
                         WHERE anchor_id = (
                             SELECT summary_anchor_id FROM session_summary_nodes
                             WHERE summary_id = ?1
                         )",
                        [summary_id],
                    )
                    .await
            }
            LcmLineageFaultForTest::ReplaceSummarySourceWithSummary {
                summary_id,
                ordinal,
                source_summary_id,
            } => {
                connection
                    .execute(
                        "DROP TRIGGER IF EXISTS session_summary_sources_immutable_update_v1",
                        (),
                    )
                    .await
                    .map_err(|error| {
                        tracedecay_runtime_core::errors::TraceDecayError::Database {
                            operation: "prepare corrupt lcm summary source fixture".to_string(),
                            message: error.to_string(),
                        }
                    })?;
                connection
                    .execute(
                        "UPDATE session_summary_sources
                         SET source_kind = 'summary',
                             source_anchor_id = NULL,
                             source_summary_id = ?3
                         WHERE summary_id = ?1 AND source_ordinal = ?2",
                        tracedecay_runtime_core::db::engine::params![
                            summary_id,
                            ordinal,
                            source_summary_id
                        ],
                    )
                    .await
            }
        };
        result.map(|_| ()).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "apply bounded lcm lineage fault fixture".to_string(),
                message: error.to_string(),
            }
        })
    }

    #[doc(hidden)]
    pub async fn lcm_lineage_counts_for_test(
        &self,
        session_id: Option<&str>,
    ) -> tracedecay_runtime_core::errors::Result<LcmLineageCountsForTest> {
        let snapshot = self
            .primary_session_database_for_test()
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open lcm lineage count snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(
                "SELECT
                    (SELECT COUNT(*) FROM session_temporal_generations
                     WHERE state = 'active' AND (?1 IS NULL OR session_id = ?1)),
                    (SELECT COUNT(*) FROM session_temporal_generations
                     WHERE ?1 IS NULL OR session_id = ?1),
                    (SELECT COUNT(*) FROM session_summary_nodes
                     WHERE ?1 IS NULL OR session_id = ?1),
                    (SELECT COUNT(*) FROM session_summary_sources source
                     JOIN session_summary_nodes node ON node.summary_id = source.summary_id
                     WHERE ?1 IS NULL OR node.session_id = ?1),
                    (SELECT COUNT(*) FROM session_summary_successors successor
                     JOIN session_summary_nodes node
                       ON node.summary_id = successor.successor_summary_id
                     WHERE ?1 IS NULL OR node.session_id = ?1),
                    (SELECT COUNT(*) FROM session_query_cursor_keys)",
                [session_id],
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query lcm lineage counts".to_string(),
                    message: error.to_string(),
                },
            )?;
        let row = rows
            .next()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read lcm lineage counts".to_string(),
                    message: error.to_string(),
                },
            )?
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read lcm lineage counts".to_string(),
                    message: "count query returned no row".to_string(),
                },
            )?;
        let value = |index| {
            row.get::<i64>(index).map_err(|error| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "decode lcm lineage counts".to_string(),
                    message: error.to_string(),
                }
            })
        };
        Ok(LcmLineageCountsForTest {
            active_generations: value(0)?,
            total_generations: value(1)?,
            summary_nodes: value(2)?,
            summary_sources: value(3)?,
            summary_successors: value(4)?,
            cursor_keys: value(5)?,
        })
    }

    #[doc(hidden)]
    pub async fn lcm_status_for_test(
        &self,
        provider: &str,
        session_id: Option<&str>,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmStatus,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_status(provider, session_id)
            .await
    }

    pub async fn lcm_status_deep_for_test(
        &self,
        provider: &str,
        session_id: Option<&str>,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmStatus,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_status_with_options(
                provider,
                session_id,
                true,
                &tracedecay_sessions::runtime::lcm::LcmGcConfig::default(),
            )
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_describe_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmDescribeRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmDescribeResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_describe(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_expand_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmExpandRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmExpandResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_expand(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_expand_summary_node_for_test(
        &self,
        provider: &str,
        session_id: &str,
        node_id: &str,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmSummaryExpansion,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_expand_summary_node(provider, session_id, node_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_expand_query_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmExpandQueryRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmExpandQueryResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_expand_query(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_grep_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmGrepRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmGrepOutcome,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_grep(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_load_session_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmLoadSessionRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmLoadSessionPage,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_load_session(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_recent_sessions_for_test(
        &self,
        provider: Option<&str>,
        limit: usize,
    ) -> Result<
        Vec<tracedecay_sessions::runtime::lcm::LcmRecentSession>,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_recent_sessions(provider, limit)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_session_providers_for_test(
        &self,
        session_id: &str,
    ) -> Result<Vec<String>, tracedecay_sessions::runtime::lcm::LcmError> {
        self.primary_session_database_for_test()
            .lcm_session_providers(session_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_session_replay_slice_for_test(
        &self,
        request: &tracedecay_sessions::runtime::lcm::LcmSessionReplayRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmSessionReplaySlice,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_session_replay_slice(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_load_raw_message_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<tracedecay_sessions::runtime::lcm::LcmRawMessage> {
        self.primary_session_database_for_test()
            .lcm_load_raw_message(provider, message_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_raw_message_search_fields_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<Option<(String, String)>, tracedecay_sessions::runtime::lcm::LcmError> {
        let snapshot = self
            .primary_session_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let mut rows = snapshot
            .query(
                "SELECT snippet_text, index_text
                 FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2",
                tracedecay_runtime_core::db::engine::params![provider, message_id],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some((row.get(0)?, row.get(1)?)))
    }

    #[doc(hidden)]
    pub async fn lcm_raw_message_fts_count_for_test(
        &self,
        query: &str,
    ) -> Result<i64, tracedecay_sessions::runtime::lcm::LcmError> {
        let snapshot = self
            .primary_session_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*)
                 FROM lcm_raw_messages_fts
                 WHERE lcm_raw_messages_fts MATCH ?1",
                [query],
            )
            .await?;
        let row = rows.next().await?.ok_or_else(|| {
            tracedecay_sessions::runtime::lcm::LcmError::Db("COUNT(*) returned no row".to_owned())
        })?;
        Ok(row.get(0)?)
    }

    #[doc(hidden)]
    pub async fn lcm_raw_message_metadata_json_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<Option<Option<String>>, tracedecay_sessions::runtime::lcm::LcmError> {
        let snapshot = self
            .primary_session_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let mut rows = snapshot
            .query(
                "SELECT metadata_json
                 FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2",
                tracedecay_runtime_core::db::engine::params![provider, message_id],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(row.get(0)?))
    }

    #[doc(hidden)]
    pub async fn lcm_delete_external_payload_for_test(
        &self,
        scope: HostAdmissionScope,
        payload_ref: &str,
        opts: &tracedecay_sessions::runtime::lcm::payload::DeleteOpts,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::payload::DeleteOutcome,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let storage_root = database.db_path().parent().ok_or_else(|| {
            tracedecay_sessions::runtime::lcm::LcmError::Db(
                "registered session database has no storage root".to_string(),
            )
        })?;
        let transaction = database
            .begin_write_transaction()
            .await
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let prepared =
            tracedecay_sessions::runtime::lcm::payload::delete_external_payload_in_transaction(
                &transaction,
                storage_root,
                payload_ref,
                opts,
            )
            .await?;
        transaction.commit().await?;

        let mut outcome = prepared.outcome;
        if prepared.pending_removal_bytes.is_some() {
            let transaction = database.begin_write_transaction().await.map_err(|error| {
                tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string())
            })?;
            let drained =
                tracedecay_sessions::runtime::lcm::gc::drain_pending_payload_delete_in_transaction(
                    &transaction,
                    storage_root,
                    payload_ref,
                )
                .await;
            match drained {
                Ok(removed) => {
                    transaction.commit().await?;
                    outcome.file_removed = removed.is_some();
                    outcome.bytes_freed = removed.unwrap_or_default();
                }
                Err(error) => {
                    let _ = transaction.rollback().await;
                    tracing::warn!(
                        payload_ref,
                        %error,
                        "payload metadata deletion committed; deferred payload file removal remains pending"
                    );
                }
            }
        }
        Ok(outcome)
    }

    #[doc(hidden)]
    pub async fn lcm_external_payload_manifest_for_test(
        &self,
        payload_ref: &str,
    ) -> Result<
        Option<LcmExternalPayloadManifestTestRecord>,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        let snapshot = self
            .primary_session_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let mut rows = snapshot
            .query(
                "SELECT manifest.payload_ref, manifest.session_id,
                        manifest.payload_digest, manifest.manifest_json,
                        receipt.receipt_id, manifest.created_at, external.created_at
                 FROM session_external_payload_manifests manifest
                 JOIN sanitization_receipts receipt
                   ON receipt.receipt_id = manifest.receipt_id
                 JOIN lcm_external_payloads external
                   ON external.payload_ref = manifest.payload_ref
                 WHERE manifest.payload_ref = ?1",
                [payload_ref],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(LcmExternalPayloadManifestTestRecord {
            payload_ref: row.get(0)?,
            session_id: row.get(1)?,
            payload_digest: row.get(2)?,
            manifest_json: row.get(3)?,
            receipt_id: row.get(4)?,
            created_at: row.get(5)?,
            external_created_at: row.get(6)?,
        }))
    }

    #[doc(hidden)]
    pub async fn lcm_summary_publication_receipt_id_for_test(
        &self,
        summary_id: &str,
    ) -> Result<Option<String>, tracedecay_sessions::runtime::lcm::LcmError> {
        let snapshot = self
            .primary_session_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let mut rows = snapshot
            .query(
                "SELECT json_extract(publication_json, '$.receipt_id')
                 FROM session_summary_nodes
                 WHERE summary_id = ?1",
                [summary_id],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(row.get(0)?)
    }

    #[doc(hidden)]
    pub async fn replace_lcm_external_payload_manifest_for_test(
        &self,
        payload_ref: &str,
        replacement: &LcmExternalPayloadManifestTestRecord,
    ) -> Result<(), tracedecay_sessions::runtime::lcm::LcmError> {
        let transaction = self
            .primary_session_database_for_test()
            .begin_write_transaction()
            .await
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        transaction
            .execute_batch("DROP TRIGGER session_external_payload_manifests_immutable_update_v1;")
            .await?;
        transaction
            .execute(
                "UPDATE session_external_payload_manifests
                 SET session_id = ?2, payload_digest = ?3, manifest_json = ?4,
                     receipt_id = ?5, created_at = ?6
                 WHERE payload_ref = ?1",
                tracedecay_runtime_core::db::engine::params![
                    payload_ref,
                    replacement.session_id.as_str(),
                    replacement.payload_digest.as_str(),
                    replacement.manifest_json.as_str(),
                    replacement.receipt_id.as_str(),
                    replacement.created_at,
                ],
            )
            .await?;
        transaction
            .execute_batch(
                "CREATE TRIGGER session_external_payload_manifests_immutable_update_v1
                 BEFORE UPDATE ON session_external_payload_manifests BEGIN
                     SELECT RAISE(ABORT, 'session external payload manifests are immutable');
                 END;",
            )
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    #[doc(hidden)]
    pub async fn lcm_doctor_for_test(
        &self,
        provider: &str,
        session_id: Option<&str>,
        mode: &str,
        apply: bool,
        clean_config: tracedecay_sessions::runtime::lcm::LcmCleanConfig,
        gc_config: tracedecay_sessions::runtime::lcm::LcmGcConfig,
    ) -> Result<serde_json::Value, tracedecay_sessions::runtime::lcm::LcmError> {
        self.primary_session_database_for_test()
            .lcm_doctor(provider, session_id, mode, apply, clean_config, gc_config)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_session_boundary_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmSessionBoundaryRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmSessionBoundaryResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_session_boundary(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_lifecycle_state_for_test(
        &self,
        provider: &str,
        conversation_id: &str,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmLifecycleState,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        let snapshot = self
            .primary_session_database_for_test()
            .read_snapshot()
            .await?;
        tracedecay_sessions::runtime::lcm::compression::lifecycle_state(
            &snapshot,
            provider,
            conversation_id,
        )
        .await
    }

    #[doc(hidden)]
    pub async fn lcm_preflight_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmPreflightRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmPreflightResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_preflight(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_compress_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmCompressionRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmCompressionResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .lcm_compress(request)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_run_payload_gc_apply_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        session_id: Option<&str>,
        config: &tracedecay_sessions::runtime::lcm::LcmGcConfig,
        now: i64,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmGcReport,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| tracedecay_sessions::runtime::lcm::LcmError::Db(error.to_string()))?;
        let storage_root = database.db_path().parent().ok_or_else(|| {
            tracedecay_sessions::runtime::lcm::LcmError::Db(
                "registered session database has no storage root".to_string(),
            )
        })?;
        database
            .lcm_run_payload_gc_apply(storage_root, provider, session_id, config, now)
            .await
    }

    #[doc(hidden)]
    pub async fn pending_codex_compaction_summary_requests_for_test(
        &self,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<
        Vec<tracedecay_global_db::PendingCodexCompactionSummary>,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .pending_codex_compaction_summary_requests(session_id, limit)
            .await
    }

    #[doc(hidden)]
    pub async fn publish_codex_compaction_summary_successor_for_test(
        &self,
        node_id: &str,
        summary_text: &str,
        route: &str,
        model: Option<&str>,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmSummaryNode,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.primary_session_database_for_test()
            .publish_codex_compaction_summary_successor(node_id, summary_text, route, model)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_summary_successor_edges_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<Vec<(String, String)>> {
        let snapshot = self
            .primary_session_database_for_test()
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open registered LCM successor-edge snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(
                "SELECT predecessor_summary_id, successor_summary_id
                 FROM session_summary_successors
                 ORDER BY predecessor_summary_id, successor_summary_id",
                (),
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query registered LCM successor edges".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut edges = Vec::new();
        while let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read registered LCM successor edge".to_string(),
                message: error.to_string(),
            }
        })? {
            let predecessor = row.get::<String>(0).map_err(|error| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "decode registered LCM predecessor".to_string(),
                    message: error.to_string(),
                }
            })?;
            let successor = row.get::<String>(1).map_err(|error| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "decode registered LCM successor".to_string(),
                    message: error.to_string(),
                }
            })?;
            edges.push((predecessor, successor));
        }
        Ok(edges)
    }

    #[doc(hidden)]
    pub async fn lcm_active_summary_availability_for_test(
        &self,
        session_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<Vec<(String, String)>> {
        let snapshot = self
            .primary_session_database_for_test()
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open registered LCM availability snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(
                "SELECT availability.summary_id, availability.availability
                 FROM session_summary_availability AS availability
                 JOIN session_temporal_generations AS generation
                   ON generation.session_id = availability.session_id
                  AND generation.generation = availability.generation
                 WHERE availability.session_id = ?1
                   AND generation.state = 'active'
                 ORDER BY availability.summary_id",
                tracedecay_runtime_core::db::engine::params![session_id],
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query registered LCM active summary availability".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut labels = Vec::new();
        while let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read registered LCM active summary availability".to_string(),
                message: error.to_string(),
            }
        })? {
            labels.push((
                row.get(0).map_err(|error| {
                    tracedecay_runtime_core::errors::TraceDecayError::Database {
                        operation: "decode registered LCM availability summary".to_string(),
                        message: error.to_string(),
                    }
                })?,
                row.get(1).map_err(|error| {
                    tracedecay_runtime_core::errors::TraceDecayError::Database {
                        operation: "decode registered LCM availability label".to_string(),
                        message: error.to_string(),
                    }
                })?,
            ));
        }
        Ok(labels)
    }

    #[doc(hidden)]
    pub async fn set_lcm_raw_message_metadata_for_test(
        &self,
        store_id: i64,
        metadata_json: Option<&str>,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.primary_session_database_for_test()
            .writer_connection()?
            .execute(
                "UPDATE lcm_raw_messages
                 SET metadata_json = ?1
                 WHERE store_id = ?2",
                tracedecay_runtime_core::db::engine::params![metadata_json, store_id],
            )
            .await
            .map(|_| ())
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "set registered LCM raw message metadata".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn insert_lcm_poison_summary_for_test(
        &self,
        poison_node_id: &str,
        predecessor_node_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.primary_session_database_for_test()
            .writer_connection()?
            .execute(
                "INSERT INTO lcm_summary_nodes (
                    node_id, provider, conversation_id, session_id, depth,
                    summary_text, summary_hash, summary_token_count, source_token_count,
                    source_time_start, source_time_end, expand_hint, metadata_json, created_at
                 )
                 SELECT ?1, provider, conversation_id, session_id, depth + 1000,
                        'unpublishable poison', 'poison-hash', summary_token_count,
                        source_token_count, source_time_start, source_time_end,
                        expand_hint, metadata_json, created_at + 1000000
                 FROM lcm_summary_nodes
                 WHERE node_id = ?2",
                tracedecay_runtime_core::db::engine::params![poison_node_id, predecessor_node_id],
            )
            .await
            .map(|_| ())
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "insert registered LCM poison summary".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn install_lcm_summary_insert_abort_trigger_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.set_lcm_summary_insert_abort_trigger_for_test(true)
            .await
    }

    #[doc(hidden)]
    pub async fn remove_lcm_summary_insert_abort_trigger_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.set_lcm_summary_insert_abort_trigger_for_test(false)
            .await
    }

    async fn set_lcm_summary_insert_abort_trigger_for_test(
        &self,
        enabled: bool,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let statement = if enabled {
            "CREATE TRIGGER fail_codex_summary_successor
             BEFORE INSERT ON lcm_summary_nodes
             BEGIN
                SELECT RAISE(ABORT, 'forced summary successor failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS fail_codex_summary_successor;"
        };
        self.primary_session_database_for_test()
            .writer_connection()?
            .execute_batch(statement)
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "configure registered LCM summary failure".to_string(),
                    message: error.to_string(),
                },
            )
    }

    /// Drives one transcript source through the retained ProjectSessions mount.
    #[doc(hidden)]
    pub async fn ingest_project_transcript_source_for_test(
        &self,
        source: &dyn tracedecay_sessions::runtime::source::TranscriptSource,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> tracedecay_sessions::runtime::source::TranscriptIngestResult<
        tracedecay_sessions::runtime::shared::TranscriptIngestStats,
    > {
        let database = self.project_database_for_test().map_err(|error| {
            tracedecay_sessions::runtime::source::TranscriptIngestError::ScanIo {
                operation: "bind registered project session test runtime",
                path: project_root.to_path_buf(),
                source: std::io::Error::other(error.to_string()),
            }
        })?;
        let store = crate::store::GlobalDbTranscriptStore::new(database);
        tracedecay_sessions::runtime::source::try_ingest_source(
            &store,
            source,
            project_root,
            max_new_bytes,
        )
        .await
    }

    /// Runs one selected provider through the exact registered project authority.
    #[doc(hidden)]
    pub async fn ingest_project_provider_for_test(
        &self,
        project_root: &Path,
        provider: Option<tracedecay_sessions::runtime::SessionProvider>,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_sessions::runtime::shared::TranscriptIngestStats,
    > {
        let project_id = self.project_id.as_ref().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "ingest registered project provider test fixture".to_string(),
                message: "registered project identity is unavailable".to_string(),
            }
        })?;
        let database = self.project_database_for_test()?;
        Ok(
            tracedecay_sessions::runtime::ingest_project_sources_for_provider(
                &self.brain_id,
                &self.profile_id,
                database,
                project_root,
                Some(project_id.clone()),
                provider,
                true,
            )
            .await
            .stats,
        )
    }

    #[doc(hidden)]
    pub async fn project_session_message_count_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        self.project_database_for_test()?
            .session_message_count()
            .await
            .map_err(
                |message| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "count registered project session messages".to_string(),
                    message,
                },
            )
    }

    #[doc(hidden)]
    pub async fn project_parse_offset_for_test(
        &self,
        path: &str,
    ) -> tracedecay_runtime_core::errors::Result<Option<tracedecay_global_db::ParseOffset>> {
        Ok(self
            .project_database_for_test()?
            .get_parse_offset(path)
            .await)
    }

    /// Bind the registered PR17 workflow/handoff authority for the mounted
    /// project. Every call reopens through the live registered database, so
    /// dropping and re-admitting the runtime at the same profile root
    /// exercises the same durable restart journey as the rest of Work.
    #[doc(hidden)]
    pub fn project_workflow_storage_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    > {
        self.project_database_for_test()?.workflow_storage()
    }

    #[doc(hidden)]
    pub async fn project_session_for_test(
        &self,
        provider: &str,
        session_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<Option<tracedecay_sessions::runtime::SessionRecord>>
    {
        Ok(self
            .project_database_for_test()?
            .get_session(provider, session_id)
            .await)
    }

    #[doc(hidden)]
    pub async fn project_session_message_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<
        Option<tracedecay_sessions::runtime::SessionMessageRecord>,
    > {
        Ok(self
            .project_database_for_test()?
            .get_session_message(provider, message_id)
            .await)
    }

    #[doc(hidden)]
    pub async fn search_project_session_messages_for_test(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> tracedecay_runtime_core::errors::Result<
        Vec<tracedecay_sessions::runtime::SessionMessageSearchResult>,
    > {
        Ok(self
            .project_database_for_test()?
            .search_session_messages(provider, project_key, query, limit)
            .await)
    }

    #[doc(hidden)]
    pub async fn project_git_sessions_for_test(
        &self,
        query: &tracedecay_sessions::runtime::git_correlation::SessionsForQuery,
    ) -> tracedecay_runtime_core::errors::Result<
        Vec<tracedecay_sessions::runtime::git_correlation::SessionGitCorrelationHit>,
    > {
        let snapshot = self
            .project_database_for_test()?
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open registered project git-session snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        tracedecay_sessions::runtime::git_correlation::sessions_for(&snapshot, query)
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query registered project git sessions".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn recent_project_session_goals_for_test(
        &self,
        project_key: &str,
        limit: usize,
    ) -> tracedecay_runtime_core::errors::Result<
        Vec<tracedecay_sessions::runtime::SessionMessageSearchResult>,
    > {
        Ok(self
            .project_database_for_test()?
            .recent_session_goals(Some(project_key), limit)
            .await)
    }

    #[doc(hidden)]
    pub async fn run_git_backfill_for_test(
        &self,
        analytics_events: &[tracedecay_global_db::AnalyticsEventRecord],
        git: &dyn tracedecay_sessions::runtime::git_correlation::GitReflogSource,
        options: &tracedecay_sessions::runtime::git_correlation::BackfillOptions,
    ) -> Result<
        tracedecay_sessions::runtime::git_correlation::BackfillStats,
        tracedecay_sessions::runtime::git_correlation::GitCorrelationError,
    > {
        let database = self.project_database_for_test().map_err(|error| {
            tracedecay_sessions::runtime::git_correlation::GitCorrelationError::Db(
                error.to_string(),
            )
        })?;
        crate::store::GlobalDbGitCorrelationStore::new(database)
            .run_backfill(analytics_events, git, options)
            .await
    }

    #[doc(hidden)]
    pub async fn run_incremental_git_backfill_for_test(
        &self,
        git: &dyn tracedecay_sessions::runtime::git_correlation::GitReflogSource,
        limit_sessions: usize,
    ) -> Result<
        tracedecay_sessions::runtime::git_correlation::BackfillStats,
        tracedecay_sessions::runtime::git_correlation::GitCorrelationError,
    > {
        let database = self.project_database_for_test().map_err(|error| {
            tracedecay_sessions::runtime::git_correlation::GitCorrelationError::Db(
                error.to_string(),
            )
        })?;
        crate::store::GlobalDbGitCorrelationStore::new(database)
            .run_incremental_backfill(git, limit_sessions)
            .await
    }

    #[doc(hidden)]
    pub async fn git_correlation_meta_for_test(
        &self,
        key: &str,
    ) -> Result<Option<i64>, tracedecay_sessions::runtime::git_correlation::GitCorrelationError>
    {
        let snapshot = self
            .project_database_for_test()
            .map_err(|error| {
                tracedecay_sessions::runtime::git_correlation::GitCorrelationError::Db(
                    error.to_string(),
                )
            })?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT value FROM git_correlation_meta WHERE key = ?1",
                tracedecay_runtime_core::db::engine::params![key],
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    #[doc(hidden)]
    pub async fn git_sessions_for_for_test(
        &self,
        query: &tracedecay_sessions::runtime::git_correlation::SessionsForQuery,
        relation: tracedecay_sessions::runtime::git_correlation::CommitRelationFilter,
    ) -> Result<
        Vec<tracedecay_sessions::runtime::git_correlation::SessionGitCorrelationHit>,
        tracedecay_sessions::runtime::git_correlation::GitCorrelationError,
    > {
        let database = self.project_database_for_test().map_err(|error| {
            tracedecay_sessions::runtime::git_correlation::GitCorrelationError::Db(
                error.to_string(),
            )
        })?;
        crate::store::GlobalDbGitCorrelationStore::new(database)
            .sessions_for_with_relation(query, relation)
            .await
    }

    #[doc(hidden)]
    pub async fn project_workflow_fact_rows_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<Vec<(String, Option<String>, Option<String>)>>
    {
        self.project_database_for_test()?.workflow_fact_rows().await
    }

    #[doc(hidden)]
    pub async fn project_lcm_raw_message_exists_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<bool> {
        Ok(self
            .project_database_for_test()?
            .lcm_load_raw_message(provider, message_id)
            .await
            .is_some())
    }

    #[doc(hidden)]
    pub async fn project_lcm_raw_message_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<
        Option<tracedecay_sessions::runtime::lcm::LcmRawMessage>,
    > {
        Ok(self
            .project_database_for_test()?
            .lcm_load_raw_message(provider, message_id)
            .await)
    }

    #[doc(hidden)]
    pub async fn project_parse_offset_by_suffix_for_test(
        &self,
        suffix: &str,
    ) -> tracedecay_runtime_core::errors::Result<Option<tracedecay_global_db::ParseOffset>> {
        let snapshot = self
            .project_database_for_test()?
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open registered project parse-offset snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(
                "SELECT byte_offset, mtime, file_id
                 FROM parse_offsets
                 WHERE file_path LIKE '%' || ?1
                 ORDER BY file_path
                 LIMIT 1",
                tracedecay_runtime_core::db::engine::params![suffix],
            )
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "query registered project parse offset by suffix".to_string(),
                    message: error.to_string(),
                },
            )?;
        let Some(row) = rows.next().await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read registered project parse offset by suffix".to_string(),
                message: error.to_string(),
            }
        })?
        else {
            return Ok(None);
        };
        let decode = |index| {
            row.get::<i64>(index)
                .map(|value| u64::try_from(value).unwrap_or_default())
                .map_err(
                    |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                        operation: "decode registered project parse offset by suffix".to_string(),
                        message: error.to_string(),
                    },
                )
        };
        Ok(Some(tracedecay_global_db::ParseOffset {
            byte_offset: decode(0)?,
            mtime: decode(1)?,
            file_id: decode(2)?,
        }))
    }

    /// Counts only the bounded durable observation tables used by restart tests.
    #[doc(hidden)]
    pub async fn project_observation_table_count_for_test(
        &self,
        table: &str,
    ) -> tracedecay_runtime_core::errors::Result<u64> {
        if !matches!(
            table,
            "observations"
                | "sanitization_receipts"
                | "projection_queue"
                | "observation_workflow_facts"
        ) {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "count registered project observation table".to_string(),
                message: format!("unsupported test table {table}"),
            });
        }
        let snapshot = self
            .project_database_for_test()?
            .read_snapshot()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "open registered project observation count snapshot".to_string(),
                    message: error.to_string(),
                },
            )?;
        let mut rows = snapshot
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "count registered project observation table".to_string(),
                    message: error.to_string(),
                },
            )?;
        let row = rows
            .next()
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read registered project observation table count".to_string(),
                    message: error.to_string(),
                },
            )?
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "read registered project observation table count".to_string(),
                    message: "count query returned no row".to_string(),
                },
            )?;
        row.get::<i64>(0)
            .map(|count| u64::try_from(count).unwrap_or_default())
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "decode registered project observation table count".to_string(),
                    message: error.to_string(),
                },
            )
    }

    /// Installs or removes the deterministic projection-failure trigger in-place.
    #[doc(hidden)]
    pub async fn set_project_projection_failure_for_test(
        &self,
        enabled: bool,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let statement = if enabled {
            "CREATE TRIGGER fail_session_message_projection
             BEFORE INSERT ON session_messages
             BEGIN
                SELECT RAISE(ABORT, 'projection failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS fail_session_message_projection;
             DROP TRIGGER IF EXISTS fail_claude_suffix_projection;"
        };
        self.project_database_for_test()?
            .writer_connection()?
            .execute_batch(statement)
            .await
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "configure registered project projection failure".to_string(),
                    message: error.to_string(),
                },
            )
    }

    #[doc(hidden)]
    pub async fn project_observation_source_cursor_for_test(
        &self,
        source: &ObservationSourceIdentityV1,
    ) -> Result<Option<ObservationSourceCursorV1>, HostAdmissionOutcome> {
        let project_id = self
            .project_id
            .as_ref()
            .ok_or_else(HostAdmissionOutcome::project_authority_unbound)?;
        self.facade()
            .get_source_cursor(
                source,
                &ObservationScopeV1::Project {
                    project_id: project_id.clone(),
                },
            )
            .await
    }

    /// Replay through the same registered observation store used by host
    /// admission without exposing its database or authority handles.
    pub async fn replay_observations(
        &self,
        scope: HostAdmissionScope,
        request: ObservationReplayRequest,
    ) -> ObservationStoreResult<Vec<StoredObservation>> {
        let facade = self.facade();
        let store =
            facade
                .observation_store(scope)
                .map_err(|outcome| ObservationStoreError::Storage {
                    operation: "bind registered host admission replay",
                    source: Box::new(std::io::Error::other(
                        outcome
                            .reason_code
                            .unwrap_or("registered_authority_unavailable"),
                    )),
                })?;
        store.replay_observations(request).await
    }

    #[doc(hidden)]
    pub async fn external_source_receipt_for_test(
        &self,
        scope: HostAdmissionScope,
        observation: &tracedecay_store::ObservationCommitReceipt,
    ) -> Result<Option<tracedecay_store::SourceCommitReceiptV1>, HostAdmissionOutcome> {
        let database = match scope {
            HostAdmissionScope::Project => self.project_registered.as_deref(),
            HostAdmissionScope::Profile => Some(self.profile_registered.as_ref()),
        }
        .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        database
            .external_source_store()
            .map_err(|_| HostAdmissionOutcome::registered_authority_unavailable())?
            .read_host_observation_receipt(observation)
            .await
            .map_err(|_| HostAdmissionOutcome::retained_unavailable("external_source_read_failed"))
    }
}

#[cfg(unix)]
fn prepare_host_admission_test_profile_root(
    profile_root: &Path,
) -> tracedecay_runtime_core::errors::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(profile_root).map_err(|error| {
        tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: format!(
                "failed to create host-admission test profile '{}': {error}",
                profile_root.display()
            ),
        }
    })?;
    let metadata = std::fs::symlink_metadata(profile_root).map_err(|error| {
        tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: format!(
                "failed to inspect host-admission test profile '{}': {error}",
                profile_root.display()
            ),
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: format!(
                "host-admission test profile '{}' must be a regular directory",
                profile_root.display()
            ),
        });
    }
    std::fs::set_permissions(profile_root, std::fs::Permissions::from_mode(0o700)).map_err(
        |error| tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: format!(
                "failed to restrict host-admission test profile '{}': {error}",
                profile_root.display()
            ),
        },
    )
}

#[cfg(not(unix))]
fn prepare_host_admission_test_profile_root(
    profile_root: &Path,
) -> tracedecay_runtime_core::errors::Result<()> {
    std::fs::create_dir_all(profile_root).map_err(|error| {
        tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: format!(
                "failed to create host-admission test profile '{}': {error}",
                profile_root.display()
            ),
        }
    })
}

fn prepare_host_admission_test_project_root(
    project_root: &Path,
    project_id: &ProjectId,
) -> tracedecay_runtime_core::errors::Result<()> {
    std::fs::create_dir_all(project_root).map_err(|error| {
        tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: format!(
                "failed to create host-admission test project '{}': {error}",
                project_root.display()
            ),
        }
    })?;
    if tracedecay_runtime_core::storage::read_enrollment_marker(project_root)?.is_none() {
        tracedecay_runtime_core::storage::write_enrollment_marker(
            project_root,
            &tracedecay_runtime_core::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
            },
        )?;
    }
    Ok(())
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
