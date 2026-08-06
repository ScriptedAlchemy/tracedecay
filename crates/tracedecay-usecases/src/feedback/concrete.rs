//! Concrete feedback runtime over existing project-store authorities.
//!
//! Durable publications use the project database's existing transactional
//! metadata lane. Request and continuation handles use the existing
//! response-handle store. Diagnostic expansion uses `DiagnosticsStore`.
//! This module adds no database table, cache, cursor codec, retry loop, or
//! provider execution path. Authorization consumes the daemon-resolved
//! `ProjectSourceAccessSnapshot` once per operation and issues one receipt.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tracedecay_application::feedback::{
    FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1, FEEDBACK_DIAGNOSTICS_USE_CASE_ID_V1,
    FEEDBACK_EXPAND_CAPABILITY_ID_V1, FEEDBACK_EXPAND_USE_CASE_ID_V1,
    FEEDBACK_GET_CAPABILITY_ID_V1, FEEDBACK_GET_USE_CASE_ID_V1, FEEDBACK_LIST_CAPABILITY_ID_V1,
    FEEDBACK_LIST_USE_CASE_ID_V1, FeedbackCompletedPublicationReadPort,
    FeedbackCompletedPublicationV1, FeedbackCycleDedupePort, FeedbackCycleDedupePublicationState,
    FeedbackCycleDedupeState, FeedbackDiagnosticsReadRequestV1, FeedbackDiagnosticsReadResultV1,
    FeedbackExpandRequestV1, FeedbackExpandResultV1, FeedbackFindingReadV1, FeedbackGetRequestV1,
    FeedbackGetResultV1, FeedbackListRequestV1, FeedbackListResultV1, FeedbackObservationPort,
    FeedbackPortFuture, FeedbackReadPortContext, FeedbackReadPortFuture, FeedbackReadService,
    FeedbackRouteAdmission, FeedbackRouteAuthorizationPort, feedback_surface_operation,
};
use tracedecay_application::{
    ApplicationContractError, ApplicationOperation, ApplicationProblem, AuthorityReceipt,
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    EvidenceDomain, OpaqueCursor, PageRequest, PolicyDecisionRef, RequestAdmission, RequestContext,
    RequestId, ResolvedScope, ResultProjection, RetrievalOrder, RetrievalRequestMeta,
    RetryDirective, now_micros,
};
use tracedecay_domain::feedback::{FeedbackDedupeKeyV1, FeedbackFindingId, FeedbackFindingV1};
use tracedecay_domain::{ActorId, ComponentVersion, ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_store::DiagnosticStore;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::concrete_evidence::{complete, interruption, unavailable};
use super::observations::{
    DurablePlan26FeedbackObservationQueueAdapterV1, DurablePlan26FeedbackObservationSinkV1,
    FeedbackObservationEnvelopeV1, FeedbackObservationReadModelV1, FeedbackObservationSinkOutcome,
    Plan26AnchorOperationV1, Plan26FeedbackObservationAdapter, Plan26FeedbackObservationEmitterV1,
    Plan26FeedbackOutcomeV1, Plan26FeedbackSourceEventV1,
};
use super::owner::{
    AuthorizedFeedbackReadRequestV1, CanonicalFeedbackReadOwnerV1, DaemonFeedbackReadOwnerV1,
    DurableFeedbackReadStoreV1, FeedbackReadOperationV1, FeedbackReadRequestAuthority,
    FeedbackReadRequestAuthorityFuture, FeedbackReadRequestResolutionV1, FeedbackReadRequestV1,
};
use crate::diagnostics_store::DiagnosticsStore;
use crate::response_handles::{
    ResponseHandleLookup, is_valid_response_handle, retrieve_response_handle, store_response_handle,
};
use crate::source_authorization::ProjectSourceAccessSnapshot;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_runtime_core::db::{Database, DatabaseWriteTransaction};

const PUBLICATION_LEDGER_METADATA_KEY: &str = "feedback.completed-publications.v1";
const OBSERVATION_LEDGER_METADATA_KEY: &str = "feedback.plan26-observations.v1";
const PUBLICATION_LEDGER_SCHEMA_VERSION: u16 = 1;
const OBSERVATION_LEDGER_SCHEMA_VERSION: u16 = 3;
const REQUEST_HANDLE_SCHEMA_VERSION: u16 = 1;
const REQUEST_HANDLE_TTL_MICROS: i64 = 15_000_000;
const DEFAULT_EXPANSION_PAGE_SIZE: u32 = 100;
const MAX_STORED_PUBLICATIONS: usize = 4_096;
const MAX_PUBLICATION_LEDGER_BYTES: usize = 8 * 1_024 * 1_024;
const MAX_STORED_OBSERVATIONS: usize = 16_384;
const MAX_STORED_OBSERVATION_BOOTS: usize = 256;
const MAX_OBSERVATION_LEDGER_BYTES: usize = 8 * 1_024 * 1_024;
const OBSERVATION_QUEUE_CAPACITY: usize = 1_024;

pub type ConcreteFeedbackOwner = DaemonFeedbackReadOwnerV1<
    ProjectFeedbackRequestAuthority,
    CanonicalFeedbackReadOwnerV1<ProjectFeedbackStore>,
    ProjectFeedbackRouteAuthorization,
>;

#[derive(Debug, Error)]
pub enum FeedbackRuntimeError {
    #[error("feedback runtime contract is invalid")]
    Contract(#[from] ApplicationContractError),
    #[error("feedback runtime route access is denied")]
    AccessDenied,
    #[error("feedback runtime store operation failed")]
    Store,
    #[error("feedback runtime handle operation failed")]
    Handle,
    #[error("feedback runtime record is corrupt")]
    Corrupt,
}

#[derive(Clone)]
pub struct ProjectFeedbackStore {
    database: Database,
    project_root: PathBuf,
    source_observations: Option<Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>>,
}

#[derive(Clone)]
pub struct ProjectFeedbackRequestAuthority {
    project_root: PathBuf,
    scope: ResolvedScope,
    requester: ActorId,
    maximum_expiry: UtcMicros,
}

#[derive(Clone)]
pub struct ProjectFeedbackRouteAuthorization {
    access: ProjectSourceAccessSnapshot,
}

impl ProjectFeedbackRouteAuthorization {
    pub fn allows(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        observed_at: UtcMicros,
    ) -> bool {
        context.admission_at(observed_at) == RequestAdmission::Admitted
            && self.access.allows(context, operation, observed_at)
    }
}

pub struct FeedbackRuntime {
    owner: Arc<ConcreteFeedbackOwner>,
    requests: ProjectFeedbackRequestAuthority,
    publications: ProjectFeedbackStore,
    access: ProjectSourceAccessSnapshot,
    observations: Arc<dyn FeedbackObservationPort + Send + Sync>,
    source_observations: Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPublicationLedgerV1 {
    schema_version: u16,
    publications: Vec<FeedbackCompletedPublicationV1>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFeedbackObservationLedgerV1 {
    schema_version: u16,
    observations: Vec<FeedbackObservationEnvelopeV1>,
    #[serde(default)]
    retention_dropped: u64,
    #[serde(default)]
    producer_boots: Vec<StoredFeedbackProducerBootV1>,
    #[serde(default)]
    retained_incomplete_boots: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFeedbackProducerBootV1 {
    boot_id: ManifestDigest,
    last_sequence: u64,
    terminal: bool,
    #[serde(default)]
    last_observed_at: Option<UtcMicros>,
}

struct ProjectFeedbackObservationSinkV1 {
    sender: mpsc::Sender<FeedbackObservationEnvelopeV1>,
    control_sender: mpsc::Sender<FeedbackObservationEnvelopeV1>,
    boot_id: ManifestDigest,
    next_sequence: AtomicU64,
    dropped_count: Arc<AtomicU64>,
    admission: Mutex<()>,
}

impl ProjectFeedbackObservationSinkV1 {
    async fn start(database: Database) -> Result<Self, FeedbackRuntimeError> {
        static BOOT_NONCE: AtomicU64 = AtomicU64::new(0);
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| FeedbackRuntimeError::Store)?;
        let boot_id = canonical_sha256(&(
            "tracedecay.feedback.producer-boot.v1",
            std::process::id(),
            now_micros(),
            BOOT_NONCE.fetch_add(1, Ordering::Relaxed),
        ))
        .map_err(|_| FeedbackRuntimeError::Corrupt)?;
        persist_feedback_producer_boot(&database, boot_id.clone()).await?;
        let (sender, mut receiver) = mpsc::channel(OBSERVATION_QUEUE_CAPACITY);
        let (control_sender, mut control_receiver) = mpsc::channel(1);
        let dropped_count = Arc::new(AtomicU64::new(0));
        let worker_dropped_count = Arc::clone(&dropped_count);
        runtime.spawn(async move {
            let mut data_open = true;
            let mut control_open = true;
            while data_open || control_open {
                let envelope = tokio::select! {
                    biased;
                    envelope = receiver.recv(), if data_open => {
                        if envelope.is_none() {
                            data_open = false;
                        }
                        envelope
                    },
                    envelope = control_receiver.recv(), if control_open => {
                        if envelope.is_none() {
                            control_open = false;
                        }
                        envelope
                    },
                };
                let Some(mut envelope) = envelope else {
                    continue;
                };
                let reported_drops =
                    apply_pending_worker_drops(&mut envelope, &worker_dropped_count);
                if persist_feedback_observation(&database, envelope)
                    .await
                    .is_err()
                {
                    saturating_add(&worker_dropped_count, reported_drops.saturating_add(1));
                }
            }
        });
        Ok(Self {
            sender,
            control_sender,
            boot_id,
            next_sequence: AtomicU64::new(0),
            dropped_count,
            admission: Mutex::new(()),
        })
    }

    fn next_sequence(&self) -> u64 {
        self.next_sequence
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    fn record_drop(&self) {
        saturating_increment(&self.dropped_count);
    }

    fn restore_drops(&self, dropped: u64) {
        if dropped == 0 {
            return;
        }
        let _ = self
            .dropped_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                Some(count.saturating_add(dropped))
            });
    }
}

impl DurablePlan26FeedbackObservationSinkV1 for ProjectFeedbackObservationSinkV1 {
    fn enqueue_durable_feedback_observation(
        &self,
        mut envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome {
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sequence = self.next_sequence();
        let dropped = self.dropped_count.swap(0, Ordering::Relaxed);
        if envelope
            .assign_delivery(
                self.boot_id.clone(),
                sequence,
                super::observations::FeedbackObservationDeliveryV1::delivered(dropped),
            )
            .is_none()
        {
            self.restore_drops(dropped);
            self.record_drop();
            return FeedbackObservationSinkOutcome::Dropped;
        }
        match self.sender.try_send(envelope) {
            Ok(()) => FeedbackObservationSinkOutcome::Enqueued,
            Err(mpsc::error::TrySendError::Full(_) | mpsc::error::TrySendError::Closed(_)) => {
                self.restore_drops(dropped);
                self.record_drop();
                FeedbackObservationSinkOutcome::Dropped
            }
        }
    }
}

impl Drop for ProjectFeedbackObservationSinkV1 {
    fn drop(&mut self) {
        let dropped = self.dropped_count.swap(0, Ordering::Relaxed);
        let sequence = self.next_sequence();
        let observed_at = now_micros();
        let Some(mut envelope) =
            super::observations::plan26_feedback_source_event_envelope_for_subject(
                self.boot_id.clone(),
                observed_at,
                Plan26FeedbackSourceEventV1::TelemetryDropObserved {
                    dropped_count: dropped,
                    last_sequence: sequence.saturating_sub(1),
                    terminal: true,
                },
            )
        else {
            return;
        };
        if envelope
            .assign_delivery(
                self.boot_id.clone(),
                sequence,
                super::observations::FeedbackObservationDeliveryV1::delivered(dropped),
            )
            .is_some()
        {
            let _ = self.control_sender.try_send(envelope);
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFeedbackRequestV1 {
    schema_version: u16,
    operation: FeedbackReadOperationV1,
    request: FeedbackReadRequestV1,
    request_id: String,
    scope_digest: ManifestDigest,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
    #[serde(default)]
    after_finding_id: Option<FeedbackFindingId>,
}

/// Concrete factory central daemon integration mounts for one admitted
/// project root.
pub async fn open_feedback_runtime(
    database: Database,
    project_root: impl Into<PathBuf>,
    scope: ResolvedScope,
    access: ProjectSourceAccessSnapshot,
) -> Result<FeedbackRuntime, FeedbackRuntimeError> {
    FeedbackRuntime::open(database, project_root, scope, access).await
}

impl FeedbackRuntime {
    pub async fn open(
        database: Database,
        project_root: impl Into<PathBuf>,
        scope: ResolvedScope,
        access: ProjectSourceAccessSnapshot,
    ) -> Result<Self, FeedbackRuntimeError> {
        scope.validate()?;
        if access.scope != scope {
            return Err(FeedbackRuntimeError::AccessDenied);
        }
        let project_root = project_root.into();
        let requester = access.requester.clone();
        let maximum_expiry = access.grant_expires_at;
        let requests = ProjectFeedbackRequestAuthority {
            project_root: project_root.clone(),
            scope,
            requester,
            maximum_expiry,
        };
        let mut publications = ProjectFeedbackStore {
            database,
            project_root,
            source_observations: None,
        };
        let route_authorization = ProjectFeedbackRouteAuthorization {
            access: access.clone(),
        };
        let durable_observations = DurablePlan26FeedbackObservationQueueAdapterV1::new(
            ProjectFeedbackObservationSinkV1::start(publications.database.clone()).await?,
        );
        let observation_adapter =
            Arc::new(Plan26FeedbackObservationAdapter::new(durable_observations));
        publications.source_observations = Some(observation_adapter.clone());
        let service = FeedbackReadService::new(
            CanonicalFeedbackReadOwnerV1::new(publications.clone()),
            route_authorization,
            tracedecay_application::feedback::feedback_read_operations()?,
        );
        let owner = Arc::new(DaemonFeedbackReadOwnerV1::new(requests.clone(), service));
        Ok(Self {
            owner,
            requests,
            publications,
            access,
            observations: observation_adapter.clone(),
            source_observations: observation_adapter,
        })
    }

    pub fn owner(&self) -> Arc<ConcreteFeedbackOwner> {
        Arc::clone(&self.owner)
    }

    pub fn publication_store(&self) -> ProjectFeedbackStore {
        self.publications.clone()
    }

    /// Returns the same daemon-route authorization used by the reader.
    pub fn route_authorization(&self) -> ProjectFeedbackRouteAuthorization {
        ProjectFeedbackRouteAuthorization {
            access: self.access.clone(),
        }
    }

    pub fn observation_port(&self) -> Arc<dyn FeedbackObservationPort + Send + Sync> {
        Arc::clone(&self.observations)
    }

    pub fn source_observation_port(
        &self,
    ) -> Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync> {
        Arc::clone(&self.source_observations)
    }

    pub fn project_root(&self) -> &Path {
        &self.requests.project_root
    }

    pub fn scope(&self) -> &ResolvedScope {
        &self.requests.scope
    }

    pub fn request_expiry_at(
        &self,
        observed_at: UtcMicros,
    ) -> Result<UtcMicros, FeedbackRuntimeError> {
        self.requests.expiry_at(observed_at)
    }

    pub fn mint_request(
        &self,
        request_id: impl Into<String>,
        request: FeedbackReadRequestV1,
        observed_at: UtcMicros,
    ) -> Result<String, FeedbackRuntimeError> {
        self.requests.mint(request_id, request, observed_at)
    }

    pub fn mint_diagnostics(
        &self,
        request_id: impl Into<String>,
        request: FeedbackDiagnosticsReadRequestV1,
        observed_at: UtcMicros,
    ) -> Result<String, FeedbackRuntimeError> {
        self.mint_request(
            request_id,
            FeedbackReadRequestV1::Diagnostics(request),
            observed_at,
        )
    }

    pub fn mint_list(
        &self,
        request_id: impl Into<String>,
        head_commit_id: Option<tracedecay_domain::CommitId>,
        page_size: u32,
        observed_at: UtcMicros,
    ) -> Result<String, FeedbackRuntimeError> {
        let page = PageRequest::first(page_size)?;
        self.mint_request(
            request_id,
            FeedbackReadRequestV1::List(FeedbackListRequestV1 {
                head_commit_id,
                page,
            }),
            observed_at,
        )
    }

    pub fn mint_get(
        &self,
        request_id: impl Into<String>,
        finding_id: FeedbackFindingId,
        observed_at: UtcMicros,
    ) -> Result<String, FeedbackRuntimeError> {
        self.mint_request(
            request_id,
            FeedbackReadRequestV1::Get(FeedbackGetRequestV1 { finding_id }),
            observed_at,
        )
    }

    pub fn mint_expand(
        &self,
        request_id: impl Into<String>,
        request: FeedbackExpandRequestV1,
        observed_at: UtcMicros,
    ) -> Result<String, FeedbackRuntimeError> {
        self.mint_request(
            request_id,
            FeedbackReadRequestV1::Expand(request),
            observed_at,
        )
    }
}

impl ProjectFeedbackRequestAuthority {
    fn expiry_at(&self, observed_at: UtcMicros) -> Result<UtcMicros, FeedbackRuntimeError> {
        let expires_at = UtcMicros(
            observed_at
                .0
                .saturating_add(REQUEST_HANDLE_TTL_MICROS)
                .min(self.maximum_expiry.0),
        );
        (expires_at > observed_at)
            .then_some(expires_at)
            .ok_or(FeedbackRuntimeError::AccessDenied)
    }

    pub fn mint(
        &self,
        request_id: impl Into<String>,
        request: FeedbackReadRequestV1,
        observed_at: UtcMicros,
    ) -> Result<String, FeedbackRuntimeError> {
        validate_request(&request)?;
        let request_id = request_id.into();
        RequestId::new(request_id.clone())?;
        let expires_at = self.expiry_at(observed_at)?;
        store_request_handle(
            &self.project_root,
            StoredFeedbackRequestV1 {
                schema_version: REQUEST_HANDLE_SCHEMA_VERSION,
                operation: request.operation(),
                request,
                request_id,
                scope_digest: self.scope.scope_digest.clone(),
                issued_at: observed_at,
                expires_at,
                after_finding_id: None,
            },
            observed_at,
        )
    }

    fn resolve_record(
        &self,
        operation: FeedbackReadOperationV1,
        handle: &str,
        observed_at: UtcMicros,
    ) -> Result<AuthorizedFeedbackReadRequestV1, FeedbackReadRequestResolutionV1> {
        let mut record = load_handle_content::<StoredFeedbackRequestV1>(
            &self.project_root,
            handle,
            observed_at,
        )?;
        if record.schema_version != REQUEST_HANDLE_SCHEMA_VERSION
            || record.operation != operation
            || record.request.operation() != operation
            || record.scope_digest != self.scope.scope_digest
            || record.issued_at >= record.expires_at
            || observed_at >= record.expires_at
            || record.expires_at > self.maximum_expiry
            || (operation != FeedbackReadOperationV1::List && record.after_finding_id.is_some())
        {
            return Err(FeedbackReadRequestResolutionV1::NotFoundOrNotAuthorized);
        }
        if validate_request(&record.request).is_err()
            || RequestId::new(record.request_id.clone()).is_err()
        {
            return Err(FeedbackReadRequestResolutionV1::NotFoundOrNotAuthorized);
        }
        let (capability, use_case) = operation_ids(operation);
        let capability = CapabilityId::new(capability)
            .map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?;
        let use_case =
            UseCaseId::new(use_case).map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?;
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new(format!("grant.daemon.feedback.{handle}"))
                .map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?,
            1,
            canonical_sha256(&(
                "tracedecay.feedback.request-grant.v1",
                handle,
                &record.scope_digest,
                capability.as_str(),
                use_case.as_str(),
            ))
            .map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?,
            ActorId::new("actor.tracedecay-daemon")
                .map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?,
            record.issued_at,
            record.expires_at,
            self.scope.clone(),
            std::collections::BTreeSet::from([capability]),
            std::collections::BTreeSet::from([use_case]),
            DisclosureClass::Evidence,
        )
        .map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?;
        let context = RequestContext::new(
            self.requester.clone(),
            self.scope.clone(),
            grant,
            RequestId::new(record.request_id)
                .map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?,
            Deadline::new(record.expires_at)
                .map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?,
            CancellationContext::active(format!("feedback.request.{handle}"))
                .map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?,
        )
        .map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?;
        if matches!(&record.request, FeedbackReadRequestV1::List(_))
            && record.after_finding_id.is_some()
            && let FeedbackReadRequestV1::List(request) = &mut record.request
        {
            request.page.cursor = Some(
                OpaqueCursor::new(handle.to_owned())
                    .map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?,
            );
        }
        Ok(AuthorizedFeedbackReadRequestV1 {
            context,
            request: record.request,
        })
    }
}

impl FeedbackReadRequestAuthority for ProjectFeedbackRequestAuthority {
    fn resolve<'a>(
        &'a self,
        operation: FeedbackReadOperationV1,
        request_handle: &'a str,
        observed_at: UtcMicros,
    ) -> FeedbackReadRequestAuthorityFuture<'a> {
        Box::pin(async move { self.resolve_record(operation, request_handle, observed_at) })
    }
}

impl FeedbackRouteAuthorizationPort for ProjectFeedbackRouteAuthorization {
    fn admit(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        observed_at: UtcMicros,
    ) -> Result<FeedbackRouteAdmission, ApplicationProblem> {
        match context.admission_at(observed_at) {
            RequestAdmission::Cancelled => {
                return Err(ApplicationProblem::cancelled_before_admission());
            }
            RequestAdmission::TimedOut => {
                return Err(ApplicationProblem::timed_out_before_admission());
            }
            RequestAdmission::Admitted => {}
        }
        if !self.allows(context, operation, observed_at) {
            return Err(ApplicationProblem::not_found_or_not_authorized(
                RetryDirective::Never,
            ));
        }
        AuthorityReceipt::from_context(
            context,
            PolicyDecisionRef::new(
                format!("route.feedback.{}", self.access.binding.binding_id.as_str()),
                1,
                self.access.configuration_provenance_digest.clone(),
                ComponentVersion::new("project-source-access.v1").map_err(|_| {
                    ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
                })?,
            )
            .map_err(|_| ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never))?,
            observed_at,
        )
        .map(FeedbackRouteAdmission::Routed)
        .map_err(|_| ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never))
    }

    fn recheck_publication(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        admission: &FeedbackRouteAdmission,
        observed_at: UtcMicros,
    ) -> Result<AuthorityReceipt, ApplicationProblem> {
        let current = self.admit(context, operation, observed_at)?;
        let FeedbackRouteAdmission::Routed(admission) = admission else {
            return Err(ApplicationProblem::not_found_or_not_authorized(
                RetryDirective::Never,
            ));
        };
        let FeedbackRouteAdmission::Routed(current) = current else {
            return Err(ApplicationProblem::not_found_or_not_authorized(
                RetryDirective::Never,
            ));
        };
        if admission.grant_id != current.grant_id
            || admission.grant_revision != current.grant_revision
            || admission.grant_digest != current.grant_digest
            || admission.authorized_scope_digest != current.authorized_scope_digest
            || admission.disclosure != current.disclosure
            || admission.policy != current.policy
        {
            return Err(ApplicationProblem::not_found_or_not_authorized(
                RetryDirective::Never,
            ));
        }
        Ok(current)
    }
}

impl FeedbackCycleDedupePort for ProjectFeedbackStore {
    fn lookup_completed<'a>(
        &'a self,
        context: &'a RequestContext,
        key: &'a FeedbackDedupeKeyV1,
    ) -> FeedbackPortFuture<'a, FeedbackCycleDedupeState> {
        if key.validate().is_err() {
            return Box::pin(async { FeedbackCycleDedupeState::Unavailable });
        }
        Box::pin(async move {
            match self.load_publications().await {
                Ok(publications) => {
                    if publications.iter().any(|publication| {
                        publication.dedupe_key == *key
                            && publication_matches_context(publication, context)
                    }) {
                        FeedbackCycleDedupeState::Duplicate
                    } else {
                        FeedbackCycleDedupeState::Unique
                    }
                }
                Err(_) => FeedbackCycleDedupeState::Unavailable,
            }
        })
    }

    fn record_completed<'a>(
        &'a self,
        context: &'a RequestContext,
        publication: &'a FeedbackCompletedPublicationV1,
    ) -> FeedbackPortFuture<'a, FeedbackCycleDedupePublicationState> {
        if publication.validate().is_err() {
            return Box::pin(async { FeedbackCycleDedupePublicationState::Unavailable });
        }
        match context.admission_at(publication.authority.revalidated_at) {
            RequestAdmission::Admitted => {}
            RequestAdmission::Cancelled => {
                return Box::pin(async { FeedbackCycleDedupePublicationState::Cancelled });
            }
            RequestAdmission::TimedOut => {
                return Box::pin(async { FeedbackCycleDedupePublicationState::TimedOut });
            }
        }
        Box::pin(async move {
            if !publication_matches_context(publication, context) {
                return FeedbackCycleDedupePublicationState::Unavailable;
            }
            match self.record_publication(publication.clone()).await {
                Ok(true) => FeedbackCycleDedupePublicationState::Recorded,
                Ok(false) => FeedbackCycleDedupePublicationState::Duplicate,
                Err(_) => FeedbackCycleDedupePublicationState::Unavailable,
            }
        })
    }
}

