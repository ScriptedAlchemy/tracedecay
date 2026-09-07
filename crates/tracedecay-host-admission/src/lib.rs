//! Host-event admission broker, spool, and observation capture.
//!
//! Wire bounds and disposition enums live in `tracedecay-sessions::admission`.
//! This crate owns the durable broker, spool, and the observation/memory
//! capture path, and depends on remaining usecases for those edges.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tracedecay_domain::{
    BrainId, CanonicalObservationIdV1, FactOwnerV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceIdentityV1, ProjectId, RetrievalAnchorId, SanitizationReceiptV1,
    UserProfileId,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationStoreError, ParseOffset, ProjectionStoreError, StoreShardScopeV1,
    build_scope_resolution_authorization_v1,
};

use tracedecay_global_db::GlobalDbObservationStore;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_private_fs::background_cpu::process_background_cpu;
use tracedecay_runtime_core::privacy::{PrivacySanitizerError, RecordSanitizerV1};
use tracedecay_session_memory::anchor_resolution::{
    EvidenceAnchorReportResolver, EvidenceAnchorResolutionReport,
};
use tracedecay_session_memory::memory::{
    EvidenceAnchorResolutionError, EvidenceAnchorResolver, ResolvedEvidenceAnchor,
};
use tracedecay_sessions::admission::{
    HostAdmissionOutcome, HostAdmissionScope, HostAdmissionStatus, HostProjectionDrainOutcome,
};
use tracedecay_sessions::observation::{
    AdvanceNonDurableSourceCursorRequest, CaptureObservationOutcome, CaptureObservationRequest,
    ExternalSourceProjectionRetryHandleV1, ExternalSourceProjectionStateV1, GetObservationRequest,
    ObservationApplication, ObservationApplicationError, ObservationCancellation,
};
use tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext;

mod discovery_queue;
mod hotpath_observe;
mod projection_drain;
mod replay;
mod runtime;
mod schedule;
/// Session-ingest authority composition over one registered database.
pub mod session_ingest_authority;
mod spool;

pub use replay::{
    REPLAY_BACKOFF_SHIFT_CAP, ReplayPassDecision, classify_replay_pass, replay_backoff,
};

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

    #[hotpath::measure(label = "host_admission.with_runtime", future = true)]
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

    #[hotpath::measure(label = "usecases.admission.admit", future = true)]
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

    #[hotpath::measure(label = "usecases.admission.begin_replay", future = true)]
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
    #[hotpath::measure(label = "usecases.admission.replay.lease", future = true)]
    pub async fn lease_next(&self) -> Result<Option<SpoolRecord>, HostAdmissionOutcome> {
        self.broker
            .with_runtime(HostAdmissionRuntime::try_lease_next)
            .await
    }

    #[hotpath::measure(label = "usecases.admission.replay.defer", future = true)]
    pub async fn defer(&self, seq: u64) -> Result<(), HostAdmissionOutcome> {
        self.broker
            .with_runtime(move |runtime| runtime.defer(seq))
            .await
    }

    #[hotpath::measure(label = "usecases.admission.replay.commit", future = true)]
    pub async fn commit(&self, seq: u64) -> Result<usize, HostAdmissionOutcome> {
        self.broker
            .with_runtime(move |runtime| runtime.commit(seq))
            .await
    }

    #[hotpath::measure(label = "usecases.admission.replay.quarantine", future = true)]
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
pub(crate) use spool::{HostAdmissionSpool, SpoolError, SpoolIntegrity};
pub use spool::{SpoolBounds, SpoolOpenReport, SpoolRecord, TerminalReason};

/// Constructs a [`HostAdmissionOutcome`] for usecases-specific reason codes;
/// the canonical constructors live on the sessions type, whose grouped
/// constructor is private.
pub(crate) const fn admission_outcome(
    status: HostAdmissionStatus,
    retryable: bool,
    reason_code: Option<&'static str>,
) -> HostAdmissionOutcome {
    HostAdmissionOutcome {
        status,
        retryable,
        reason_code,
        recovery: None,
        storage_cause: None,
    }
}

