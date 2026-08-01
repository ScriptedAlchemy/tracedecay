//! Daemon-owned retained lifecycle events for application operations.
//!
//! This authority is intentionally memory-only. A daemon restart invalidates
//! every operation frontier; operation-specific durable journals remain owned
//! by their existing application/store contracts.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, broadcast, watch};
use tokio_stream::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tracedecay_application::{
    ApplicationProblem, ApplicationProblemEnvelope, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, InvocationTarget, LegalAction,
    OpaqueCursor, OperationReceipt, OperationTermination, PageRequest, ProblemOwningLayer,
    RequestContext, RequestId, ResolvedScope, ResultContractRef, ResumeToken, RetryDirective,
    SafeDiagnostic, StreamEvent, StreamEventKind, StreamFrontier, StreamGap, StreamTermination,
};
use tracedecay_domain::{
    ActorId, CodeGenerationId, CommitId, ContentDigest, ProjectId, RetrievalGrainV1,
    SessionCursorKeyIdV1, SessionCursorVersionV1, SessionId, SignedCursorKeyRefV1, TemporalModeV1,
    UtcMicros, canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, SchemaId, UseCaseId};

use tracedecay_temporal_query::cursor::{CursorError, StableSortKey, encode_cursor, verify_cursor};
use tracedecay_temporal_query::ports::{
    BindingDigest, InMemoryCursorAuthenticator, KernelVersions, TemporalExecutionSnapshot,
    TemporalSnapshotRequest, TemporalWatermarks,
};

use tracedecay_temporal_query::resolution::ValidatedAuthorization;

const RESUME_KEY_RANDOM_BYTES: usize = 16;
const RESUME_KEY_MATERIAL_BYTES: usize = 32;

/// Every operation-event problem shares this contract, so its schema identity
/// is validated once per process instead of on each envelope conversion.
static OPERATION_EVENT_PROBLEM_CONTRACT: LazyLock<ResultContractRef> = LazyLock::new(|| {
    ResultContractRef::new(
        SchemaId::new("schema.tracedecay.operation-event.problem.v1")
            .unwrap_or_else(|_| panic!("the operation-event problem schema id is static")),
        1,
    )
    .unwrap_or_else(|_| panic!("the operation-event problem contract is static"))
});

fn current_micros_for_cancellation() -> UtcMicros {
    UtcMicros(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_micros()).ok())
            .unwrap_or(i64::MAX),
    )
}

/// Stable operation identity. The originating authorized request owns the
/// identity; paths, labels, and client-selected payloads never participate.
#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(transparent)]
pub struct OperationId(RequestId);

impl OperationId {
    pub fn from_request(request_id: RequestId) -> Self {
        Self(request_id)
    }

    pub fn request_id(&self) -> &RequestId {
        &self.0
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// Caller-owned controls used to reconstitute an operation request context.
pub struct OperationRequestControls<'a> {
    request_id: RequestId,
    deadline: Deadline,
    cancellation: CancellationContext,
    observed_at: UtcMicros,
    resume_token: Option<&'a ResumeToken>,
}

impl<'a> OperationRequestControls<'a> {
    #[must_use]
    pub fn new(
        request_id: RequestId,
        deadline: Deadline,
        cancellation: CancellationContext,
        observed_at: UtcMicros,
        resume_token: Option<&'a ResumeToken>,
    ) -> Self {
        Self {
            request_id,
            deadline,
            cancellation,
            observed_at,
            resume_token,
        }
    }
}

/// Closed operation names prevent lifecycle metadata from becoming an
/// arbitrary payload side channel.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    GitPreview,
    GitApply,
    FeedbackDiagnostics,
    FeedbackGet,
    FeedbackExpand,
    FeedbackList,
    TestRun,
}

/// The only item payload published by the lifecycle stream. Progress, gaps,
/// and terminal receipts use the canonical `StreamEventKind` variants.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum OperationEventItem {
    Accepted {
        operation_id: OperationId,
        originating_request_id: RequestId,
        operation: OperationKind,
        content_class: DisclosureClass,
    },
    TestRunResult {
        test: String,
        passed: bool,
    },
}

pub type OperationEvent = StreamEvent<OperationEventItem>;

static OPERATION_EVENTS: OnceLock<OperationEventAuthority> = OnceLock::new();