impl FeedbackCompletedPublicationReadPort for FeedbackRuntime {
    fn latest_committed<'a>(
        &'a self,
        context: &'a RequestContext,
        observed_at: UtcMicros,
    ) -> FeedbackPortFuture<'a, Option<FeedbackCompletedPublicationV1>> {
        if context.admission_at(observed_at) != RequestAdmission::Admitted {
            return Box::pin(async { None });
        }
        let Ok(Some(operation)) = feedback_surface_operation("feedback_list") else {
            return Box::pin(async { None });
        };
        let authorization = self.route_authorization();
        let store = self.publication_store();
        Box::pin(async move {
            if !authorization.allows(context, &operation, observed_at) {
                return None;
            }
            store
                .scoped_publications(context)
                .await
                .ok()?
                .into_iter()
                .max_by(publication_order)
        })
    }
}

impl DurableFeedbackReadStoreV1 for ProjectFeedbackStore {
    fn diagnostics<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackDiagnosticsReadRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackDiagnosticsReadResultV1> {
        Box::pin(async move {
            let domains = vec![
                EvidenceDomain::Diagnostic,
                EvidenceDomain::Graph,
                EvidenceDomain::Test,
            ];
            if let Some(interrupted) = interruption(context.request, now_micros(), domains.clone())
            {
                return interrupted;
            }
            let Ok(publications) = self.scoped_publications(context.request).await else {
                return unavailable(now_micros(), domains);
            };
            let finished_at = now_micros();
            if let Some(interrupted) = interruption(context.request, finished_at, domains.clone()) {
                return interrupted;
            }
            let publication = publications
                .iter()
                .filter(|publication| {
                    publication.result.scope.head_commit_id == request.head_commit_id
                })
                .max_by(|left, right| publication_order(left, right));
            let Some(publication) = publication else {
                return unavailable(finished_at, domains);
            };
            complete(
                FeedbackDiagnosticsReadResultV1 {
                    cycle: publication.result.clone(),
                },
                vec![publication],
                domains,
                None,
                None,
                finished_at,
            )
        })
    }