/// Durable success whose external-source projection is still queued.
const fn external_source_projection_pending() -> HostAdmissionOutcome {
    admission_outcome(
        HostAdmissionStatus::AcceptedForReplay,
        false,
        Some("external_source_projection_pending"),
    )
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
        Box::pin(HostAdmissionFacade::capture_observation(self, request))
    }

    fn capture_observations<'a>(
        &'a self,
        requests: Vec<CaptureObservationRequest>,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Vec<CaptureObservationOutcome>> {
        Box::pin(HostAdmissionFacade::capture_observations(self, requests))
    }

    fn advance_non_durable_source_cursor<'a>(
        &'a self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, CursorAdvanceOutcome> {
        Box::pin(HostAdmissionFacade::advance_non_durable_source_cursor(
            self,
            advance,
            cancellation,
        ))
    }

    fn get_source_cursor<'a>(
        &'a self,
        source: &'a ObservationSourceIdentityV1,
        scope: &'a ObservationScopeV1,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Option<ObservationSourceCursorV1>>
    {
        Box::pin(HostAdmissionFacade::get_source_cursor(self, source, scope))
    }

    fn observation_receipt<'a>(
        &'a self,
        provider: &'a str,
        scope: &'a ObservationScopeV1,
        observation_id: &'a CanonicalObservationIdV1,
        cancellation: &'a ObservationCancellation,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Option<SanitizationReceiptV1>> {
        Box::pin(async move {
            let application = self.application(provider, scope)?;
            application
                .get_observation(GetObservationRequest::new(
                    observation_id.clone(),
                    cancellation.clone(),
                ))
                .await
                .map(|read| {
                    read.observation()
                        .map(|stored| stored.observation().receipt().clone())
                })
                .map_err(|error| classify_error(&error))
        })
    }

    fn drain_projection_queue<'a>(
        &'a self,
        provider: &'a str,
        scope: &'a ObservationScopeV1,
        cancellation: &'a ObservationCancellation,
        max: usize,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, HostProjectionDrainOutcome> {
        Box::pin(HostAdmissionFacade::drain_projection_queue(
            self,
            provider,
            scope,
            cancellation,
            max,
        ))
    }

    fn has_session_message<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        message_id: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, bool> {
        Box::pin(HostAdmissionFacade::has_session_message(
            self, scope, provider, message_id,
        ))
    }

    fn existing_session_message_ids<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        message_ids: Vec<String>,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Vec<String>> {
        Box::pin(HostAdmissionFacade::existing_session_message_ids(
            self,
            scope,
            provider,
            message_ids,
        ))
    }

    fn read_session_backfill_state<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        key: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Option<String>> {
        Box::pin(HostAdmissionFacade::read_session_backfill_state(
            self, scope, key,
        ))
    }

    fn list_session_backfill_state_page<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        key_prefix: &'a str,
        after_key: Option<&'a str>,
        through_key: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Vec<(String, String)>> {
        Box::pin(HostAdmissionFacade::list_session_backfill_state_page(
            self,
            scope,
            key_prefix,
            after_key,
            through_key,
        ))
    }

    fn session_backfill_state_high_water<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        key_prefix: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Option<String>> {
        Box::pin(HostAdmissionFacade::session_backfill_state_high_water(
            self, scope, key_prefix,
        ))
    }

    fn compare_and_swap_session_backfill_state<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        key: &'a str,
        expected: Option<&'a str>,
        replacement: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, bool> {
        Box::pin(
            HostAdmissionFacade::compare_and_swap_session_backfill_state(
                self,
                scope,
                key,
                expected,
                replacement,
            ),
        )
    }

    fn compare_and_delete_session_backfill_state<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        key: &'a str,
        expected: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, bool> {
        Box::pin(
            HostAdmissionFacade::compare_and_delete_session_backfill_state(
                self, scope, key, expected,
            ),
        )
    }

    fn get_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, Option<ParseOffset>> {
        Box::pin(HostAdmissionFacade::get_parse_offset(self, scope, path))
    }

    fn advance_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
        offset: ParseOffset,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, ()> {
        Box::pin(HostAdmissionFacade::advance_parse_offset(
            self, scope, path, offset,
        ))
    }

    fn replace_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
        expected: ParseOffset,
        next: ParseOffset,
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, ()> {
        Box::pin(HostAdmissionFacade::replace_registered_parse_offset(
            self, scope, path, expected, next,
        ))
    }

    fn replace_parse_offset_pair<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        first: (&'a str, ParseOffset, ParseOffset),
        second: (&'a str, ParseOffset, ParseOffset),
    ) -> tracedecay_sessions::admission::AdmissionFuture<'a, ()> {
        Box::pin(HostAdmissionFacade::replace_registered_parse_offset_pair(
            self, scope, first, second,
        ))
    }

    fn enqueue_discovery_paths<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        paths: Vec<PathBuf>,
    ) -> tracedecay_sessions::admission::AdmissionFuture<
        'a,
        Option<tracedecay_sessions::admission::HostDiscoveryQueueEntry>,
    > {
        Box::pin(HostAdmissionFacade::enqueue_discovery_paths(
            self, scope, provider, paths,
        ))
    }

    fn discovery_paths_after<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        after_sequence: u64,
        limit: usize,
    ) -> tracedecay_sessions::admission::AdmissionFuture<
        'a,
        Vec<tracedecay_sessions::admission::HostDiscoveryQueueEntry>,
    > {
        Box::pin(HostAdmissionFacade::discovery_paths_after(
            self,
            scope,
            provider,
            after_sequence,
            limit,
        ))
    }

    fn discovery_path<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        sequence: u64,
    ) -> tracedecay_sessions::admission::AdmissionFuture<
        'a,
        Option<tracedecay_sessions::admission::HostDiscoveryQueueEntry>,
    > {
        Box::pin(HostAdmissionFacade::discovery_path(
            self, scope, provider, sequence,
        ))
    }
}