pub fn operation_event_authority() -> OperationEventAuthority {
    OPERATION_EVENTS
        .get_or_init(OperationEventAuthority::default)
        .clone()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationBinding {
    operation_id: OperationId,
    originating_request_id: RequestId,
    operation: OperationKind,
    event_disclosure: DisclosureClass,
    authorization: OperationAuthorization,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OperationAuthorization {
    Request {
        actor: ActorId,
        scope: ResolvedScope,
        access_digest: String,
        allowed_capabilities: BTreeSet<CapabilityId>,
        allowed_use_cases: BTreeSet<UseCaseId>,
    },
    ProjectRoot {
        root_uri: String,
        head_commit_id: Option<CommitId>,
        code_generation_id: Option<CodeGenerationId>,
        document_content_digests: BTreeMap<String, ContentDigest>,
        deadline: Deadline,
    },
}

impl OperationBinding {
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }

    pub fn originating_request_id(&self) -> &RequestId {
        &self.originating_request_id
    }

    pub const fn operation(&self) -> OperationKind {
        self.operation
    }

    pub const fn event_disclosure(&self) -> DisclosureClass {
        self.event_disclosure
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperationStreamConfig {
    pub retained_event_capacity: usize,
    pub max_operations: usize,
    pub max_subscribers_per_operation: usize,
}

impl Default for OperationStreamConfig {
    fn default() -> Self {
        Self {
            retained_event_capacity: 256,
            max_operations: 1_024,
            max_subscribers_per_operation: 32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationCancelOutcome {
    Requested,
    AlreadyRequested,
    AlreadyTerminal,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum OperationEventError {
    #[error("operation event authority configuration must use non-zero bounds")]
    InvalidConfiguration,
    #[error("operation request context is invalid: {0}")]
    InvalidContext(String),
    #[error("operation request was not admitted")]
    RequestNotAdmitted,
    #[error("operation identity is already bound")]
    AlreadyBound,
    #[error("operation event authority is saturated")]
    Saturated,
    #[error("operation was not found or the requester is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("operation history frontier expired (the daemon may have restarted)")]
    FrontierExpired,
    #[error("operation resume token expired (the daemon may have restarted)")]
    ResumeExpired,
    #[error("operation resume token authority is unavailable")]
    ResumeUnavailable,
    #[error("requested operation frontier is ahead of the current frontier")]
    InvalidFrontier,
    #[error("operation progress is invalid")]
    InvalidProgress,
    #[error("operation already published a different terminal receipt")]
    TerminalAlreadyPublished,
    #[error("operation terminal receipt is invalid: {0}")]
    InvalidTerminal(String),
    #[error("managed test-run event is invalid")]
    InvalidTestRunEvent,
}

impl OperationEventError {
    /// Converts runtime stream failures into the one canonical application
    /// problem envelope used by every transport.
    pub fn into_problem_envelope(self, request_id: RequestId) -> ApplicationProblemEnvelope {
        let saturated = matches!(self, Self::Saturated);
        let problem = match self {
            Self::NotFoundOrNotAuthorized => {
                ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
            }
            Self::FrontierExpired | Self::ResumeExpired => ApplicationProblem::Stale {
                diagnostic: SafeDiagnostic::new(
                    "operation_event.resume_expired",
                    "The operation-event resume frontier has expired",
                )
                .unwrap_or_else(|_| panic!("operation-event diagnostics are static")),
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![LegalAction::Refresh],
            },
            Self::InvalidFrontier => ApplicationProblem::Conflict {
                diagnostic: SafeDiagnostic::new(
                    "operation_event.invalid_frontier",
                    "The requested operation-event frontier is invalid",
                )
                .unwrap_or_else(|_| panic!("operation-event diagnostics are static")),
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![LegalAction::Refresh],
            },
            Self::RequestNotAdmitted => ApplicationProblem::timed_out_before_admission(),
            Self::Saturated => ApplicationProblem::Saturated {
                diagnostic: SafeDiagnostic::new(
                    "operation_event.saturated",
                    "Operation-event capacity is temporarily saturated",
                )
                .unwrap_or_else(|_| panic!("operation-event diagnostics are static")),
                retry: RetryDirective::AfterDelay,
                legal_actions: vec![LegalAction::Retry],
            },
            // Permanently invalid input: the same request can never succeed, so
            // the client must correct it rather than retry.
            Self::InvalidContext(_)
            | Self::InvalidProgress
            | Self::InvalidTerminal(_)
            | Self::InvalidTestRunEvent => ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic::new(
                    "operation_event.invalid_request",
                    "The operation-event request is invalid",
                )
                .unwrap_or_else(|_| panic!("operation-event diagnostics are static")),
                retry: RetryDirective::Never,
                legal_actions: vec![LegalAction::CorrectRequest],
            },
            // Idempotency facts: the identity or terminal receipt is already
            // published, so the client re-reads current state instead of
            // retrying the same publish.
            Self::AlreadyBound | Self::TerminalAlreadyPublished => ApplicationProblem::Conflict {
                diagnostic: SafeDiagnostic::new(
                    "operation_event.already_published",
                    "The operation-event identity is already published",
                )
                .unwrap_or_else(|_| panic!("operation-event diagnostics are static")),
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![LegalAction::Refresh],
            },
            // A misconfigured authority is a deterministic, process-lifetime
            // failure. It is not the caller's request that is wrong and no
            // amount of retrying will change the outcome.
            Self::InvalidConfiguration => ApplicationProblem::Unsupported {
                diagnostic: SafeDiagnostic::new(
                    "operation_event.unsupported",
                    "The operation-event authority is not configured for this operation",
                )
                .unwrap_or_else(|_| panic!("operation-event diagnostics are static")),
                retry: RetryDirective::Never,
                legal_actions: vec![LegalAction::ContactAdministrator],
            },
            // Genuinely transient: the resume-token authority could not answer.
            Self::ResumeUnavailable => ApplicationProblem::unavailable(
                SafeDiagnostic::new(
                    "operation_event.unavailable",
                    "The operation-event service is unavailable",
                )
                .unwrap_or_else(|_| panic!("operation-event diagnostics are static")),
            ),
        };
        let envelope = ApplicationProblemEnvelope::new(
            OPERATION_EVENT_PROBLEM_CONTRACT.clone(),
            request_id,
            problem,
        )
        .with_owning_layer(ProblemOwningLayer::Runtime);
        if saturated {
            envelope
                .with_retry_after_millis(Some(250))
                .unwrap_or_else(|_| panic!("the operation-event retry delay is bounded"))
        } else {
            envelope
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedTestRunResult {
    pub(crate) test: String,
    pub(crate) passed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedTestRunSnapshot {
    pub(crate) operation_id: OperationId,
    pub(crate) generation: u64,
    pub(crate) source_revision: u64,
    pub(crate) head_commit_id: Option<CommitId>,
    pub(crate) code_generation_id: Option<CodeGenerationId>,
    pub(crate) document_content_digests: BTreeMap<String, ContentDigest>,
    pub(crate) deadline: Deadline,
    pub(crate) results: Vec<ManagedTestRunResult>,
    pub(crate) result_offset: usize,
    pub(crate) available_results: usize,
    pub(crate) next_cursor: Option<OpaqueCursor>,
    pub(crate) completed: u64,
    pub(crate) total: Option<u64>,
    pub(crate) termination: Option<OperationTermination>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedTestRunCurrentScope {
    pub(crate) root_uri: String,
    pub(crate) head_commit_id: Option<CommitId>,
    pub(crate) code_generation_id: Option<CodeGenerationId>,
    pub(crate) document_uri: Option<String>,
    pub(crate) document_content_digest: Option<ContentDigest>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManagedTestRunUnavailableReason {
    FrontierExpired,
    CurrentHeadUnbound,
    CurrentCodeGenerationUnbound,
    RetainedHeadUnbound,
    RetainedCodeGenerationUnbound,
    CurrentDocumentUnbound,
    RetainedDocumentUnbound,
    AuthorityFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManagedTestRunStaleReason {
    SourceIdentity,
    DocumentContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
// Current carries the full snapshot; boxing would ripple through reader match sites.
#[allow(clippy::large_enum_variant)]
pub(crate) enum ManagedTestRunReadOutcome {
    Current(ManagedTestRunSnapshot),
    Stale(ManagedTestRunStaleReason),
    Unavailable(ManagedTestRunUnavailableReason),
}

/// Canonical generation- and content-bound reader for retained managed test
/// runs. Adapters project this result; they do not read the event authority or
/// decide current identity independently.
#[derive(Clone)]
pub(crate) struct CanonicalManagedTestRunReader {
    events: OperationEventAuthority,
}

impl CanonicalManagedTestRunReader {
    pub(crate) fn new(events: OperationEventAuthority) -> Self {
        Self { events }
    }

    pub(crate) async fn latest_current(
        &self,
        current: &ManagedTestRunCurrentScope,
    ) -> ManagedTestRunReadOutcome {
        let snapshot = match self.events.latest_managed_test_run(&current.root_uri).await {
            Ok(snapshot) => snapshot,
            Err(OperationEventError::FrontierExpired) => {
                return ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::FrontierExpired,
                );
            }
            Err(_) => {
                return ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::AuthorityFailure,
                );
            }
        };
        current_managed_test_run(snapshot, current)
    }

    pub(crate) fn try_latest_current(
        &self,
        current: &ManagedTestRunCurrentScope,
    ) -> Option<ManagedTestRunReadOutcome> {
        let snapshot = match self.events.try_latest_managed_test_run(&current.root_uri)? {
            Ok(snapshot) => snapshot,
            Err(OperationEventError::FrontierExpired) => {
                return Some(ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::FrontierExpired,
                ));
            }
            Err(_) => {
                return Some(ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::AuthorityFailure,
                ));
            }
        };
        Some(current_managed_test_run(snapshot, current))
    }

    pub(crate) async fn latest_current_page(
        &self,
        current: &ManagedTestRunCurrentScope,
        page: &PageRequest,
    ) -> ManagedTestRunReadOutcome {
        let snapshot = match self.latest_current(current).await {
            ManagedTestRunReadOutcome::Current(snapshot) => snapshot,
            outcome => return outcome,
        };
        match self.events.page_managed_test_run(snapshot, page).await {
            Ok(snapshot) => ManagedTestRunReadOutcome::Current(snapshot),
            Err(_) => ManagedTestRunReadOutcome::Unavailable(
                ManagedTestRunUnavailableReason::AuthorityFailure,
            ),
        }
    }
}

fn current_managed_test_run(
    snapshot: ManagedTestRunSnapshot,
    current: &ManagedTestRunCurrentScope,
) -> ManagedTestRunReadOutcome {
    let Some(current_head) = current.head_commit_id.as_ref() else {
        return ManagedTestRunReadOutcome::Unavailable(
            ManagedTestRunUnavailableReason::CurrentHeadUnbound,
        );
    };
    let Some(current_generation) = current.code_generation_id.as_ref() else {
        return ManagedTestRunReadOutcome::Unavailable(
            ManagedTestRunUnavailableReason::CurrentCodeGenerationUnbound,
        );
    };
    let Some(retained_head) = snapshot.head_commit_id.as_ref() else {
        return ManagedTestRunReadOutcome::Unavailable(
            ManagedTestRunUnavailableReason::RetainedHeadUnbound,
        );
    };
    let Some(retained_generation) = snapshot.code_generation_id.as_ref() else {
        return ManagedTestRunReadOutcome::Unavailable(
            ManagedTestRunUnavailableReason::RetainedCodeGenerationUnbound,
        );
    };
    if retained_head != current_head || retained_generation != current_generation {
        return ManagedTestRunReadOutcome::Stale(ManagedTestRunStaleReason::SourceIdentity);
    }
    match (
        current.document_uri.as_ref(),
        current.document_content_digest.as_ref(),
    ) {
        (None, None) => {}
        (Some(document_uri), Some(current_digest)) => {
            let Some(retained_digest) =
                snapshot.document_content_digests.get(document_uri.as_str())
            else {
                return ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::RetainedDocumentUnbound,
                );
            };
            if retained_digest != current_digest {
                return ManagedTestRunReadOutcome::Stale(
                    ManagedTestRunStaleReason::DocumentContent,
                );
            }
        }
        _ => {
            return ManagedTestRunReadOutcome::Unavailable(
                ManagedTestRunUnavailableReason::CurrentDocumentUnbound,
            );
        }
    }
    ManagedTestRunReadOutcome::Current(snapshot)
}

#[derive(Clone)]
pub struct OperationEventAuthority {
    inner: Arc<AuthorityInner>,
}

struct AuthorityInner {
    config: OperationStreamConfig,
    resume: OperationResumeAuthority,
    state: Mutex<AuthorityState>,
}

#[derive(Default)]
struct AuthorityState {
    operations: BTreeMap<OperationId, OperationRecord>,
    insertion_order: VecDeque<OperationId>,
}

struct OperationRecord {
    binding: OperationBinding,
    generation: u64,
    resume_token: ResumeToken,
    history: VecDeque<OperationEvent>,
    next_sequence: u64,
    terminal: Option<OperationEvent>,
    live: broadcast::Sender<OperationEvent>,
    frontier: watch::Sender<StreamFrontier>,
    cancellation: watch::Sender<Option<UtcMicros>>,
    subscribers: Arc<AtomicUsize>,
}

struct OperationResumeAuthority {
    key: SignedCursorKeyRefV1,
    authenticator: InMemoryCursorAuthenticator,
    next_generation: AtomicU64,
}

impl OperationResumeAuthority {
    fn new() -> Result<Self, OperationEventError> {
        let mut key_random = [0_u8; RESUME_KEY_RANDOM_BYTES];
        let mut key_material = [0_u8; RESUME_KEY_MATERIAL_BYTES];
        getrandom::getrandom(&mut key_random)
            .map_err(|_| OperationEventError::ResumeUnavailable)?;
        getrandom::getrandom(&mut key_material)
            .map_err(|_| OperationEventError::ResumeUnavailable)?;
        let key = SignedCursorKeyRefV1 {
            key_id: SessionCursorKeyIdV1::new(format!(
                "cursor.operation-stream.{}",
                hex::encode(key_random)
            ))
            .map_err(|_| OperationEventError::ResumeUnavailable)?,
            version: SessionCursorVersionV1::new(1)
                .map_err(|_| OperationEventError::ResumeUnavailable)?,
        };
        let authenticator = InMemoryCursorAuthenticator::new(key.clone(), key_material.to_vec())
            .map_err(|_| OperationEventError::ResumeUnavailable)?;
        Ok(Self {
            key,
            authenticator,
            next_generation: AtomicU64::new(1),
        })
    }

    fn next_generation(&self) -> Result<u64, OperationEventError> {
        self.next_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                (generation < i64::MAX as u64).then_some(generation + 1)
            })
            .map_err(|_| OperationEventError::ResumeUnavailable)
    }

    fn issue(
        &self,
        binding: &OperationBinding,
        generation: u64,
    ) -> Result<ResumeToken, OperationEventError> {
        let snapshot = operation_resume_snapshot(binding, generation, self.key.clone())?;
        let encoded = encode_cursor(
            &snapshot,
            &StableSortKey {
                normalized_score_micros: 0,
                knowledge_at_micros: generation as i64,
                stable_id: binding.operation_id.to_string(),
            },
            &self.authenticator,
        )
        .map_err(|_| OperationEventError::ResumeUnavailable)?;
        ResumeToken::new(encoded).map_err(|_| OperationEventError::ResumeUnavailable)
    }

    fn verify(
        &self,
        token: &ResumeToken,
        record: &OperationRecord,
    ) -> Result<(), OperationEventError> {
        let snapshot =
            operation_resume_snapshot(&record.binding, record.generation, self.key.clone())?;
        let sort_key = verify_cursor(token.as_str(), &snapshot, &self.authenticator)
            .map_err(operation_resume_verification_error)?;
        if sort_key.normalized_score_micros != 0
            || sort_key.knowledge_at_micros != record.generation as i64
            || sort_key.stable_id != record.binding.operation_id.to_string()
        {
            return Err(OperationEventError::NotFoundOrNotAuthorized);
        }
        Ok(())
    }

    fn issue_test_result_cursor(
        &self,
        binding: &OperationBinding,
        generation: u64,
        completed: u64,
        next_offset: usize,
    ) -> Result<OpaqueCursor, OperationEventError> {
        let snapshot = operation_resume_snapshot(binding, generation, self.key.clone())?;
        let encoded = encode_cursor(
            &snapshot,
            &StableSortKey {
                normalized_score_micros: u64::try_from(next_offset)
                    .map_err(|_| OperationEventError::ResumeUnavailable)?,
                knowledge_at_micros: i64::try_from(completed)
                    .map_err(|_| OperationEventError::ResumeUnavailable)?,
                stable_id: binding.operation_id.to_string(),
            },
            &self.authenticator,
        )
        .map_err(|_| OperationEventError::ResumeUnavailable)?;
        OpaqueCursor::new(encoded).map_err(|_| OperationEventError::ResumeUnavailable)
    }

    fn verify_test_result_cursor(
        &self,
        cursor: &OpaqueCursor,
        record: &OperationRecord,
        completed: u64,
    ) -> Result<usize, OperationEventError> {
        let snapshot =
            operation_resume_snapshot(&record.binding, record.generation, self.key.clone())?;
        let sort_key = verify_cursor(cursor.as_str(), &snapshot, &self.authenticator)
            .map_err(operation_resume_verification_error)?;
        if sort_key.stable_id != record.binding.operation_id.to_string()
            || sort_key.knowledge_at_micros
                != i64::try_from(completed).map_err(|_| OperationEventError::ResumeUnavailable)?
        {
            return Err(OperationEventError::NotFoundOrNotAuthorized);
        }
        usize::try_from(sort_key.normalized_score_micros)
            .map_err(|_| OperationEventError::NotFoundOrNotAuthorized)
    }
}

impl Default for OperationEventAuthority {
    fn default() -> Self {
        Self::new(OperationStreamConfig::default())
            .unwrap_or_else(|_| panic!("the default operation event authority is valid"))
    }
}

impl OperationEventAuthority {
    pub fn new(config: OperationStreamConfig) -> Result<Self, OperationEventError> {
        if config.retained_event_capacity == 0
            || config.max_operations == 0
            || config.max_subscribers_per_operation == 0
        {
            return Err(OperationEventError::InvalidConfiguration);
        }
        let resume = OperationResumeAuthority::new()?;
        Ok(Self {
            inner: Arc::new(AuthorityInner {
                config,
                resume,
                state: Mutex::new(AuthorityState::default()),
            }),
        })
    }

    /// Reconstitutes transport controls over the exact authority retained for
    /// an operation. The active-project identity is supplied by the
    /// authenticated HTTP mount; client paths and scope payloads are never
    /// accepted.
    pub async fn resolve_request_context(
        &self,
        operation_id: &OperationId,
        active_project_id: &ProjectId,
        controls: OperationRequestControls<'_>,
    ) -> Result<RequestContext, OperationEventError> {
        self.resolve_invocation_context_inner(operation_id, Some(active_project_id), None, controls)
            .await
    }

    /// Daemon invocation admission resolves authority from the retained
    /// operation and only revalidates a caller-carried exact scope.
    pub async fn resolve_invocation_context(
        &self,
        operation_id: &OperationId,
        target: &InvocationTarget,
        controls: OperationRequestControls<'_>,
    ) -> Result<RequestContext, OperationEventError> {
        self.resolve_invocation_context_inner(operation_id, None, target.resolved(), controls)
            .await
    }

    async fn resolve_invocation_context_inner(
        &self,
        operation_id: &OperationId,
        expected_project_id: Option<&ProjectId>,
        expected_scope: Option<&ResolvedScope>,
        controls: OperationRequestControls<'_>,
    ) -> Result<RequestContext, OperationEventError> {
        let OperationRequestControls {
            request_id,
            deadline,
            cancellation,
            observed_at,
            resume_token,
        } = controls;
        let state = self.inner.state.lock().await;
        let Some(record) = state.operations.get(operation_id) else {
            return Err(if resume_token.is_some() {
                OperationEventError::ResumeExpired
            } else {
                OperationEventError::NotFoundOrNotAuthorized
            });
        };
        let OperationAuthorization::Request {
            actor,
            scope,
            access_digest: _,
            allowed_capabilities,
            allowed_use_cases,
        } = &record.binding.authorization
        else {
            return Err(OperationEventError::NotFoundOrNotAuthorized);
        };
        if expected_project_id.is_some_and(|project_id| scope.project_id != *project_id)
            || expected_scope.is_some_and(|expected| scope != expected)
        {
            return Err(OperationEventError::NotFoundOrNotAuthorized);
        }
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.http.operation-events.v1").map_err(invalid_context)?,
            1,
            canonical_sha256(&(
                "tracedecay.http.operation-events.grant.v1",
                operation_id,
                &request_id,
                scope,
                deadline.expires_at,
            ))
            .map_err(invalid_context)?,
            ActorId::new("actor.tracedecay-daemon").map_err(invalid_context)?,
            observed_at,
            deadline.expires_at,
            scope.clone(),
            allowed_capabilities.clone(),
            allowed_use_cases.clone(),
            record.binding.event_disclosure(),
        )
        .map_err(invalid_context)?;
        let context = RequestContext::new(
            actor.clone(),
            scope.clone(),
            grant,
            request_id,
            deadline,
            cancellation,
        )
        .map_err(invalid_context)?;
        validate_admitted_context(&context, observed_at)?;
        Ok(context)
    }

    /// Registers an admitted operation and publishes its sole accepted event.
    pub async fn begin(
        &self,
        context: &RequestContext,
        operation: OperationKind,
        observed_at: UtcMicros,
    ) -> Result<OperationEmitter, OperationEventError> {
        validate_admitted_context(context, observed_at)?;
        let operation_id = OperationId::from_request(context.request_id().clone());
        let binding = OperationBinding {
            operation_id: operation_id.clone(),
            originating_request_id: context.request_id().clone(),
            operation,
            event_disclosure: DisclosureClass::Metadata,
            authorization: OperationAuthorization::Request {
                actor: context.actor().clone(),
                scope: context.scope().clone(),
                access_digest: context.grant().digest.as_str().to_owned(),
                allowed_capabilities: context.grant().allowed_capabilities.clone(),
                allowed_use_cases: context.grant().allowed_use_cases.clone(),
            },
        };

        let mut state = self.inner.state.lock().await;
        if let Some(record) = state.operations.get(&operation_id) {
            if record.binding != binding {
                return Err(OperationEventError::AlreadyBound);
            }
            return Ok(OperationEmitter {
                authority: self.clone(),
                binding,
                cancellation: record.cancellation.subscribe(),
            });
        }
        self.evict_terminal_if_needed(&mut state)?;
        let generation = self.inner.resume.next_generation()?;
        let resume_token = self.inner.resume.issue(&binding, generation)?;

        let (live, _) = broadcast::channel(self.inner.config.retained_event_capacity);
        let initial_frontier = StreamFrontier {
            next_sequence: 0,
            retained_from_sequence: 0,
            resume_token: Some(resume_token.clone()),
        };
        let (frontier, _) = watch::channel(initial_frontier);
        let (cancellation, cancellation_receiver) = watch::channel(None);
        let accepted = StreamEvent {
            sequence: 0,
            kind: StreamEventKind::Item(OperationEventItem::Accepted {
                operation_id: operation_id.clone(),
                originating_request_id: context.request_id().clone(),
                operation,
                content_class: DisclosureClass::Metadata,
            }),
        };
        let mut history = VecDeque::with_capacity(self.inner.config.retained_event_capacity);
        history.push_back(accepted.clone());
        let record = OperationRecord {
            binding: binding.clone(),
            generation,
            resume_token,
            history,
            next_sequence: 1,
            terminal: None,
            live,
            frontier,
            cancellation,
            subscribers: Arc::new(AtomicUsize::new(0)),
        };
        record.frontier.send_replace(frontier_for(&record));
        let _ = record.live.send(accepted);
        state.insertion_order.push_back(operation_id.clone());
        state.operations.insert(operation_id.clone(), record);

        Ok(OperationEmitter {
            authority: self.clone(),
            binding,
            cancellation: cancellation_receiver,
        })
    }

    /// Starts one trusted project-local managed test run. The caller is the
    /// already-routed project workflow handler, so the retained authorization
    /// key is the canonical admitted root URI rather than client payload.
    pub async fn begin_managed_test_run(
        &self,
        root_uri: String,
        request_id: RequestId,
        head_commit_id: Option<CommitId>,
        code_generation_id: Option<CodeGenerationId>,
        document_content_digests: BTreeMap<String, ContentDigest>,
        deadline: Deadline,
    ) -> Result<OperationEmitter, OperationEventError> {
        if root_uri.len() > 4_096 || !root_uri.starts_with("file:") {
            return Err(OperationEventError::InvalidTestRunEvent);
        }
        let operation_id = OperationId::from_request(request_id.clone());
        let binding = OperationBinding {
            operation_id: operation_id.clone(),
            originating_request_id: request_id.clone(),
            operation: OperationKind::TestRun,
            event_disclosure: DisclosureClass::Metadata,
            authorization: OperationAuthorization::ProjectRoot {
                root_uri,
                head_commit_id,
                code_generation_id,
                document_content_digests,
                deadline,
            },
        };
        let mut state = self.inner.state.lock().await;
        if let Some(record) = state.operations.get(&operation_id) {
            if record.binding != binding {
                return Err(OperationEventError::AlreadyBound);
            }
            return Ok(OperationEmitter {
                authority: self.clone(),
                binding,
                cancellation: record.cancellation.subscribe(),
            });
        }
        self.evict_terminal_if_needed(&mut state)?;
        let generation = self.inner.resume.next_generation()?;
        let resume_token = self.inner.resume.issue(&binding, generation)?;
        let (live, _) = broadcast::channel(self.inner.config.retained_event_capacity);
        let (frontier, _) = watch::channel(StreamFrontier {
            next_sequence: 0,
            retained_from_sequence: 0,
            resume_token: Some(resume_token.clone()),
        });
        let (cancellation, cancellation_receiver) = watch::channel(None);
        let accepted = StreamEvent {
            sequence: 0,
            kind: StreamEventKind::Item(OperationEventItem::Accepted {
                operation_id: operation_id.clone(),
                originating_request_id: request_id,
                operation: OperationKind::TestRun,
                content_class: DisclosureClass::Metadata,
            }),
        };
        let mut history = VecDeque::with_capacity(self.inner.config.retained_event_capacity);
        history.push_back(accepted.clone());
        let record = OperationRecord {
            binding: binding.clone(),
            generation,
            resume_token,
            history,
            next_sequence: 1,
            terminal: None,
            live,
            frontier,
            cancellation,
            subscribers: Arc::new(AtomicUsize::new(0)),
        };
        record.frontier.send_replace(frontier_for(&record));
        let _ = record.live.send(accepted);
        state.insertion_order.push_back(operation_id.clone());
        state.operations.insert(operation_id, record);
        Ok(OperationEmitter {
            authority: self.clone(),
            binding,
            cancellation: cancellation_receiver,
        })
    }

    /// Returns the newest retained managed test run for exactly one admitted
    /// project root. Absence after restart or eviction is `FrontierExpired`.
    pub(crate) async fn latest_managed_test_run(
        &self,
        root_uri: &str,
    ) -> Result<ManagedTestRunSnapshot, OperationEventError> {
        let state = self.inner.state.lock().await;
        managed_test_run_snapshot(&state, root_uri)
    }

    pub(crate) fn try_latest_managed_test_run(
        &self,
        root_uri: &str,
    ) -> Option<Result<ManagedTestRunSnapshot, OperationEventError>> {
        let state = self.inner.state.try_lock().ok()?;
        Some(managed_test_run_snapshot(&state, root_uri))
    }

    async fn page_managed_test_run(
        &self,
        mut snapshot: ManagedTestRunSnapshot,
        page: &PageRequest,
    ) -> Result<ManagedTestRunSnapshot, OperationEventError> {
        let state = self.inner.state.lock().await;
        let record = state
            .operations
            .get(&snapshot.operation_id)
            .ok_or(OperationEventError::NotFoundOrNotAuthorized)?;
        if record.generation != snapshot.generation {
            return Err(OperationEventError::NotFoundOrNotAuthorized);
        }
        let available_results = snapshot.results.len();
        let offset = match page.cursor.as_ref() {
            Some(cursor) => {
                self.inner
                    .resume
                    .verify_test_result_cursor(cursor, record, snapshot.completed)?
            }
            None => 0,
        };
        if offset > available_results {
            return Err(OperationEventError::NotFoundOrNotAuthorized);
        }
        let page_size = usize::try_from(page.page_size)
            .map_err(|_| OperationEventError::NotFoundOrNotAuthorized)?;
        let end = offset.saturating_add(page_size).min(available_results);
        snapshot.results = snapshot.results[offset..end].to_vec();
        snapshot.result_offset = offset;
        snapshot.available_results = available_results;
        snapshot.next_cursor = (end < available_results)
            .then(|| {
                self.inner.resume.issue_test_result_cursor(
                    &record.binding,
                    record.generation,
                    snapshot.completed,
                    end,
                )
            })
            .transpose()?;
        Ok(snapshot)
    }

    /// Requests cancellation for one exact trusted project-local test run.
    pub(crate) async fn cancel_managed_test_run(
        &self,
        operation_id: &OperationId,
        root_uri: &str,
    ) -> Result<OperationCancelOutcome, OperationEventError> {
        let state = self.inner.state.lock().await;
        let record = state
            .operations
            .get(operation_id)
            .ok_or(OperationEventError::FrontierExpired)?;
        if !matches!(
            &record.binding.authorization,
            OperationAuthorization::ProjectRoot { root_uri: retained, .. }
                if retained.trim_end_matches('/') == root_uri.trim_end_matches('/')
        ) {
            return Err(OperationEventError::NotFoundOrNotAuthorized);
        }
        if record.terminal.is_some() {
            return Ok(OperationCancelOutcome::AlreadyTerminal);
        }
        let already_requested = record.cancellation.borrow().is_some();
        if !already_requested {
            record
                .cancellation
                .send_replace(Some(current_micros_for_cancellation()));
        }
        Ok(if already_requested {
            OperationCancelOutcome::AlreadyRequested
        } else {
            OperationCancelOutcome::Requested
        })
    }

    /// Replays retained events from `requested_next_sequence`, then follows
    /// the same bounded Tokio broadcast stream used by live producers.
    pub async fn subscribe(
        &self,
        operation_id: &OperationId,
        context: &RequestContext,
        observed_at: UtcMicros,
        requested_next_sequence: u64,
        resume_token: Option<&ResumeToken>,
    ) -> Result<OperationEventSubscription, OperationEventError> {
        validate_admitted_context(context, observed_at)?;
        let state = self.inner.state.lock().await;
        let Some(record) = state.operations.get(operation_id) else {
            return Err(if resume_token.is_some() {
                OperationEventError::ResumeExpired
            } else {
                OperationEventError::FrontierExpired
            });
        };
        authorize(record, context)?;
        if requested_next_sequence > 0 && resume_token.is_none() {
            return Err(OperationEventError::ResumeExpired);
        }
        if let Some(resume_token) = resume_token {
            self.inner.resume.verify(resume_token, record)?;
        }

        let frontier = frontier_for(record);
        if requested_next_sequence > frontier.next_sequence {
            return Err(OperationEventError::InvalidFrontier);
        }
        record
            .subscribers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < self.inner.config.max_subscribers_per_operation).then_some(count + 1)
            })
            .map_err(|_| OperationEventError::Saturated)?;

        // Subscribe while the authority lock still fences publishers, then
        // snapshot replay. No event can fall between the two lanes.
        let live = record.live.subscribe();
        let mut replay = VecDeque::new();
        if requested_next_sequence < frontier.retained_from_sequence {
            replay.push_back(StreamEvent {
                sequence: requested_next_sequence,
                kind: StreamEventKind::Gap(StreamGap {
                    first_missing_sequence: requested_next_sequence,
                    last_missing_sequence: frontier.retained_from_sequence - 1,
                    frontier: frontier.clone(),
                }),
            });
        }
        replay.extend(
            record
                .history
                .iter()
                .filter(|event| event.sequence >= requested_next_sequence)
                .cloned(),
        );
        if replay.is_empty()
            && let Some(terminal) = &record.terminal
        {
            replay.push_back(terminal.clone());
        }

        Ok(OperationEventSubscription {
            correlation_id: record.binding.originating_request_id.clone(),
            frontier: frontier.clone(),
            stream: OperationEventStream {
                replay,
                pending_live: None,
                live: BroadcastStream::new(live),
                frontier: record.frontier.subscribe(),
                expected_sequence: requested_next_sequence,
                terminal_seen: false,
                subscribers: Arc::clone(&record.subscribers),
            },
        })
    }

    /// Requests cancellation after revalidating actor, scope, grant, and
    /// disclosure. Subscription disconnects never call this method.
    pub async fn cancel(
        &self,
        operation_id: &OperationId,
        context: &RequestContext,
        observed_at: UtcMicros,
    ) -> Result<OperationCancelOutcome, OperationEventError> {
        validate_admitted_context(context, observed_at)?;
        let state = self.inner.state.lock().await;
        let Some(record) = state.operations.get(operation_id) else {
            return Err(OperationEventError::FrontierExpired);
        };
        authorize(record, context)?;
        if record.terminal.is_some() {
            return Ok(OperationCancelOutcome::AlreadyTerminal);
        }
        let already_requested = record.cancellation.borrow().is_some();
        if !already_requested {
            record.cancellation.send_replace(Some(observed_at));
        }
        Ok(if already_requested {
            OperationCancelOutcome::AlreadyRequested
        } else {
            OperationCancelOutcome::Requested
        })
    }

    /// Drops all memory-retained frontiers. Existing streams close; reconnects
    /// receive `FrontierExpired` rather than a fabricated snapshot.
    pub async fn expire_all(&self) {
        let mut state = self.inner.state.lock().await;
        state.operations.clear();
        state.insertion_order.clear();
    }

    async fn emit_progress(
        &self,
        operation_id: &OperationId,
        completed: u64,
        total: Option<u64>,
    ) -> Result<OperationEvent, OperationEventError> {
        if total.is_some_and(|total| completed > total) {
            return Err(OperationEventError::InvalidProgress);
        }
        let mut state = self.inner.state.lock().await;
        let record = state
            .operations
            .get_mut(operation_id)
            .ok_or(OperationEventError::FrontierExpired)?;
        if record.terminal.is_some() {
            return Err(OperationEventError::TerminalAlreadyPublished);
        }
        let event = StreamEvent {
            sequence: record.next_sequence,
            kind: StreamEventKind::Progress { completed, total },
        };
        retain_and_publish(
            record,
            event.clone(),
            self.inner.config.retained_event_capacity,
        );
        Ok(event)
    }

    async fn emit_test_result(
        &self,
        operation_id: &OperationId,
        test: String,
        passed: bool,
    ) -> Result<OperationEvent, OperationEventError> {
        if test.is_empty()
            || test.len() > 1_024
            || test.trim() != test
            || test.chars().any(char::is_control)
        {
            return Err(OperationEventError::InvalidTestRunEvent);
        }
        let mut state = self.inner.state.lock().await;
        let record = state
            .operations
            .get_mut(operation_id)
            .ok_or(OperationEventError::FrontierExpired)?;
        if record.binding.operation != OperationKind::TestRun {
            return Err(OperationEventError::InvalidTestRunEvent);
        }
        if record.terminal.is_some() {
            return Err(OperationEventError::TerminalAlreadyPublished);
        }
        let event = StreamEvent {
            sequence: record.next_sequence,
            kind: StreamEventKind::Item(OperationEventItem::TestRunResult { test, passed }),
        };
        retain_and_publish(
            record,
            event.clone(),
            self.inner.config.retained_event_capacity,
        );
        Ok(event)
    }

    async fn emit_terminal(
        &self,
        operation_id: &OperationId,
        receipt: OperationReceipt,
    ) -> Result<OperationEvent, OperationEventError> {
        receipt
            .validate()
            .map_err(|error| OperationEventError::InvalidTerminal(error.to_string()))?;
        let mut state = self.inner.state.lock().await;
        let record = state
            .operations
            .get_mut(operation_id)
            .ok_or(OperationEventError::FrontierExpired)?;
        if let Some(existing) = &record.terminal {
            if matches!(
                &existing.kind,
                StreamEventKind::Terminal(terminal) if terminal.receipt == receipt
            ) {
                return Ok(existing.clone());
            }
            return Err(OperationEventError::TerminalAlreadyPublished);
        }
        let event = StreamEvent::<OperationEventItem>::terminal(
            record.next_sequence,
            StreamTermination {
                termination: receipt.termination,
                receipt,
            },
        )
        .map_err(|error| OperationEventError::InvalidTerminal(error.to_string()))?;
        retain_and_publish(
            record,
            event.clone(),
            self.inner.config.retained_event_capacity,
        );
        record.terminal = Some(event.clone());
        Ok(event)
    }

    fn evict_terminal_if_needed(
        &self,
        state: &mut AuthorityState,
    ) -> Result<(), OperationEventError> {
        while state.operations.len() >= self.inner.config.max_operations {
            let Some(position) = state.insertion_order.iter().position(|operation_id| {
                state.operations.get(operation_id).is_none_or(|record| {
                    record.terminal.is_some() && record.subscribers.load(Ordering::Acquire) == 0
                })
            }) else {
                return Err(OperationEventError::Saturated);
            };
            if let Some(operation_id) = state.insertion_order.remove(position) {
                state.operations.remove(&operation_id);
            }
        }
        Ok(())
    }
}

fn managed_test_run_snapshot(
    state: &AuthorityState,
    root_uri: &str,
) -> Result<ManagedTestRunSnapshot, OperationEventError> {
    let record = state
        .insertion_order
        .iter()
        .rev()
        .filter_map(|operation_id| state.operations.get(operation_id))
        .find(|record| {
            record.binding.operation == OperationKind::TestRun
                && matches!(
                    &record.binding.authorization,
                    OperationAuthorization::ProjectRoot {
                        root_uri: retained,
                        ..
                    } if retained.trim_end_matches('/') == root_uri.trim_end_matches('/')
                )
        })
        .ok_or(OperationEventError::FrontierExpired)?;
    let results = record
        .history
        .iter()
        .filter_map(|event| match &event.kind {
            StreamEventKind::Item(OperationEventItem::TestRunResult { test, passed }) => {
                Some(ManagedTestRunResult {
                    test: test.clone(),
                    passed: *passed,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let available_results = results.len();
    let (completed, total) = record
        .history
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            StreamEventKind::Progress { completed, total } => Some((*completed, *total)),
            _ => None,
        })
        .unwrap_or((0, None));
    let termination = record
        .terminal
        .as_ref()
        .and_then(|event| match &event.kind {
            StreamEventKind::Terminal(terminal) => Some(terminal.termination),
            _ => None,
        });
    Ok(ManagedTestRunSnapshot {
        operation_id: record.binding.operation_id.clone(),
        generation: record.generation,
        source_revision: record.next_sequence,
        head_commit_id: match &record.binding.authorization {
            OperationAuthorization::ProjectRoot { head_commit_id, .. } => head_commit_id.clone(),
            OperationAuthorization::Request { .. } => None,
        },
        code_generation_id: match &record.binding.authorization {
            OperationAuthorization::ProjectRoot {
                code_generation_id, ..
            } => code_generation_id.clone(),
            OperationAuthorization::Request { .. } => None,
        },
        document_content_digests: match &record.binding.authorization {
            OperationAuthorization::ProjectRoot {
                document_content_digests,
                ..
            } => document_content_digests.clone(),
            OperationAuthorization::Request { .. } => BTreeMap::new(),
        },
        deadline: match &record.binding.authorization {
            OperationAuthorization::ProjectRoot { deadline, .. } => deadline.clone(),
            OperationAuthorization::Request { .. } => {
                return Err(OperationEventError::InvalidTestRunEvent);
            }
        },
        results,
        result_offset: 0,
        available_results,
        next_cursor: None,
        completed,
        total,
        termination,
    })
}

#[derive(Clone)]
pub struct OperationEmitter {
    authority: OperationEventAuthority,
    binding: OperationBinding,
    cancellation: watch::Receiver<Option<UtcMicros>>,
}

impl OperationEmitter {
    pub fn binding(&self) -> &OperationBinding {
        &self.binding
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.borrow().is_some()
    }

    pub fn cancellation_requested_at(&self) -> Option<UtcMicros> {
        *self.cancellation.borrow()
    }

    pub async fn cancelled(&mut self) {
        while self.cancellation.borrow_and_update().is_none() {
            if self.cancellation.changed().await.is_err() {
                return;
            }
        }
    }

    pub async fn request_managed_test_cancellation(
        &self,
    ) -> Result<OperationCancelOutcome, OperationEventError> {
        let OperationAuthorization::ProjectRoot { root_uri, .. } = &self.binding.authorization
        else {
            return Err(OperationEventError::NotFoundOrNotAuthorized);
        };
        self.authority
            .cancel_managed_test_run(self.binding.operation_id(), root_uri)
            .await
    }

    pub async fn progress(
        &self,
        completed: u64,
        total: Option<u64>,
    ) -> Result<OperationEvent, OperationEventError> {
        self.authority
            .emit_progress(self.binding.operation_id(), completed, total)
            .await
    }

    pub async fn test_result(
        &self,
        test: String,
        passed: bool,
    ) -> Result<OperationEvent, OperationEventError> {
        self.authority
            .emit_test_result(self.binding.operation_id(), test, passed)
            .await
    }

    /// Idempotently publishes the one receipt-bearing terminal event.
    pub async fn terminal(
        &self,
        receipt: OperationReceipt,
    ) -> Result<OperationEvent, OperationEventError> {
        self.authority
            .emit_terminal(self.binding.operation_id(), receipt)
            .await
    }
}

pub struct OperationEventSubscription {
    correlation_id: RequestId,
    frontier: StreamFrontier,
    stream: OperationEventStream,
}

impl OperationEventSubscription {
    pub fn correlation_id(&self) -> &RequestId {
        &self.correlation_id
    }

    pub fn frontier(&self) -> &StreamFrontier {
        &self.frontier
    }

    /// Mount API: pass these values directly to
    /// `tracedecay_api::sse_response(correlation_id, frontier, stream)`.
    pub fn into_sse_parts(self) -> (RequestId, StreamFrontier, OperationEventStream) {
        (self.correlation_id, self.frontier, self.stream)
    }
}

pub struct OperationEventStream {
    replay: VecDeque<OperationEvent>,
    pending_live: Option<OperationEvent>,
    live: BroadcastStream<OperationEvent>,
    frontier: watch::Receiver<StreamFrontier>,
    expected_sequence: u64,
    terminal_seen: bool,
    subscribers: Arc<AtomicUsize>,
}

impl Stream for OperationEventStream {
    type Item = OperationEvent;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let stream = self.get_mut();
        if stream.terminal_seen {
            return Poll::Ready(None);
        }
        if let Some(event) = stream.replay.pop_front() {
            stream.observe(&event);
            return Poll::Ready(Some(event));
        }
        if let Some(event) = stream.pending_live.take() {
            stream.observe(&event);
            return Poll::Ready(Some(event));
        }

        loop {
            match Pin::new(&mut stream.live).poll_next(context) {
                Poll::Ready(Some(Ok(event))) if event.sequence < stream.expected_sequence => {}
                Poll::Ready(Some(Ok(event))) if event.sequence > stream.expected_sequence => {
                    let gap = stream.gap(stream.expected_sequence, event.sequence - 1);
                    stream.expected_sequence = event.sequence;
                    stream.pending_live = Some(event);
                    return Poll::Ready(Some(gap));
                }
                Poll::Ready(Some(Ok(event))) => {
                    stream.observe(&event);
                    return Poll::Ready(Some(event));
                }
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(skipped)))) => {
                    let first_missing = stream.expected_sequence;
                    let last_missing = first_missing.saturating_add(skipped.saturating_sub(1));
                    let gap = stream.gap(first_missing, last_missing);
                    stream.expected_sequence = last_missing.saturating_add(1);
                    return Poll::Ready(Some(gap));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl OperationEventStream {
    fn observe(&mut self, event: &OperationEvent) {
        match &event.kind {
            StreamEventKind::Gap(gap) => {
                self.expected_sequence = gap.last_missing_sequence.saturating_add(1);
            }
            StreamEventKind::Terminal(_) => {
                self.expected_sequence = event.sequence.saturating_add(1);
                self.terminal_seen = true;
            }
            _ => {
                self.expected_sequence = event.sequence.saturating_add(1);
            }
        }
    }

    fn gap(&self, first_missing_sequence: u64, last_missing_sequence: u64) -> OperationEvent {
        StreamEvent {
            sequence: first_missing_sequence,
            kind: StreamEventKind::Gap(StreamGap {
                first_missing_sequence,
                last_missing_sequence,
                frontier: self.frontier.borrow().clone(),
            }),
        }
    }
}

impl Drop for OperationEventStream {
    fn drop(&mut self) {
        let _ = self
            .subscribers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            });
    }
}

fn validate_admitted_context(
    context: &RequestContext,
    observed_at: UtcMicros,
) -> Result<(), OperationEventError> {
    context
        .validate()
        .map_err(|error| OperationEventError::InvalidContext(error.to_string()))?;
    if context.cancellation().is_cancelled()
        || context.deadline().is_elapsed_at(observed_at)
        || context.grant().is_expired_at(observed_at)
    {
        return Err(OperationEventError::RequestNotAdmitted);
    }
    Ok(())
}

fn invalid_context(error: impl fmt::Display) -> OperationEventError {
    OperationEventError::InvalidContext(error.to_string())
}

fn authorize(
    record: &OperationRecord,
    context: &RequestContext,
) -> Result<(), OperationEventError> {
    let OperationAuthorization::Request {
        actor,
        scope,
        access_digest: _,
        allowed_capabilities,
        allowed_use_cases,
    } = &record.binding.authorization
    else {
        return Err(OperationEventError::NotFoundOrNotAuthorized);
    };
    (context.actor() == actor
        && context.scope() == scope
        && context.grant().disclosure >= record.binding.event_disclosure()
        && context
            .grant()
            .allowed_capabilities
            .is_superset(allowed_capabilities)
        && context
            .grant()
            .allowed_use_cases
            .is_superset(allowed_use_cases))
    .then_some(())
    .ok_or(OperationEventError::NotFoundOrNotAuthorized)
}

fn retain_and_publish(record: &mut OperationRecord, event: OperationEvent, capacity: usize) {
    record.next_sequence = event.sequence.saturating_add(1);
    record.history.push_back(event.clone());
    while record.history.len() > capacity {
        record.history.pop_front();
    }
    record.frontier.send_replace(frontier_for(record));
    let _ = record.live.send(event);
}

fn frontier_for(record: &OperationRecord) -> StreamFrontier {
    StreamFrontier {
        next_sequence: record.next_sequence,
        retained_from_sequence: record
            .history
            .front()
            .map_or(record.next_sequence, |event| event.sequence),
        resume_token: Some(record.resume_token.clone()),
    }
}

fn operation_resume_snapshot(
    binding: &OperationBinding,
    generation: u64,
    key: SignedCursorKeyRefV1,
) -> Result<TemporalExecutionSnapshot, OperationEventError> {
    let (root_digest, access_digest) = match &binding.authorization {
        OperationAuthorization::Request {
            scope,
            access_digest,
            ..
        } => (
            scope.scope_digest.as_str().to_owned(),
            access_digest.clone(),
        ),
        OperationAuthorization::ProjectRoot { root_uri, .. } => (
            canonical_sha256(&("operation-stream-root-v1", root_uri))
                .map_err(|_| OperationEventError::ResumeUnavailable)?
                .as_str()
                .to_owned(),
            canonical_sha256(&("operation-stream-project-root-access-v1", root_uri))
                .map_err(|_| OperationEventError::ResumeUnavailable)?
                .as_str()
                .to_owned(),
        ),
    };
    let request_digest = canonical_sha256(&(
        "operation-stream-request-v1",
        &binding.operation_id,
        &binding.originating_request_id,
    ))
    .map_err(|_| OperationEventError::ResumeUnavailable)?;
    let filter_digest = canonical_sha256(&(
        "operation-stream-filter-v1",
        binding.operation,
        binding.event_disclosure,
    ))
    .map_err(|_| OperationEventError::ResumeUnavailable)?;
    let configuration_digest = canonical_sha256(&("operation-stream-configuration-v1", 1_u32))
        .map_err(|_| OperationEventError::ResumeUnavailable)?;
    let session_id = SessionId::new(format!(
        "operation-stream-{}",
        request_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| OperationEventError::ResumeUnavailable)?;
    let request = TemporalSnapshotRequest::new(
        session_id,
        root_digest,
        request_digest.as_str(),
        access_digest,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
    )
    .and_then(|request| request.with_filter_digest(filter_digest.as_str()))
    .map_err(|_| OperationEventError::ResumeUnavailable)?;
    TemporalExecutionSnapshot::new_authorized(
        request,
        TemporalWatermarks {
            generation,
            source: generation,
            projection: generation,
            index: generation,
            summary: generation,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new(
                "configuration_digest",
                configuration_digest.as_str(),
            )
            .map_err(|_| OperationEventError::ResumeUnavailable)?,
        },
        Some(key),
        ValidatedAuthorization::Authorized,
    )
    .map_err(|_| OperationEventError::ResumeUnavailable)
}

fn operation_resume_verification_error(error: CursorError) -> OperationEventError {
    match error {
        CursorError::Expired
        | CursorError::UnknownOrExpiredKey
        | CursorError::KeyUnavailable
        | CursorError::KeyIdMismatch
        | CursorError::KeyVersionMismatch
        | CursorError::GenerationMismatch
        | CursorError::ParticipantManifestMismatch
        | CursorError::EpochMismatch
        | CursorError::SourceWatermarkMismatch
        | CursorError::ProjectionWatermarkMismatch
        | CursorError::IndexWatermarkMismatch
        | CursorError::SummaryWatermarkMismatch => OperationEventError::ResumeExpired,
        CursorError::Malformed
        | CursorError::Tampered
        | CursorError::WrongRequest
        | CursorError::FilterMismatch
        | CursorError::RootMismatch
        | CursorError::SessionMismatch
        | CursorError::WrongAccess
        | CursorError::TemporalModeMismatch
        | CursorError::GrainMismatch
        | CursorError::SchemaMismatch
        | CursorError::RankingMismatch
        | CursorError::ConfigurationMismatch
        | CursorError::SortKeyMismatch => OperationEventError::NotFoundOrNotAuthorized,
        CursorError::InvalidKeyMaterial => OperationEventError::ResumeUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_application::{
        ApplicationProblemKind, Deadline, OpaqueCursor, PageRequest, RequestId,
    };
    use tracedecay_domain::{CodeGenerationId, CommitId, ContentDigest, UtcMicros};

    use super::{
        CanonicalManagedTestRunReader, ManagedTestRunCurrentScope, ManagedTestRunReadOutcome,
        ManagedTestRunStaleReason, ManagedTestRunUnavailableReason, OperationCancelOutcome,
        OperationEventAuthority, OperationEventError, OperationId,
    };

    #[test]
    fn operation_stream_errors_use_canonical_problem_envelopes() {
        let saturated = OperationEventError::Saturated.into_problem_envelope(
            RequestId::new("request.operation.saturated").expect("request id"),
        );
        assert_eq!(saturated.problem.kind(), ApplicationProblemKind::Saturated);
        assert_eq!(saturated.problem.retry_after_millis, Some(250));

        let expired = OperationEventError::ResumeExpired.into_problem_envelope(
            RequestId::new("request.operation.expired").expect("request id"),
        );
        assert_eq!(expired.problem.kind(), ApplicationProblemKind::Stale);
        assert_eq!(
            expired.problem.code,
            "operation_event.resume_expired".to_owned()
        );
    }

    #[tokio::test]
    async fn managed_test_snapshot_retains_operation_generation() {
        let authority = OperationEventAuthority::default();
        let emitter = authority
            .begin_managed_test_run(
                "file:///workspace".to_owned(),
                RequestId::new("request.test-run.generation").expect("request id"),
                Some(
                    CommitId::new("0123456789abcdef0123456789abcdef01234567").expect("head commit"),
                ),
                Some(CodeGenerationId::new("generation.test.current").expect("code generation")),
                BTreeMap::new(),
                Deadline::new(UtcMicros(10_000)).expect("deadline"),
            )
            .await
            .expect("managed test run");
        emitter
            .test_result("suite::passes".to_owned(), true)
            .await
            .expect("test result");
        emitter.progress(1, Some(1)).await.expect("test progress");

        let snapshot = authority
            .latest_managed_test_run("file:///workspace")
            .await
            .expect("managed test snapshot");

        assert_eq!(snapshot.generation, 1);
        assert_eq!(
            snapshot.head_commit_id.as_ref().map(CommitId::as_str),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(
            snapshot
                .code_generation_id
                .as_ref()
                .map(CodeGenerationId::as_str),
            Some("generation.test.current")
        );
        assert_eq!(snapshot.deadline.expires_at, UtcMicros(10_000));
        assert_eq!(snapshot.completed, 1);
    }

    #[tokio::test]
    async fn managed_test_authority_cancellation_reaches_the_emitter() {
        let authority = OperationEventAuthority::default();
        let request_id = RequestId::new("request.test-run.cancel").expect("request id");
        let operation_id = OperationId::from_request(request_id.clone());
        let emitter = authority
            .begin_managed_test_run(
                "file:///workspace".to_owned(),
                request_id,
                None,
                None,
                BTreeMap::new(),
                Deadline::new(UtcMicros(10_000)).expect("deadline"),
            )
            .await
            .expect("managed test run");

        assert_eq!(
            authority
                .cancel_managed_test_run(&operation_id, "file:///workspace")
                .await
                .expect("cancel"),
            OperationCancelOutcome::Requested
        );
        assert!(emitter.is_cancelled());
    }

    #[tokio::test]
    async fn canonical_test_run_reader_rejects_document_content_drift() {
        let authority = OperationEventAuthority::default();
        let head = CommitId::new("0123456789abcdef0123456789abcdef01234567").expect("head commit");
        let generation = CodeGenerationId::new("generation.test.current").expect("code generation");
        let document_uri = "file:///workspace/src/lib.rs";
        let retained_digest =
            ContentDigest::new(format!("sha256:{}", "a".repeat(64))).expect("retained digest");
        authority
            .begin_managed_test_run(
                "file:///workspace".to_owned(),
                RequestId::new("request.test-run.document-drift").expect("request id"),
                Some(head.clone()),
                Some(generation.clone()),
                BTreeMap::from([(document_uri.to_owned(), retained_digest.clone())]),
                Deadline::new(UtcMicros(10_000)).expect("deadline"),
            )
            .await
            .expect("managed test run");
        let reader = CanonicalManagedTestRunReader::new(authority);
        let mut current = ManagedTestRunCurrentScope {
            root_uri: "file:///workspace".to_owned(),
            head_commit_id: Some(head),
            code_generation_id: Some(generation),
            document_uri: Some(document_uri.to_owned()),
            document_content_digest: Some(retained_digest),
        };

        assert!(matches!(
            reader.latest_current(&current).await,
            ManagedTestRunReadOutcome::Current(_)
        ));
        current.document_content_digest =
            Some(ContentDigest::new(format!("sha256:{}", "b".repeat(64))).expect("changed digest"));
        assert_eq!(
            reader.latest_current(&current).await,
            ManagedTestRunReadOutcome::Stale(ManagedTestRunStaleReason::DocumentContent)
        );
    }

    #[tokio::test]
    async fn canonical_test_run_reader_rejects_exact_source_identity_drift() {
        let authority = OperationEventAuthority::default();
        let head = CommitId::new("0123456789abcdef0123456789abcdef01234567").expect("head commit");
        let generation = CodeGenerationId::new("generation.test.current").expect("code generation");
        authority
            .begin_managed_test_run(
                "file:///workspace".to_owned(),
                RequestId::new("request.test-run.source-drift").expect("request id"),
                Some(head.clone()),
                Some(generation.clone()),
                BTreeMap::new(),
                Deadline::new(UtcMicros(10_000)).expect("deadline"),
            )
            .await
            .expect("managed test run");
        let reader = CanonicalManagedTestRunReader::new(authority);

        for current in [
            ManagedTestRunCurrentScope {
                root_uri: "file:///workspace".to_owned(),
                head_commit_id: Some(
                    CommitId::new("fedcba9876543210fedcba9876543210fedcba98")
                        .expect("changed head"),
                ),
                code_generation_id: Some(generation.clone()),
                document_uri: None,
                document_content_digest: None,
            },
            ManagedTestRunCurrentScope {
                root_uri: "file:///workspace".to_owned(),
                head_commit_id: Some(head.clone()),
                code_generation_id: Some(
                    CodeGenerationId::new("generation.test.changed")
                        .expect("changed code generation"),
                ),
                document_uri: None,
                document_content_digest: None,
            },
        ] {
            assert_eq!(
                reader.latest_current(&current).await,
                ManagedTestRunReadOutcome::Stale(ManagedTestRunStaleReason::SourceIdentity)
            );
        }
    }

    #[tokio::test]
    async fn canonical_test_run_reader_pages_with_an_authenticated_stable_cursor() {
        let authority = OperationEventAuthority::default();
        let head = CommitId::new("0123456789abcdef0123456789abcdef01234567").expect("head commit");
        let generation = CodeGenerationId::new("generation.test.page").expect("code generation");
        let emitter = authority
            .begin_managed_test_run(
                "file:///workspace".to_owned(),
                RequestId::new("request.test-run.page").expect("request id"),
                Some(head.clone()),
                Some(generation.clone()),
                BTreeMap::new(),
                Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            )
            .await
            .expect("managed test run");
        for index in 0..3 {
            emitter
                .test_result(format!("suite::test_{index}"), index != 1)
                .await
                .expect("test result");
        }
        emitter.progress(3, Some(3)).await.expect("test progress");
        let reader = CanonicalManagedTestRunReader::new(authority);
        let current = ManagedTestRunCurrentScope {
            root_uri: "file:///workspace".to_owned(),
            head_commit_id: Some(head),
            code_generation_id: Some(generation),
            document_uri: None,
            document_content_digest: None,
        };

        let ManagedTestRunReadOutcome::Current(first) = reader
            .latest_current_page(&current, &PageRequest::first(2).expect("page"))
            .await
        else {
            panic!("first page must be current");
        };
        assert_eq!(first.results.len(), 2);
        assert_eq!(first.result_offset, 0);
        assert_eq!(first.available_results, 3);
        let cursor = first.next_cursor.expect("continuation");
        let tampered = OpaqueCursor::new(format!("{}x", cursor.as_str())).expect("opaque cursor");
        assert_eq!(
            reader
                .latest_current_page(
                    &current,
                    &PageRequest::new(2, Some(tampered)).expect("tampered page"),
                )
                .await,
            ManagedTestRunReadOutcome::Unavailable(
                ManagedTestRunUnavailableReason::AuthorityFailure,
            )
        );

        let ManagedTestRunReadOutcome::Current(second) = reader
            .latest_current_page(
                &current,
                &PageRequest::new(2, Some(cursor)).expect("continuation page"),
            )
            .await
        else {
            panic!("second page must be current");
        };
        assert_eq!(second.results.len(), 1);
        assert_eq!(second.results[0].test, "suite::test_2");
        assert_eq!(second.result_offset, 2);
        assert!(second.next_cursor.is_none());
    }
}