    fn get<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackGetRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackGetResultV1> {
        Box::pin(async move {
            let domains = vec![EvidenceDomain::Diagnostic];
            if let Some(interrupted) = interruption(context.request, now_micros(), domains.clone())
            {
                return interrupted;
            }
            let Ok(publications) = self.scoped_publications(context.request).await else {
                return unavailable(now_micros(), domains);
            };
            let finished_at = now_micros();
            if let Some(interrupted) = interruption(context.request, finished_at, domains.clone()) {
                return interrupted;
            }
            let selected = latest_finding(&publications, &request.finding_id);
            let Some((publication, finding)) = selected else {
                return unavailable(finished_at, domains);
            };
            let Ok(view) = self.finding_view(context.request, publication, finding, finished_at)
            else {
                return unavailable(finished_at, domains);
            };
            complete(
                FeedbackGetResultV1 { finding: view },
                vec![publication],
                domains,
                None,
                None,
                finished_at,
            )
        })
    }

    fn expand<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackExpandRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackExpandResultV1> {
        Box::pin(async move {
            let domains = vec![EvidenceDomain::Anchor];
            let started_at = now_micros();
            if let Some(interrupted) = interruption(context.request, started_at, domains.clone()) {
                self.observe_expansion(
                    context.request,
                    request,
                    interruption_outcome(context.request, started_at),
                    0,
                    started_at,
                );
                return interrupted;
            }
            let Ok(publications) = self.scoped_publications(context.request).await else {
                let observed_at = now_micros();
                self.observe_expansion(
                    context.request,
                    request,
                    Plan26FeedbackOutcomeV1::Unavailable,
                    0,
                    observed_at,
                );
                return unavailable(observed_at, domains);
            };
            let mut finished_at = now_micros();
            if let Some(interrupted) = interruption(context.request, finished_at, domains.clone()) {
                self.observe_expansion(
                    context.request,
                    request,
                    interruption_outcome(context.request, finished_at),
                    0,
                    finished_at,
                );
                return interrupted;
            }
            let Some((publication, finding)) = latest_finding(&publications, &request.finding_id)
            else {
                self.observe_expansion(
                    context.request,
                    request,
                    Plan26FeedbackOutcomeV1::Stale,
                    0,
                    finished_at,
                );
                return unavailable(finished_at, domains);
            };
            if finding.retrieval_anchor_id.as_ref() != Some(&request.expansion.anchor) {
                self.observe_expansion(
                    context.request,
                    request,
                    Plan26FeedbackOutcomeV1::Denied,
                    0,
                    finished_at,
                );
                return unavailable(finished_at, domains);
            }
            let diagnostics = DiagnosticsStore::new(self.database.conn());
            let Ok(Some(_)) = diagnostics
                .diagnostic_by_anchor(&request.expansion.anchor)
                .await
            else {
                let observed_at = now_micros();
                self.observe_expansion(
                    context.request,
                    request,
                    Plan26FeedbackOutcomeV1::Partial,
                    0,
                    observed_at,
                );
                return unavailable(observed_at, domains);
            };
            let expanded = tracedecay_application::AnchorExpandResult {
                anchors: vec![request.expansion.anchor.clone()],
            };
            finished_at = now_micros();
            if let Some(interrupted) = interruption(context.request, finished_at, domains.clone()) {
                self.observe_expansion(
                    context.request,
                    request,
                    interruption_outcome(context.request, finished_at),
                    0,
                    finished_at,
                );
                return interrupted;
            }
            let Ok(view) = self.finding_view(context.request, publication, finding, finished_at)
            else {
                self.observe_expansion(
                    context.request,
                    request,
                    Plan26FeedbackOutcomeV1::Unavailable,
                    0,
                    finished_at,
                );
                return unavailable(finished_at, domains);
            };
            self.observe_expansion(
                context.request,
                request,
                Plan26FeedbackOutcomeV1::Completed,
                expanded.anchors.len().try_into().unwrap_or(u32::MAX),
                finished_at,
            );
            complete(
                FeedbackExpandResultV1 {
                    finding: view,
                    expansion: expanded,
                },
                vec![publication],
                domains,
                None,
                None,
                finished_at,
            )
        })
    }