impl<'a> HostAdmissionFacade<'a> {
    pub const fn new(authorities: HostAdmissionAuthorities<'a>) -> Self {
        Self { authorities }
    }

    pub fn probe(&self, provider: &str, scope: HostAdmissionScope) -> HostAdmissionOutcome {
        if !supported_provider(provider) {
            return admission_outcome(
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

    #[hotpath::measure(label = "usecases.admission.get_source_cursor", future = true)]
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

    #[hotpath::measure(label = "usecases.admission.capture_observation", future = true)]
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
        project_captured_outcome(database, outcome).await
    }

    /// Sanitize then persist a bounded window through one store-owned batch.
    ///
    /// This overrides the trait default, which still walks
    /// [`Self::capture_observation`] one frame at a time. An empty window
    /// returns empty without minting a skipped-authority success.
    #[hotpath::measure(label = "usecases.admission.capture_observations", future = true)]
    pub async fn capture_observations(
        &self,
        requests: Vec<CaptureObservationRequest>,
    ) -> Result<Vec<CaptureObservationOutcome>, HostAdmissionOutcome> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        crate::hotpath_observe::admission_capture_frames(requests.len());
        let provider = first.provider().to_owned();
        let scope = first.scope().clone();
        self.authorities.validate_scope(&scope)?;
        for request in &requests {
            if request.provider() != provider || request.scope() != &scope {
                return Err(admission_outcome(
                    HostAdmissionStatus::Degraded,
                    false,
                    Some("invalid_observation_contract"),
                ));
            }
        }
        let database = self
            .authorities
            .registered_database(host_scope(&scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        let application = self.application(&provider, &scope)?;
        let provenance = self.authorities.repository_provenance.clone();
        let requests = requests
            .into_iter()
            .map(|request| request.with_repository_provenance(provenance.clone()))
            .collect();
        let outcomes = application
            .capture_observations(requests)
            .await
            .map_err(|error| classify_error(&error))?;
        project_captured_outcomes(database, outcomes).await
    }

    /// Persist one sanitized write through the store the façade already holds.
    #[hotpath::measure(label = "usecases.admission.persist_observation", future = true)]
    pub async fn persist_observation(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
        write: AnchoredObservationWrite,
    ) -> Result<ObservationPersistOutcome, HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        if write.observation().source().provider().as_str() != provider
            || write.observation().scope() != scope
        {
            return Err(admission_outcome(
                HostAdmissionStatus::Degraded,
                false,
                Some("invalid_observation_contract"),
            ));
        }
        let store = self.store(provider, scope)?;
        store
            .persist_observation(write)
            .await
            .map_err(|error| classify_error(&ObservationApplicationError::Store(error)))
    }

    /// Persist a bounded window through one store-owned writer transaction.
    ///
    /// An empty window returns empty without opening the store or minting a
    /// skipped-authority success. Mixed provider or scope writes fail closed.
    #[hotpath::measure(label = "usecases.admission.persist_observations", future = true)]
    pub async fn persist_observations(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
        writes: Vec<AnchoredObservationWrite>,
    ) -> Result<Vec<ObservationPersistOutcome>, HostAdmissionOutcome> {
        if writes.is_empty() {
            return Ok(Vec::new());
        }
        crate::hotpath_observe::admission_persist_frames(writes.len());
        self.authorities.validate_scope(scope)?;
        for write in &writes {
            if write.observation().source().provider().as_str() != provider
                || write.observation().scope() != scope
            {
                return Err(admission_outcome(
                    HostAdmissionStatus::Degraded,
                    false,
                    Some("invalid_observation_contract"),
                ));
            }
        }
        let store = self.store(provider, scope)?;
        store
            .persist_observations(writes)
            .await
            .map(|outcomes| {
                outcomes
                    .into_iter()
                    .map(|outcome| outcome.into_parts().0)
                    .collect()
            })
            .map_err(|error| classify_error(&ObservationApplicationError::Store(error)))
    }

    pub async fn capture(&self, request: CaptureObservationRequest) -> HostAdmissionOutcome {
        match self.capture_observation(request).await {
            Ok(outcome) => classify_capture(outcome),
            Err(outcome) => outcome,
        }
    }

    #[hotpath::measure(label = "usecases.admission.advance_cursor", future = true)]
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

    fn application(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
    ) -> Result<ObservationApplication<GlobalDbObservationStore>, HostAdmissionOutcome> {
        let store = self.store(provider, scope)?;
        let sanitizer = RecordSanitizerV1::observation_v1().map_err(|_| {
            admission_outcome(
                HostAdmissionStatus::Unavailable,
                false,
                Some("sanitizer_unavailable"),
            )
        })?;
        let background_cpu = process_background_cpu().ok_or_else(|| {
            admission_outcome(
                HostAdmissionStatus::Unavailable,
                false,
                Some("background_cpu_unavailable"),
            )
        })?;
        Ok(ObservationApplication::new(store, sanitizer).with_background_cpu(background_cpu))
    }

    fn store(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
    ) -> Result<GlobalDbObservationStore, HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        let scope = host_scope(scope);
        let probe = self.probe(provider, scope);
        if probe.status != HostAdmissionStatus::Supported {
            return Err(probe);
        }
        match self.authorities.registered_database(scope)? {
            Some(database) => Ok(database.observation_store()),
            None => Err(HostAdmissionOutcome::registered_authority_unavailable()),
        }
    }
}

impl EvidenceAnchorResolver for HostAdmissionFacade<'_> {
    #[hotpath::measure(label = "usecases.admission.resolve_evidence_anchor", future = true)]
    async fn resolve_evidence_anchor(
        &self,
        owner: FactOwnerV1,
        anchor_id: RetrievalAnchorId,
    ) -> Result<ResolvedEvidenceAnchor, EvidenceAnchorResolutionError> {
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
        ResolvedEvidenceAnchor::new(record).map_err(|error| {
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
    #[hotpath::measure(
        label = "usecases.admission.resolve_evidence_anchor_report",
        future = true
    )]
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
    admission_outcome(
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
        ProjectionStoreError::SequenceOverflow(_)
        | ProjectionStoreError::Gap { .. }
        | ProjectionStoreError::NotQueued
        | ProjectionStoreError::ObservationNotFound
        | ProjectionStoreError::InvalidRebuildFrontier { .. }
        | ProjectionStoreError::ProvenanceCollision
        | ProjectionStoreError::Anchor(_) => {
            HostAdmissionOutcome::degraded("projection_state_invalid")
        }
        ProjectionStoreError::UnsupportedProvider(_) => {
            HostAdmissionOutcome::degraded("projection_provider_unsupported")
        }
        ProjectionStoreError::OutputCollision { .. } => {
            HostAdmissionOutcome::degraded("projection_output_collision")
        }
        ProjectionStoreError::Contract(_) => {
            HostAdmissionOutcome::degraded("projection_contract_rejected")
        }
        ProjectionStoreError::SanitizationRefused { .. } => {
            HostAdmissionOutcome::degraded("projection_sanitization_refused")
        }
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
        CaptureObservationOutcome::AcceptedForReplay { outcome, .. }
            if matches!(*outcome, ObservationPersistOutcome::ExactDuplicate(_)) =>
        {
            admission_outcome(
                HostAdmissionStatus::ExactDuplicate,
                false,
                Some("external_source_projection_pending"),
            )
        }
        CaptureObservationOutcome::AcceptedForReplay { .. } => external_source_projection_pending(),
        CaptureObservationOutcome::Persisted { outcome, .. } => match *outcome {
            ObservationPersistOutcome::Committed(_) => {
                admission_outcome(HostAdmissionStatus::Committed, false, None)
            }
            ObservationPersistOutcome::ExactDuplicate(_) => {
                admission_outcome(HostAdmissionStatus::ExactDuplicate, false, None)
            }
            ObservationPersistOutcome::CoveredDuplicate(_) => admission_outcome(
                HostAdmissionStatus::Committed,
                false,
                Some("duplicate_coverage_committed"),
            ),
        },
        CaptureObservationOutcome::Rejected { .. } => admission_outcome(
            HostAdmissionStatus::Degraded,
            false,
            Some("sanitizer_rejected"),
        ),
        CaptureObservationOutcome::Quarantined { .. } => admission_outcome(
            HostAdmissionStatus::Degraded,
            false,
            Some("sanitizer_quarantined"),
        ),
    }
}

fn classify_external_source_error(
    error: tracedecay_session_memory::external_source_store::RuntimeExternalSourceErrorV1,
) -> HostAdmissionOutcome {
    tracing::warn!(%error, "registered external-source commit failed");
    match error {
        tracedecay_session_memory::external_source_store::RuntimeExternalSourceErrorV1::Unavailable => {
            HostAdmissionOutcome::retained_unavailable("external_source_runtime_unavailable")
        }
        _ => HostAdmissionOutcome::retained_unavailable("external_source_commit_failed"),
    }
}

async fn project_captured_outcome(
    database: &RegisteredGlobalDb,
    outcome: CaptureObservationOutcome,
) -> Result<CaptureObservationOutcome, HostAdmissionOutcome> {
    let CaptureObservationOutcome::Persisted {
        outcome: persisted, ..
    } = &outcome
    else {
        return Ok(outcome);
    };
    let projection =
        tracedecay_session_memory::external_source_store::RuntimeExternalSourceStore::new(
            database.runtime_client(),
        )
        .capture_host_observation(persisted.receipt())
        .await
        .map_err(classify_external_source_error)?;
    if let tracedecay_session_memory::external_source_store::RuntimeSourceCaptureOutcomeV1::ProjectionPending(receipt) =
        projection
    {
        return accepted_for_external_source_replay(outcome, receipt);
    }
    Ok(outcome)
}

async fn project_captured_outcomes(
    database: &RegisteredGlobalDb,
    outcomes: Vec<CaptureObservationOutcome>,
) -> Result<Vec<CaptureObservationOutcome>, HostAdmissionOutcome> {
    let mut receipts = Vec::new();
    let mut persisted_slots = Vec::new();
    for (slot, outcome) in outcomes.iter().enumerate() {
        if let CaptureObservationOutcome::Persisted {
            outcome: persisted, ..
        } = outcome
        {
            persisted_slots.push(slot);
            receipts.push(persisted.receipt().clone());
        }
    }
    if receipts.is_empty() {
        return Ok(outcomes);
    }
    let projections =
        tracedecay_session_memory::external_source_store::RuntimeExternalSourceStore::new(
            database.runtime_client(),
        )
        .capture_host_observations(&receipts)
        .await
        .map_err(classify_external_source_error)?;
    if projections.len() != persisted_slots.len() {
        return Err(HostAdmissionOutcome::retained_unavailable(
            "external_source_commit_failed",
        ));
    }
    let mut next = persisted_slots.into_iter().zip(projections);
    let mut pending = next.next();
    let mut projected = Vec::with_capacity(outcomes.len());
    for (slot, outcome) in outcomes.into_iter().enumerate() {
        if let Some((projected_slot, projection)) = pending.take() {
            if projected_slot == slot {
                if let tracedecay_session_memory::external_source_store::RuntimeSourceCaptureOutcomeV1::ProjectionPending(
                    receipt,
                ) = projection
                {
                    projected.push(accepted_for_external_source_replay(outcome, receipt)?);
                } else {
                    projected.push(outcome);
                }
                pending = next.next();
                continue;
            }
            pending = Some((projected_slot, projection));
        }
        projected.push(outcome);
    }
    Ok(projected)
}

fn accepted_for_external_source_replay(
    outcome: CaptureObservationOutcome,
    receipt: tracedecay_store::SourceCommitReceiptV1,
) -> Result<CaptureObservationOutcome, HostAdmissionOutcome> {
    let CaptureObservationOutcome::Persisted {
        outcome,
        projection_status,
        sanitized_record,
        findings,
    } = outcome
    else {
        return Err(HostAdmissionOutcome::retained_unavailable(
            "external_source_projection_receipt_mismatch",
        ));
    };
    let durable_observation_id = outcome.receipt().observation().observation_id().clone();
    let retry_handle = ExternalSourceProjectionRetryHandleV1::new(
        receipt.source_frontier().binding().clone(),
        receipt.receipt_digest().clone(),
    );
    Ok(CaptureObservationOutcome::AcceptedForReplay {
        durable_observation_id,
        projection_state: ExternalSourceProjectionStateV1::Pending,
        retry_handle,
        outcome,
        projection_status,
        sanitized_record,
        findings,
    })
}

fn classify_store_error(error: &ObservationStoreError) -> HostAdmissionOutcome {
    match error {
        ObservationStoreError::BatchRequiresScalarFallback { cause } => {
            return HostAdmissionOutcome::batch_requires_scalar_fallback(*cause);
        }
        ObservationStoreError::ObservationCollision { .. } => {
            return HostAdmissionOutcome::deterministic_content_refusal(
                "observation_identity_collision",
            );
        }
        ObservationStoreError::SanitizationReceiptCollision => {
            return HostAdmissionOutcome::deterministic_content_refusal(
                "observation_sanitization_receipt_collision",
            );
        }
        ObservationStoreError::RetrievalAnchorAliasCollision { .. } => {
            return HostAdmissionOutcome::deterministic_content_refusal(
                "observation_retrieval_anchor_alias_collision",
            );
        }
        _ => {}
    }
    let reason_code = match error {
        ObservationStoreError::CursorObservationMismatch => "observation_cursor_mismatch",
        ObservationStoreError::CursorCoverageMismatch => "observation_cursor_coverage_mismatch",
        ObservationStoreError::CursorAdvanceCollision => "observation_cursor_advance_collision",
        ObservationStoreError::CursorAdvanceLedgerDisagreement { .. } => {
            "observation_cursor_advance_ledger_disagreement"
        }
        ObservationStoreError::CursorSanitizationReceiptMismatch => {
            "observation_cursor_sanitization_receipt_mismatch"
        }
        ObservationStoreError::ObservationCollision { .. } => "observation_identity_collision",
        ObservationStoreError::SanitizationReceiptCollision => {
            "observation_sanitization_receipt_collision"
        }
        ObservationStoreError::RetrievalAnchorObservationMismatch => {
            "observation_retrieval_anchor_observation_mismatch"
        }
        ObservationStoreError::RetrievalAnchorOwnerMismatch => {
            "observation_retrieval_anchor_owner_mismatch"
        }
        ObservationStoreError::RetrievalAnchorSourceGenerationMismatch => {
            "observation_retrieval_anchor_source_generation_mismatch"
        }
        ObservationStoreError::RetrievalAnchorSourceLineageMismatch => {
            "observation_retrieval_anchor_source_lineage_mismatch"
        }
        ObservationStoreError::RetrievalAnchorProjectionGenerationMismatch => {
            "observation_retrieval_anchor_projection_generation_mismatch"
        }
        ObservationStoreError::RetrievalAnchorCollision => "observation_retrieval_anchor_collision",
        ObservationStoreError::RetrievalAnchorContract(_) => {
            "observation_retrieval_anchor_contract_invalid"
        }
        ObservationStoreError::RepositoryProvenanceAvailabilityMismatch => {
            "observation_repository_provenance_availability_mismatch"
        }
        ObservationStoreError::RepositoryProvenanceBindingMismatch => {
            "observation_repository_provenance_binding_mismatch"
        }
        ObservationStoreError::RepositoryProvenanceContract(_) => {
            "observation_repository_provenance_contract_invalid"
        }
        ObservationStoreError::RetrievalAnchorAliasCollision { .. } => {
            "observation_retrieval_anchor_alias_collision"
        }
        ObservationStoreError::InvalidReplayLimit { .. } => "observation_replay_limit_invalid",
        ObservationStoreError::Contract(_) => "observation_store_contract_invalid",
        ObservationStoreError::CursorConflict { .. } | ObservationStoreError::Storage { .. } => {
            unreachable!("retryable store failures are classified before static reason mapping")
        }
        _ => "observation_store_failed",
    };
    admission_outcome(HostAdmissionStatus::Degraded, false, Some(reason_code))
}

fn classify_error(error: &ObservationApplicationError) -> HostAdmissionOutcome {
    match error {
        ObservationApplicationError::Cancelled => admission_outcome(
            HostAdmissionStatus::Backpressured,
            true,
            Some("admission_cancelled"),
        ),
        // A worker that stopped before finishing left the batch unapplied
        // without saying anything about the observations themselves, so this
        // is an availability failure the caller re-drives once a worker is
        // back — not a rejection of the payload.
        ObservationApplicationError::BatchWorkerStopped => admission_outcome(
            HostAdmissionStatus::Unavailable,
            true,
            Some("batch_worker_stopped"),
        ),
        ObservationApplicationError::BatchContainsNonDurable => {
            HostAdmissionOutcome::deterministic_content_refusal("privacy_boundary_failed")
        }
        ObservationApplicationError::Store(ObservationStoreError::CursorConflict { .. }) => {
            admission_outcome(
                HostAdmissionStatus::Backpressured,
                true,
                Some("cursor_conflict"),
            )
        }
        ObservationApplicationError::Store(ObservationStoreError::Storage {
            operation,
            source,
        }) => {
            tracing::warn!(
                operation,
                error = %source,
                reason_code = "authority_write_failed",
                "observation store storage failure classified as authority_write_failed"
            );
            let mut outcome = admission_outcome(
                HostAdmissionStatus::Unavailable,
                true,
                Some("authority_write_failed"),
            );
            outcome.storage_cause = Some(format!("{operation}: {source}"));
            outcome
        }
        ObservationApplicationError::Contract(_) => {
            HostAdmissionOutcome::deterministic_content_refusal("invalid_observation_contract")
        }
        ObservationApplicationError::Privacy(
            PrivacySanitizerError::InvalidPolicy | PrivacySanitizerError::DetectorUnavailable,
        ) => admission_outcome(
            HostAdmissionStatus::Unavailable,
            true,
            Some("privacy_authority_unavailable"),
        ),
        ObservationApplicationError::Privacy(_) => {
            HostAdmissionOutcome::deterministic_content_refusal("privacy_boundary_failed")
        }
        ObservationApplicationError::Store(error) => classify_store_error(error),
    }
}

#[cfg(test)]
#[path = "host_admission_batch_test.rs"]
mod host_admission_batch_test;
#[cfg(test)]
#[path = "host_admission_test.rs"]
mod host_admission_test;