    fn list<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackListRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackListResultV1> {
        Box::pin(async move {
            let domains = vec![EvidenceDomain::Diagnostic];
            if let Some(interrupted) = interruption(context.request, now_micros(), domains.clone())
            {
                return interrupted;
            }
            let Ok(publications) = self.scoped_publications(context.request).await else {
                return unavailable(now_micros(), domains);
            };
            let mut finished_at = now_micros();
            if let Some(interrupted) = interruption(context.request, finished_at, domains.clone()) {
                return interrupted;
            }
            let mut latest = BTreeMap::<FeedbackFindingId, (usize, FeedbackFindingV1)>::new();
            for (index, publication) in publications.iter().enumerate() {
                if request
                    .head_commit_id
                    .as_ref()
                    .is_some_and(|head| &publication.result.scope.head_commit_id != head)
                {
                    continue;
                }
                for finding in &publication.result.findings {
                    match latest.get(&finding.finding_id) {
                        Some((prior, _))
                            if publication_order(&publications[*prior], publication)
                                != std::cmp::Ordering::Less => {}
                        _ => {
                            latest.insert(finding.finding_id.clone(), (index, finding.clone()));
                        }
                    }
                }
            }
            let records: Vec<_> = latest.into_iter().collect();
            let Ok(start) = self.list_start(context.request, request, &records, finished_at) else {
                return unavailable(finished_at, domains);
            };
            let end = start
                .saturating_add(request.page.page_size as usize)
                .min(records.len());
            let selected = &records[start..end];
            let mut views = Vec::with_capacity(selected.len());
            let mut authorities = Vec::new();
            for (_, (publication_index, finding)) in selected {
                let publication = &publications[*publication_index];
                let Ok(view) =
                    self.finding_view(context.request, publication, finding, finished_at)
                else {
                    return unavailable(finished_at, domains);
                };
                views.push(view);
                authorities.push(publication);
            }
            let (cursor, expires_at) = if end < records.len() {
                match self.next_list_handle(
                    context.request,
                    request,
                    &records[end - 1].0,
                    finished_at,
                ) {
                    Ok(next) => (Some(next.0), Some(next.1)),
                    Err(_) => {
                        return unavailable(finished_at, domains);
                    }
                }
            } else {
                (None, None)
            };
            finished_at = now_micros();
            if let Some(interrupted) = interruption(context.request, finished_at, domains.clone()) {
                return interrupted;
            }
            complete(
                FeedbackListResultV1 { findings: views },
                authorities,
                domains,
                Some((records.len() as u64, cursor)),
                expires_at,
                finished_at,
            )
        })
    }
}

impl ProjectFeedbackStore {
    pub async fn observation_read_model(
        &self,
    ) -> Result<FeedbackObservationReadModelV1, FeedbackRuntimeError> {
        plan26_feedback_observation_read_model(&self.database).await
    }

    fn observe_expansion(
        &self,
        context: &RequestContext,
        request: &FeedbackExpandRequestV1,
        outcome: Plan26FeedbackOutcomeV1,
        returned_count: u32,
        observed_at: UtcMicros,
    ) {
        let (Some(observations), Ok(subject_digest)) = (
            self.source_observations.as_ref(),
            canonical_sha256(&(
                "tracedecay.feedback.expansion-observation.v1",
                &context.scope().scope_digest,
                request,
            )),
        ) else {
            return;
        };
        observations.observe_source_event_for_subject(
            subject_digest,
            observed_at,
            Plan26FeedbackSourceEventV1::AnchorExpansion {
                operation: Plan26AnchorOperationV1::EvidenceExpansion,
                outcome,
                returned_count,
                duration_micros: None,
            },
        );
    }

    async fn load_publications(
        &self,
    ) -> Result<Vec<FeedbackCompletedPublicationV1>, FeedbackRuntimeError> {
        let Some(encoded) = self
            .database
            .get_metadata(PUBLICATION_LEDGER_METADATA_KEY)
            .await
            .map_err(|_| FeedbackRuntimeError::Store)?
        else {
            return Ok(Vec::new());
        };
        if encoded.len() > MAX_PUBLICATION_LEDGER_BYTES {
            return Err(FeedbackRuntimeError::Corrupt);
        }
        decode_ledger(&encoded)
    }

    async fn scoped_publications(
        &self,
        context: &RequestContext,
    ) -> Result<Vec<FeedbackCompletedPublicationV1>, FeedbackRuntimeError> {
        let publications = self.load_publications().await?;
        Ok(publications
            .into_iter()
            .filter(|publication| publication_matches_context(publication, context))
            .collect())
    }

    /// Latest validated durable publication visible in the exact admitted
    /// project/repository/worktree/ref scope. Doctor consumes this mounted read
    /// store; it does not scan provider-local state or mutable paths.
    pub async fn doctor_latest_publication(
        &self,
        context: &RequestContext,
    ) -> Result<Option<FeedbackCompletedPublicationV1>, FeedbackRuntimeError> {
        let publication = self
            .scoped_publications(context)
            .await?
            .into_iter()
            .max_by(publication_order);
        if let Some(publication) = &publication {
            publication
                .validate()
                .map_err(|_| FeedbackRuntimeError::Corrupt)?;
        }
        Ok(publication)
    }

    async fn record_publication(
        &self,
        publication: FeedbackCompletedPublicationV1,
    ) -> Result<bool, FeedbackRuntimeError> {
        publication
            .validate()
            .map_err(|_| FeedbackRuntimeError::Corrupt)?;
        let transaction = self
            .database
            .begin_write_transaction("record feedback completed publication")
            .await
            .map_err(|_| FeedbackRuntimeError::Store)?;
        let mut rows = transaction
            .query_engine(
                "SELECT value FROM metadata WHERE key = ?1",
                params![PUBLICATION_LEDGER_METADATA_KEY],
            )
            .await
            .map_err(|_| FeedbackRuntimeError::Store)?;
        let encoded = rows
            .next()
            .await
            .map_err(|_| FeedbackRuntimeError::Store)?
            .map(|row| row.get::<String>(0))
            .transpose()
            .map_err(|_| FeedbackRuntimeError::Store)?;
        drop(rows);
        let mut publications = encoded
            .as_deref()
            .map(decode_ledger)
            .transpose()?
            .unwrap_or_default();
        if publications
            .iter()
            .any(|stored| stored.dedupe_key == publication.dedupe_key)
        {
            transaction
                .rollback()
                .await
                .map_err(|_| FeedbackRuntimeError::Store)?;
            return Ok(false);
        }
        if publications.len() >= MAX_STORED_PUBLICATIONS {
            transaction
                .rollback()
                .await
                .map_err(|_| FeedbackRuntimeError::Store)?;
            return Err(FeedbackRuntimeError::Store);
        }
        publications.push(publication);
        publications.sort_by(|left, right| {
            left.authority
                .revalidated_at
                .cmp(&right.authority.revalidated_at)
                .then_with(|| left.result.result_id.cmp(&right.result.result_id))
        });
        let encoded = serde_json::to_string(&StoredPublicationLedgerV1 {
            schema_version: PUBLICATION_LEDGER_SCHEMA_VERSION,
            publications,
        })
        .map_err(|_| FeedbackRuntimeError::Corrupt)?;
        if encoded.len() > MAX_PUBLICATION_LEDGER_BYTES {
            transaction
                .rollback()
                .await
                .map_err(|_| FeedbackRuntimeError::Store)?;
            return Err(FeedbackRuntimeError::Store);
        }
        self.database
            .set_metadata_unguarded(&transaction, PUBLICATION_LEDGER_METADATA_KEY, &encoded)
            .await
            .map_err(|_| FeedbackRuntimeError::Store)?;
        transaction
            .commit()
            .await
            .map_err(|_| FeedbackRuntimeError::Store)?;
        Ok(true)
    }

    fn finding_view(
        &self,
        context: &RequestContext,
        publication: &FeedbackCompletedPublicationV1,
        finding: &FeedbackFindingV1,
        observed_at: UtcMicros,
    ) -> Result<FeedbackFindingReadV1, FeedbackRuntimeError> {
        let get_handle = store_request_handle(
            &self.project_root,
            request_record(
                context,
                FeedbackReadRequestV1::Get(FeedbackGetRequestV1 {
                    finding_id: finding.finding_id.clone(),
                }),
            ),
            observed_at,
        )?;
        let expand_handle = finding
            .retrieval_anchor_id
            .as_ref()
            .map(|anchor| {
                let page = PageRequest::first(DEFAULT_EXPANSION_PAGE_SIZE)?;
                store_request_handle(
                    &self.project_root,
                    request_record(
                        context,
                        FeedbackReadRequestV1::Expand(FeedbackExpandRequestV1 {
                            finding_id: finding.finding_id.clone(),
                            expansion: tracedecay_application::AnchorExpandRequest {
                                anchor: anchor.clone(),
                                meta: RetrievalRequestMeta::current(
                                    page,
                                    ResultProjection::ReferencesOnly,
                                    RetrievalOrder::StableIdentity,
                                ),
                            },
                        }),
                    ),
                    observed_at,
                )
            })
            .transpose()?;
        Ok(FeedbackFindingReadV1 {
            result_id: publication.result.result_id.clone(),
            cycle_id: publication.result.cycle_id.clone(),
            scope: publication.result.scope.clone(),
            finding: finding.clone(),
            get_handle: OpaqueCursor::new(get_handle)?,
            expand_handle: expand_handle.map(OpaqueCursor::new).transpose()?,
        })
    }

    fn list_start(
        &self,
        context: &RequestContext,
        request: &FeedbackListRequestV1,
        records: &[(FeedbackFindingId, (usize, FeedbackFindingV1))],
        observed_at: UtcMicros,
    ) -> Result<usize, FeedbackRuntimeError> {
        let Some(cursor) = request.page.cursor.as_ref() else {
            return Ok(0);
        };
        let stored = load_handle_content::<StoredFeedbackRequestV1>(
            &self.project_root,
            cursor.as_str(),
            observed_at,
        )
        .map_err(|_| FeedbackRuntimeError::Handle)?;
        let FeedbackReadRequestV1::List(stored_request) = &stored.request else {
            return Err(FeedbackRuntimeError::Handle);
        };
        let Some(after_finding_id) = stored.after_finding_id.as_ref() else {
            return Err(FeedbackRuntimeError::Handle);
        };
        if stored.schema_version != REQUEST_HANDLE_SCHEMA_VERSION
            || stored.operation != FeedbackReadOperationV1::List
            || stored.scope_digest != context.scope().scope_digest
            || stored_request.head_commit_id != request.head_commit_id
            || stored_request.page.page_size != request.page.page_size
            || observed_at >= stored.expires_at
        {
            return Err(FeedbackRuntimeError::Handle);
        }
        Ok(records.partition_point(|(finding_id, _)| finding_id <= after_finding_id))
    }

    fn next_list_handle(
        &self,
        context: &RequestContext,
        request: &FeedbackListRequestV1,
        after_finding_id: &FeedbackFindingId,
        observed_at: UtcMicros,
    ) -> Result<(OpaqueCursor, UtcMicros), FeedbackRuntimeError> {
        let expires_at = context.deadline().expires_at;
        let mut next_request = request_record(
            context,
            FeedbackReadRequestV1::List(FeedbackListRequestV1 {
                head_commit_id: request.head_commit_id.clone(),
                page: PageRequest::first(request.page.page_size)?,
            }),
        );
        next_request.after_finding_id = Some(after_finding_id.clone());
        let next_handle = store_request_handle(&self.project_root, next_request, observed_at)?;
        Ok((OpaqueCursor::new(next_handle)?, expires_at))
    }
}

/// Read the canonical durable Plan-26 observation projection from an already
/// admitted project database. Doctor uses this same projection rather than
/// deriving a second telemetry model.
pub async fn plan26_feedback_observation_read_model(
    database: &Database,
) -> Result<FeedbackObservationReadModelV1, FeedbackRuntimeError> {
    let ledger = match database
        .get_metadata(OBSERVATION_LEDGER_METADATA_KEY)
        .await
        .map_err(|_| FeedbackRuntimeError::Store)?
    {
        Some(encoded) if encoded.len() <= MAX_OBSERVATION_LEDGER_BYTES => {
            decode_observation_ledger(&encoded)?
        }
        Some(_) => return Err(FeedbackRuntimeError::Corrupt),
        None => StoredFeedbackObservationLedgerV1 {
            schema_version: OBSERVATION_LEDGER_SCHEMA_VERSION,
            observations: Vec::new(),
            retention_dropped: 0,
            producer_boots: Vec::new(),
            retained_incomplete_boots: 0,
        },
    };
    project_observation_ledger(&ledger)
}

fn project_observation_ledger(
    ledger: &StoredFeedbackObservationLedgerV1,
) -> Result<FeedbackObservationReadModelV1, FeedbackRuntimeError> {
    let latest_boot = ledger.producer_boots.last().map(|boot| &boot.boot_id);
    let incomplete_boots = ledger
        .producer_boots
        .iter()
        .filter(|boot| !boot.terminal && Some(&boot.boot_id) != latest_boot)
        .count()
        .try_into()
        .unwrap_or(u64::MAX)
        .saturating_add(ledger.retained_incomplete_boots);
    let mut model = FeedbackObservationReadModelV1::project_with_accounting(
        &ledger.observations,
        ledger.retention_dropped,
        incomplete_boots,
    )
    .ok_or(FeedbackRuntimeError::Corrupt)?;
    if let Some(boot) = ledger.producer_boots.last() {
        model.watermark.producer_boot_id = Some(boot.boot_id.clone());
        model.watermark.producer_sequence = (boot.last_sequence > 0).then_some(boot.last_sequence);
        model.watermark.observed_through = boot.last_observed_at;
        if ledger.observations.is_empty() && ledger.retention_dropped == 0 && incomplete_boots == 0
        {
            model.coverage = super::observations::Plan26CoverageV1::Known;
        }
    }
    Ok(model)
}

fn request_record(
    context: &RequestContext,
    request: FeedbackReadRequestV1,
) -> StoredFeedbackRequestV1 {
    StoredFeedbackRequestV1 {
        schema_version: REQUEST_HANDLE_SCHEMA_VERSION,
        operation: request.operation(),
        request,
        request_id: context.request_id().as_str().to_owned(),
        scope_digest: context.scope().scope_digest.clone(),
        issued_at: context.grant().issued_at,
        expires_at: context.deadline().expires_at,
        after_finding_id: None,
    }
}

fn store_request_handle(
    project_root: &Path,
    record: StoredFeedbackRequestV1,
    observed_at: UtcMicros,
) -> Result<String, FeedbackRuntimeError> {
    let content = serde_json::to_string(&record).map_err(|_| FeedbackRuntimeError::Corrupt)?;
    let stored = store_response_handle(project_root, &content, micros_to_seconds(observed_at))
        .map_err(|_| FeedbackRuntimeError::Handle)?;
    Ok(stored.handle)
}

fn load_handle_content<T>(
    project_root: &Path,
    handle: &str,
    observed_at: UtcMicros,
) -> Result<T, FeedbackReadRequestResolutionV1>
where
    T: for<'de> Deserialize<'de>,
{
    if !is_valid_response_handle(handle) {
        return Err(FeedbackReadRequestResolutionV1::NotFoundOrNotAuthorized);
    }
    match retrieve_response_handle(project_root, handle, micros_to_seconds(observed_at))
        .map_err(|_| FeedbackReadRequestResolutionV1::Unavailable)?
    {
        ResponseHandleLookup::Found(record) => serde_json::from_str(&record.content)
            .map_err(|_| FeedbackReadRequestResolutionV1::NotFoundOrNotAuthorized),
        ResponseHandleLookup::Missing | ResponseHandleLookup::Expired { .. } => {
            Err(FeedbackReadRequestResolutionV1::NotFoundOrNotAuthorized)
        }
    }
}

fn decode_ledger(
    encoded: &str,
) -> Result<Vec<FeedbackCompletedPublicationV1>, FeedbackRuntimeError> {
    let ledger: StoredPublicationLedgerV1 =
        serde_json::from_str(encoded).map_err(|_| FeedbackRuntimeError::Corrupt)?;
    if ledger.schema_version != PUBLICATION_LEDGER_SCHEMA_VERSION
        || ledger.publications.len() > MAX_STORED_PUBLICATIONS
        || ledger
            .publications
            .iter()
            .any(|publication| publication.validate().is_err())
    {
        return Err(FeedbackRuntimeError::Corrupt);
    }
    Ok(ledger.publications)
}

fn interruption_outcome(
    context: &RequestContext,
    observed_at: UtcMicros,
) -> Plan26FeedbackOutcomeV1 {
    match context.admission_at(observed_at) {
        RequestAdmission::Cancelled => Plan26FeedbackOutcomeV1::Cancelled,
        RequestAdmission::TimedOut => Plan26FeedbackOutcomeV1::TimedOut,
        RequestAdmission::Admitted => Plan26FeedbackOutcomeV1::Unavailable,
    }
}

async fn persist_feedback_observation(
    database: &Database,
    envelope: FeedbackObservationEnvelopeV1,
) -> Result<(), FeedbackRuntimeError> {
    if envelope.validate().is_none() {
        return Err(FeedbackRuntimeError::Corrupt);
    }
    let transaction = database
        .begin_write_transaction("record plan26 feedback observation")
        .await
        .map_err(|_| FeedbackRuntimeError::Store)?;
    let mut ledger = load_observation_ledger(&transaction).await?;
    if ledger
        .observations
        .iter()
        .any(|stored| stored.idempotency_key == envelope.idempotency_key)
    {
        transaction
            .rollback()
            .await
            .map_err(|_| FeedbackRuntimeError::Store)?;
        return Ok(());
    }
    let boot_id = envelope
        .producer_boot_id
        .clone()
        .ok_or(FeedbackRuntimeError::Corrupt)?;
    let producer_sequence = envelope
        .producer_sequence
        .ok_or(FeedbackRuntimeError::Corrupt)?;
    let terminal = matches!(
        envelope.source_event.as_ref(),
        Some(Plan26FeedbackSourceEventV1::TelemetryDropObserved { terminal: true, .. })
    );
    match ledger
        .producer_boots
        .iter_mut()
        .find(|boot| boot.boot_id == boot_id)
    {
        Some(boot) => {
            if producer_sequence <= boot.last_sequence {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| FeedbackRuntimeError::Store)?;
                return Err(FeedbackRuntimeError::Corrupt);
            }
            boot.last_sequence = producer_sequence;
            boot.terminal |= terminal;
            boot.last_observed_at = Some(
                boot.last_observed_at
                    .map_or(envelope.observed_at, |observed_at| {
                        observed_at.max(envelope.observed_at)
                    }),
            );
        }
        None => ledger.producer_boots.push(StoredFeedbackProducerBootV1 {
            boot_id,
            last_sequence: producer_sequence,
            terminal,
            last_observed_at: Some(envelope.observed_at),
        }),
    }
    ledger.observations.push(envelope);
    ledger.schema_version = OBSERVATION_LEDGER_SCHEMA_VERSION;
    let encoded = encode_bounded_observation_ledger(
        &mut ledger,
        MAX_STORED_OBSERVATIONS,
        MAX_STORED_OBSERVATION_BOOTS,
        MAX_OBSERVATION_LEDGER_BYTES,
    )?;
    database
        .set_metadata_unguarded(&transaction, OBSERVATION_LEDGER_METADATA_KEY, &encoded)
        .await
        .map_err(|_| FeedbackRuntimeError::Store)?;
    transaction
        .commit()
        .await
        .map_err(|_| FeedbackRuntimeError::Store)
}

async fn persist_feedback_producer_boot(
    database: &Database,
    boot_id: ManifestDigest,
) -> Result<(), FeedbackRuntimeError> {
    boot_id
        .validate()
        .map_err(|_| FeedbackRuntimeError::Corrupt)?;
    let transaction = database
        .begin_write_transaction("record plan26 feedback producer boot")
        .await
        .map_err(|_| FeedbackRuntimeError::Store)?;
    let mut ledger = load_observation_ledger(&transaction).await?;
    if ledger
        .producer_boots
        .iter()
        .any(|boot| boot.boot_id == boot_id)
    {
        transaction
            .rollback()
            .await
            .map_err(|_| FeedbackRuntimeError::Store)?;
        return Ok(());
    }
    ledger.producer_boots.push(StoredFeedbackProducerBootV1 {
        boot_id,
        last_sequence: 0,
        terminal: false,
        last_observed_at: None,
    });
    ledger.schema_version = OBSERVATION_LEDGER_SCHEMA_VERSION;
    let encoded = encode_bounded_observation_ledger(
        &mut ledger,
        MAX_STORED_OBSERVATIONS,
        MAX_STORED_OBSERVATION_BOOTS,
        MAX_OBSERVATION_LEDGER_BYTES,
    )?;
    database
        .set_metadata_unguarded(&transaction, OBSERVATION_LEDGER_METADATA_KEY, &encoded)
        .await
        .map_err(|_| FeedbackRuntimeError::Store)?;
    transaction
        .commit()
        .await
        .map_err(|_| FeedbackRuntimeError::Store)
}

async fn load_observation_ledger(
    transaction: &DatabaseWriteTransaction<'_>,
) -> Result<StoredFeedbackObservationLedgerV1, FeedbackRuntimeError> {
    let mut rows = transaction
        .query_engine(
            "SELECT value FROM metadata WHERE key = ?1",
            params![OBSERVATION_LEDGER_METADATA_KEY],
        )
        .await
        .map_err(|_| FeedbackRuntimeError::Store)?;
    let encoded = rows
        .next()
        .await
        .map_err(|_| FeedbackRuntimeError::Store)?
        .map(|row| row.get::<String>(0))
        .transpose()
        .map_err(|_| FeedbackRuntimeError::Store)?;
    drop(rows);
    encoded
        .as_deref()
        .map(decode_observation_ledger)
        .transpose()
        .map(|ledger| {
            ledger.unwrap_or(StoredFeedbackObservationLedgerV1 {
                schema_version: OBSERVATION_LEDGER_SCHEMA_VERSION,
                observations: Vec::new(),
                retention_dropped: 0,
                producer_boots: Vec::new(),
                retained_incomplete_boots: 0,
            })
        })
}

fn decode_observation_ledger(
    encoded: &str,
) -> Result<StoredFeedbackObservationLedgerV1, FeedbackRuntimeError> {
    let ledger: StoredFeedbackObservationLedgerV1 =
        serde_json::from_str(encoded).map_err(|_| FeedbackRuntimeError::Corrupt)?;
    if !matches!(
        ledger.schema_version,
        1 | 2 | OBSERVATION_LEDGER_SCHEMA_VERSION
    ) || ledger.observations.len() > MAX_STORED_OBSERVATIONS
        || ledger
            .observations
            .iter()
            .any(|observation| observation.validate().is_none())
        || ledger.producer_boots.len() > MAX_STORED_OBSERVATION_BOOTS
        || ledger
            .producer_boots
            .iter()
            .enumerate()
            .any(|(index, boot)| {
                boot.boot_id.validate().is_err()
                    || ledger.producer_boots[..index]
                        .iter()
                        .any(|prior| prior.boot_id == boot.boot_id)
            })
    {
        return Err(FeedbackRuntimeError::Corrupt);
    }
    Ok(ledger)
}

fn encode_bounded_observation_ledger(
    ledger: &mut StoredFeedbackObservationLedgerV1,
    max_observations: usize,
    max_boots: usize,
    max_bytes: usize,
) -> Result<String, FeedbackRuntimeError> {
    while ledger.observations.len() > max_observations {
        ledger.observations.remove(0);
        ledger.retention_dropped = ledger.retention_dropped.saturating_add(1);
    }
    while ledger.producer_boots.len() > max_boots {
        retain_removed_boot_accounting(ledger, 0);
    }
    loop {
        let encoded =
            serde_json::to_string(ledger).map_err(|_| FeedbackRuntimeError::Corrupt)?;
        if encoded.len() <= max_bytes {
            return Ok(encoded);
        }
        if ledger.observations.len() > 1 {
            ledger.observations.remove(0);
            ledger.retention_dropped = ledger.retention_dropped.saturating_add(1);
        } else if ledger.producer_boots.len() > 1 {
            retain_removed_boot_accounting(ledger, 0);
        } else {
            return Err(FeedbackRuntimeError::Store);
        }
    }
}

fn retain_removed_boot_accounting(ledger: &mut StoredFeedbackObservationLedgerV1, index: usize) {
    let boot = ledger.producer_boots.remove(index);
    if !boot.terminal {
        ledger.retained_incomplete_boots = ledger.retained_incomplete_boots.saturating_add(1);
    }
}

fn saturating_increment(counter: &AtomicU64) {
    saturating_add(counter, 1);
}

fn saturating_add(counter: &AtomicU64, increment: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
        Some(count.saturating_add(increment))
    });
}

fn apply_pending_worker_drops(
    envelope: &mut FeedbackObservationEnvelopeV1,
    dropped_count: &AtomicU64,
) -> u64 {
    let pending = dropped_count.swap(0, Ordering::Relaxed);
    envelope.delivery.dropped = envelope.delivery.dropped.saturating_add(pending);
    if envelope.delivery.dropped > 0 {
        envelope.delivery.coverage = super::observations::Plan26CoverageV1::Partial;
    }
    envelope.delivery.dropped
}

fn publication_matches_context(
    publication: &FeedbackCompletedPublicationV1,
    context: &RequestContext,
) -> bool {
    let scope = context.scope();
    publication.validate().is_ok()
        && publication.authorized_scope == *scope
        && publication.result.scope.project_id == scope.project_id
        && publication.result.scope.repository_id == scope.repository_id
        && publication.result.scope.worktree_id == scope.worktree_id
        && scope.reference.as_ref().is_some_and(|reference| {
            reference.as_str() == publication.result.scope.branch_ref.as_str()
        })
}

fn publication_order(
    left: &FeedbackCompletedPublicationV1,
    right: &FeedbackCompletedPublicationV1,
) -> std::cmp::Ordering {
    left.authority
        .revalidated_at
        .cmp(&right.authority.revalidated_at)
        .then_with(|| left.result.result_id.cmp(&right.result.result_id))
}

fn latest_finding<'a>(
    publications: &'a [FeedbackCompletedPublicationV1],
    finding_id: &FeedbackFindingId,
) -> Option<(&'a FeedbackCompletedPublicationV1, &'a FeedbackFindingV1)> {
    publications
        .iter()
        .filter_map(|publication| {
            publication
                .result
                .findings
                .iter()
                .find(|finding| &finding.finding_id == finding_id)
                .map(|finding| (publication, finding))
        })
        .max_by(|(left, _), (right, _)| publication_order(left, right))
}

fn operation_ids(operation: FeedbackReadOperationV1) -> (&'static str, &'static str) {
    match operation {
        FeedbackReadOperationV1::Diagnostics => (
            FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
            FEEDBACK_DIAGNOSTICS_USE_CASE_ID_V1,
        ),
        FeedbackReadOperationV1::Get => {
            (FEEDBACK_GET_CAPABILITY_ID_V1, FEEDBACK_GET_USE_CASE_ID_V1)
        }
        FeedbackReadOperationV1::Expand => (
            FEEDBACK_EXPAND_CAPABILITY_ID_V1,
            FEEDBACK_EXPAND_USE_CASE_ID_V1,
        ),
        FeedbackReadOperationV1::List => {
            (FEEDBACK_LIST_CAPABILITY_ID_V1, FEEDBACK_LIST_USE_CASE_ID_V1)
        }
    }
}

fn validate_request(request: &FeedbackReadRequestV1) -> Result<(), ApplicationContractError> {
    match request {
        FeedbackReadRequestV1::Diagnostics(request) => request.validate(),
        FeedbackReadRequestV1::Get(request) => request.validate(),
        FeedbackReadRequestV1::Expand(request) => request.validate(),
        FeedbackReadRequestV1::List(request) => request.validate(),
    }
}

fn micros_to_seconds(micros: UtcMicros) -> i64 {
    micros.0.div_euclid(1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_application::{
        CancellationContext, CapabilityGrantSnapshot, Deadline, RequestId,
    };
    use tracedecay_domain::configuration::{
        AuthorityRef, ConfigurationRevisionId, ScopeSourceBinding, SourceBindingId, SourceKindV1,
    };
    use tracedecay_domain::{
        LocatorDigest, ProjectId, RefId, RepositoryId, WorktreeId, canonical_sha256,
    };

    fn id<T: TryFrom<String>>(value: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn scope() -> ResolvedScope {
        ResolvedScope::new(
            id::<ProjectId>("project.feedback-reader.fixture"),
            id::<RepositoryId>("repository.feedback-reader.fixture"),
            id::<WorktreeId>("worktree.feedback-reader.fixture"),
            Some(id::<RefId>("refs/heads/main")),
        )
        .unwrap()
    }

    fn context(operation: &ApplicationOperation, actor: &str) -> RequestContext {
        let scope = scope();
        let requester = id::<ActorId>(actor);
        let grant = CapabilityGrantSnapshot::new(
            id("grant.feedback-reader.fixture"),
            1,
            canonical_sha256(&"feedback-reader-grant").unwrap(),
            id("actor.feedback-reader.issuer"),
            UtcMicros(1),
            UtcMicros(100),
            scope.clone(),
            [operation.capability_id().clone()].into_iter().collect(),
            [operation.use_case_id().clone()].into_iter().collect(),
            DisclosureClass::Evidence,
        )
        .unwrap();
        RequestContext::new(
            requester,
            scope,
            grant,
            RequestId::new(format!("request.feedback-reader.{actor}")).unwrap(),
            Deadline::new(UtcMicros(100)).unwrap(),
            CancellationContext::active(format!("cancel.feedback-reader.{actor}")).unwrap(),
        )
        .unwrap()
    }

    fn source_access(
        operation: &ApplicationOperation,
        context: &RequestContext,
    ) -> ProjectSourceAccessSnapshot {
        let locator = LocatorDigest::new(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();
        ProjectSourceAccessSnapshot {
            scope: context.scope().clone(),
            requester: context.actor().clone(),
            binding: ScopeSourceBinding::new(
                SourceBindingId::new("binding.feedback-reader.fixture").unwrap(),
                SourceKindV1::Cursor,
                locator,
                AuthorityRef::Project(context.scope().project_id.clone()),
            )
            .unwrap(),
            configuration_revision: ConfigurationRevisionId::new(
                "revision.feedback-reader.fixture",
            )
            .unwrap(),
            configuration_digest: canonical_sha256(&"feedback-reader-configuration").unwrap(),
            configuration_provenance_digest: canonical_sha256(
                &"feedback-reader-configuration-provenance",
            )
            .unwrap(),
            effective_capabilities: [operation.capability_id().clone()].into_iter().collect(),
            grant_expires_at: UtcMicros(100),
        }
    }

    #[test]
    fn latest_publication_route_authorization_is_exact_and_fails_closed() {
        let operation = feedback_surface_operation("feedback_list")
            .unwrap()
            .unwrap();
        let admitted = context(&operation, "actor.feedback-reader.requester");
        let authorization = ProjectFeedbackRouteAuthorization {
            access: source_access(&operation, &admitted),
        };
        assert!(authorization.allows(&admitted, &operation, UtcMicros(10)));

        let wrong_actor = context(&operation, "actor.feedback-reader.other");
        assert!(!authorization.allows(&wrong_actor, &operation, UtcMicros(10)));
        assert!(!authorization.allows(&admitted, &operation, UtcMicros(100)));
    }

    fn source_envelope(boot_id: &ManifestDigest, sequence: u64) -> FeedbackObservationEnvelopeV1 {
        super::super::observations::plan26_feedback_source_event_envelope_for_subject(
            boot_id.clone(),
            UtcMicros(sequence.try_into().unwrap()),
            Plan26FeedbackSourceEventV1::ArgumentRejected {
                operation: super::super::observations::Plan26FeedbackOperationV1::FeedbackGet,
                outcome: Plan26FeedbackOutcomeV1::Rejected,
            },
        )
        .unwrap()
    }

    fn sequenced_source_envelope(
        boot_id: &ManifestDigest,
        sequence: u64,
    ) -> FeedbackObservationEnvelopeV1 {
        let mut envelope = source_envelope(boot_id, sequence);
        envelope
            .assign_delivery(
                boot_id.clone(),
                sequence,
                super::super::observations::FeedbackObservationDeliveryV1::delivered(0),
            )
            .unwrap();
        envelope
    }

    #[test]
    fn durable_queue_carries_drops_and_shutdown_uses_reserved_control_lane() {
        let boot_id = canonical_sha256(&"feedback-concrete-boot").unwrap();
        let (sender, mut receiver) = mpsc::channel(1);
        let (control_sender, mut control_receiver) = mpsc::channel(1);
        let sink = ProjectFeedbackObservationSinkV1 {
            sender,
            control_sender,
            boot_id: boot_id.clone(),
            next_sequence: AtomicU64::new(0),
            dropped_count: Arc::new(AtomicU64::new(0)),
            admission: Mutex::new(()),
        };

        assert_eq!(
            sink.enqueue_durable_feedback_observation(source_envelope(&boot_id, 1)),
            FeedbackObservationSinkOutcome::Enqueued
        );
        assert_eq!(
            sink.enqueue_durable_feedback_observation(source_envelope(&boot_id, 2)),
            FeedbackObservationSinkOutcome::Dropped
        );
        let first = receiver.try_recv().unwrap();
        assert_eq!(first.producer_sequence, Some(1));
        assert_eq!(
            sink.enqueue_durable_feedback_observation(source_envelope(&boot_id, 3)),
            FeedbackObservationSinkOutcome::Enqueued
        );
        let recovered = receiver.try_recv().unwrap();
        assert_eq!(recovered.producer_sequence, Some(3));
        assert_eq!(recovered.delivery.dropped, 1);

        drop(sink);
        let terminal = control_receiver.try_recv().unwrap();
        assert!(matches!(
            terminal.source_event,
            Some(Plan26FeedbackSourceEventV1::TelemetryDropObserved {
                dropped_count: 0,
                terminal: true,
                ..
            })
        ));
    }

    #[test]
    fn durable_ledger_retention_keeps_newest_and_reports_omission() {
        let boot_id = canonical_sha256(&"feedback-retention-boot").unwrap();
        let first = sequenced_source_envelope(&boot_id, 1);
        let second = sequenced_source_envelope(&boot_id, 2);
        let expected = second.idempotency_key.clone();
        let last_observed_at = second.observed_at;
        let mut ledger = StoredFeedbackObservationLedgerV1 {
            schema_version: OBSERVATION_LEDGER_SCHEMA_VERSION,
            observations: vec![first, second],
            retention_dropped: 0,
            producer_boots: vec![StoredFeedbackProducerBootV1 {
                boot_id,
                last_sequence: 2,
                terminal: false,
                last_observed_at: Some(last_observed_at),
            }],
            retained_incomplete_boots: 0,
        };

        let encoded = encode_bounded_observation_ledger(&mut ledger, 1, 1, usize::MAX).unwrap();
        let decoded = decode_observation_ledger(&encoded).unwrap();
        assert_eq!(decoded.observations.len(), 1);
        assert_eq!(decoded.observations[0].idempotency_key, expected);
        assert_eq!(decoded.retention_dropped, 1);
    }

    #[test]
    fn retained_incomplete_boot_and_latest_watermark_survive_bounded_history() {
        let old_boot = canonical_sha256(&"feedback-old-incomplete-boot").unwrap();
        let latest_boot = canonical_sha256(&"feedback-latest-boot").unwrap();
        let observed_at = UtcMicros(42);
        let mut ledger = StoredFeedbackObservationLedgerV1 {
            schema_version: OBSERVATION_LEDGER_SCHEMA_VERSION,
            observations: Vec::new(),
            retention_dropped: 1,
            producer_boots: vec![
                StoredFeedbackProducerBootV1 {
                    boot_id: old_boot,
                    last_sequence: 0,
                    terminal: false,
                    last_observed_at: None,
                },
                StoredFeedbackProducerBootV1 {
                    boot_id: latest_boot.clone(),
                    last_sequence: 7,
                    terminal: true,
                    last_observed_at: Some(observed_at),
                },
            ],
            retained_incomplete_boots: 0,
        };

        let encoded = encode_bounded_observation_ledger(&mut ledger, 0, 1, usize::MAX).unwrap();
        let decoded = decode_observation_ledger(&encoded).unwrap();
        assert_eq!(decoded.retained_incomplete_boots, 1);
        let model = project_observation_ledger(&decoded).unwrap();
        assert_eq!(
            model.coverage,
            super::super::observations::Plan26CoverageV1::Unknown
        );
        assert_eq!(model.denominators.retention_dropped, 1);
        assert_eq!(model.denominators.incomplete_boots, 1);
        assert_eq!(model.watermark.producer_boot_id, Some(latest_boot));
        assert_eq!(model.watermark.producer_sequence, Some(7));
        assert_eq!(model.watermark.observed_through, Some(observed_at));
    }

    #[test]
    fn worker_persistence_drops_are_carried_into_the_next_envelope() {
        let boot_id = canonical_sha256(&"feedback-worker-drop-boot").unwrap();
        let mut envelope = sequenced_source_envelope(&boot_id, 1);
        envelope.delivery = super::super::observations::FeedbackObservationDeliveryV1::delivered(2);
        let dropped_count = AtomicU64::new(3);

        assert_eq!(apply_pending_worker_drops(&mut envelope, &dropped_count), 5);
        assert_eq!(dropped_count.load(Ordering::Relaxed), 0);
        assert_eq!(
            envelope.delivery.coverage,
            super::super::observations::Plan26CoverageV1::Partial
        );

        saturating_add(&dropped_count, envelope.delivery.dropped.saturating_add(1));
        assert_eq!(dropped_count.load(Ordering::Relaxed), 6);
    }
}
