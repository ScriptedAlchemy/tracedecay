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

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, TryLockError, broadcast, watch};
use tokio_stream::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tracedecay_contracts::{
    ApplicationContractError, ApplicationProblem, ApplicationProblemEnvelope, CancellationContext,
    CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass, InvocationTarget,
    LegalAction, OperationReceipt, ProblemOwningLayer, RequestContext, RequestId, ResolvedScope,
    ResultContractRef, ResumeToken, RetryDirective, SafeDiagnostic, StreamEvent, StreamEventKind,
    StreamFrontier, StreamGap, StreamTermination, now_micros,
};
use tracedecay_domain::{
    ActorId, CursorBindingMismatchV1, CursorBindingV1, ProjectId, RetrievalGrainV1,
    SessionCursorKeyIdV1, SessionCursorVersionV1, SessionId, SignedCursorKeyRefV1, TemporalModeV1,
    UtcMicros, canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, SchemaId, UseCaseId};

use tracedecay_temporal_query::cursor::InMemoryCursorAuthenticator;
use tracedecay_temporal_query::cursor::{CursorError, StableSortKey, encode_cursor, verify_cursor};
use tracedecay_temporal_query::execution::BindingDigest;
use tracedecay_temporal_query::snapshot::TemporalSnapshotRequest;
use tracedecay_temporal_query::snapshot::{
    KernelVersions, TemporalExecutionSnapshot, TemporalWatermarks,
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
    #[error("{0}")]
    CursorRefused(CursorBindingMismatchV1),
}

impl OperationEventError {
    /// Converts runtime stream failures into the one canonical application
    /// problem envelope used by every transport.
    pub fn into_problem_envelope(
        self,
        request_id: RequestId,
    ) -> Result<ApplicationProblemEnvelope, ApplicationContractError> {
        let saturated = matches!(self, Self::Saturated);
        let problem = match self {
            Self::NotFoundOrNotAuthorized => {
                ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
            }
            Self::CursorRefused(mismatch) => ApplicationProblem::cursor_refused(&mismatch),
            Self::FrontierExpired | Self::ResumeExpired => ApplicationProblem::Stale {
                diagnostic: SafeDiagnostic::new(
                    "operation_event.resume_expired",
                    "The operation-event resume frontier has expired",
                )?,
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![LegalAction::Refresh],
                detail: None,
            },
            Self::InvalidFrontier => ApplicationProblem::conflict(
                "operation_event.invalid_frontier",
                "The requested operation-event frontier is invalid",
            ),
            Self::RequestNotAdmitted => ApplicationProblem::timed_out_before_admission(),
            Self::Saturated => ApplicationProblem::saturated(
                "operation_event.saturated",
                "Operation-event capacity is temporarily saturated",
            ),
            // Permanently invalid input: the same request can never succeed, so
            // the client must correct it rather than retry.
            Self::InvalidContext(_)
            | Self::InvalidProgress
            | Self::InvalidTerminal(_)
            | Self::InvalidTestRunEvent => ApplicationProblem::invalid_request(
                "operation_event.invalid_request",
                "The operation-event request is invalid",
            ),
            // Idempotency facts: the identity or terminal receipt is already
            // published, so the client re-reads current state instead of
            // retrying the same publish.
            Self::AlreadyBound | Self::TerminalAlreadyPublished => ApplicationProblem::conflict(
                "operation_event.already_published",
                "The operation-event identity is already published",
            ),
            // A misconfigured authority is a deterministic, process-lifetime
            // failure. It is not the caller's request that is wrong and no
            // amount of retrying will change the outcome.
            Self::InvalidConfiguration => ApplicationProblem::Unsupported {
                diagnostic: SafeDiagnostic::new(
                    "operation_event.unsupported",
                    "The operation-event authority is not configured for this operation",
                )?,
                retry: RetryDirective::Never,
                legal_actions: vec![LegalAction::ContactAdministrator],
                detail: None,
            },
            // Genuinely transient: the resume-token authority could not answer.
            Self::ResumeUnavailable => ApplicationProblem::unavailable(SafeDiagnostic::new(
                "operation_event.unavailable",
                "The operation-event service is unavailable",
            )?),
        };
        let envelope = ApplicationProblemEnvelope::new(
            OPERATION_EVENT_PROBLEM_CONTRACT.clone(),
            request_id,
            problem,
        )?
        .with_owning_layer(ProblemOwningLayer::Runtime);
        if saturated {
            envelope.with_retry_after_millis(Some(250))
        } else {
            Ok(envelope)
        }
    }
}

/// What the live stream reports about a managed run it is still executing.
/// The run's source identity and terminal result live in its durable record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedTestRunProgress {
    pub(crate) completed: u64,
    pub(crate) total: Option<u64>,
    pub(crate) deadline: Deadline,
}

/// Identifies the newest managed run the live stream holds for one root and
/// how many events it has published. A managed run's durable record changes
/// only while its live stream publishes, so an unchanged value means there is
/// nothing new to read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedTestRunActivity {
    operation_id: OperationId,
    published_events: u64,
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
            &resume_binding()?,
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
        let sort_key = verify_cursor(
            token.as_str(),
            &snapshot,
            &resume_binding()?,
            &self.authenticator,
        )
        .map_err(operation_resume_verification_error)?;
        if sort_key.normalized_score_micros != 0
            || sort_key.knowledge_at_micros != record.generation as i64
            || sort_key.stable_id != record.binding.operation_id.to_string()
        {
            return Err(OperationEventError::NotFoundOrNotAuthorized);
        }
        Ok(())
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

    #[tracing::instrument(name = "usecases.operation.resolve_context", level = "trace", skip_all)]
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
    #[tracing::instrument(name = "usecases.operation.begin", level = "trace", skip_all)]
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
    #[tracing::instrument(name = "usecases.operation.begin_test_run", level = "trace", skip_all)]
    pub async fn begin_managed_test_run(
        &self,
        root_uri: String,
        request_id: RequestId,
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
            authorization: OperationAuthorization::ProjectRoot { root_uri, deadline },
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

    /// Progress of `operation_id` while the live stream still executes it
    /// under `root_uri`. `None` once it published its terminal receipt, and
    /// when the stream no longer holds it: the daemon restarted, or its
    /// producer ended without settling.
    pub(crate) async fn managed_test_run_progress(
        &self,
        root_uri: &str,
        operation_id: &str,
    ) -> Option<ManagedTestRunProgress> {
        let state = self.inner.state.lock().await;
        let record = managed_test_runs_newest_first(&state, root_uri)
            .find(|record| record.binding.operation_id.to_string() == operation_id)?;
        if record.terminal.is_some() {
            return None;
        }
        let OperationAuthorization::ProjectRoot { deadline, .. } = &record.binding.authorization
        else {
            return None;
        };
        let (completed, total) = record
            .history
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                StreamEventKind::Progress { completed, total } => Some((*completed, *total)),
                _ => None,
            })
            .unwrap_or((0, None));
        Some(ManagedTestRunProgress {
            completed,
            total,
            deadline: deadline.clone(),
        })
    }

    /// The activity of the newest managed run held for `root_uri`, without
    /// waiting on the authority: `Ok(None)` when it holds no run for the root,
    /// and `Err` while another caller holds it.
    pub(crate) fn try_managed_test_run_activity(
        &self,
        root_uri: &str,
    ) -> Result<Option<ManagedTestRunActivity>, TryLockError> {
        let state = self.inner.state.try_lock()?;
        Ok(managed_test_runs_newest_first(&state, root_uri)
            .next()
            .map(|record| ManagedTestRunActivity {
                operation_id: record.binding.operation_id.clone(),
                published_events: record.next_sequence,
            }))
    }

    /// Requests cancellation for one exact trusted project-local test run.
    #[tracing::instrument(name = "usecases.operation.cancel_test_run", level = "trace", skip_all)]
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
            record.cancellation.send_replace(Some(now_micros()));
        }
        Ok(if already_requested {
            OperationCancelOutcome::AlreadyRequested
        } else {
            OperationCancelOutcome::Requested
        })
    }

    /// Replays retained events from `requested_next_sequence`, then follows
    /// the same bounded Tokio broadcast stream used by live producers.
    #[tracing::instrument(name = "usecases.operation.subscribe", level = "trace", skip_all)]
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
    #[tracing::instrument(name = "usecases.operation.cancel", level = "trace", skip_all)]
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

    /// Drops every memory-retained frontier that no live producer still
    /// writes. Streams on those operations close; reconnects receive
    /// `FrontierExpired` rather than a fabricated snapshot.
    ///
    /// A record whose `OperationEmitter` is still alive is kept. The
    /// process-global authority is shared by every daemon composition in the
    /// process and by the project workflow handlers that begin managed test
    /// runs outside any composition, so one composition's shutdown must not
    /// truncate a stream another producer is between admission and its first
    /// result on. The emitter is the only holder of the cancellation
    /// receiver, so its receiver count is the producer liveness signal.
    #[tracing::instrument(name = "usecases.operation.expire_idle", level = "trace", skip_all)]
    pub async fn expire_idle(&self) {
        let mut state = self.inner.state.lock().await;
        let AuthorityState {
            operations,
            insertion_order,
        } = &mut *state;
        operations.retain(|_, record| record.cancellation.receiver_count() > 0);
        insertion_order.retain(|operation_id| operations.contains_key(operation_id));
    }

    #[tracing::instrument(name = "usecases.operation.emit_progress", level = "trace", skip_all)]
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

    #[tracing::instrument(
        name = "usecases.operation.emit_test_result",
        level = "trace",
        skip_all
    )]
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

    #[tracing::instrument(name = "usecases.operation.emit_terminal", level = "trace", skip_all)]
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

fn managed_test_runs_newest_first<'state>(
    state: &'state AuthorityState,
    root_uri: &str,
) -> impl Iterator<Item = &'state OperationRecord> {
    let root_uri = root_uri.trim_end_matches('/').to_owned();
    state
        .insertion_order
        .iter()
        .rev()
        .filter_map(|operation_id| state.operations.get(operation_id))
        .filter(move |record| {
            record.binding.operation == OperationKind::TestRun
                && matches!(
                    &record.binding.authorization,
                    OperationAuthorization::ProjectRoot {
                        root_uri: retained,
                        ..
                    } if retained.trim_end_matches('/') == root_uri
                )
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

/// Resume tokens continue one operation's event stream; the operation and its
/// generation ride the snapshot.
fn resume_binding() -> Result<CursorBindingV1, OperationEventError> {
    CursorBindingV1::new("operation_events", Vec::new())
        .map_err(|_| OperationEventError::ResumeUnavailable)
}

fn operation_resume_verification_error(error: CursorError) -> OperationEventError {
    match error {
        CursorError::Binding(mismatch) => OperationEventError::CursorRefused(mismatch),
        CursorError::Expired
        | CursorError::UnknownOrExpiredKey
        | CursorError::KeyUnavailable
        | CursorError::KeyIdMismatch
        | CursorError::KeyVersionMismatch
        | CursorError::GenerationMismatch
        | CursorError::ParticipantManifestMismatch
        | CursorError::EpochMismatch
        | CursorError::CandidateCohortMismatch
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
    use tracedecay_contracts::{
        ApplicationProblemKind, Deadline, OperationBudgetUsage, OperationReceipt, RequestId,
    };
    use tracedecay_domain::UtcMicros;

    use super::{
        ManagedTestRunProgress, OperationCancelOutcome, OperationEventAuthority,
        OperationEventError, OperationEventItem, OperationId, StreamEventKind,
    };

    #[test]
    fn operation_stream_errors_use_canonical_problem_envelopes() {
        let saturated = OperationEventError::Saturated
            .into_problem_envelope(
                RequestId::new("request.operation.saturated").expect("request id"),
            )
            .expect("saturated problem envelope");
        assert_eq!(saturated.problem.kind(), ApplicationProblemKind::Saturated);
        assert_eq!(saturated.problem.retry_after_millis, Some(250));

        let expired = OperationEventError::ResumeExpired
            .into_problem_envelope(RequestId::new("request.operation.expired").expect("request id"))
            .expect("expired problem envelope");
        assert_eq!(expired.problem.kind(), ApplicationProblemKind::Stale);
        assert_eq!(
            expired.problem.code,
            "operation_event.resume_expired".to_owned()
        );
    }

    #[tokio::test]
    async fn a_live_managed_run_reports_progress_until_its_terminal_receipt() {
        let authority = OperationEventAuthority::default();
        let request_id = RequestId::new("request.test-run.progress").expect("request id");
        let operation_id = OperationId::from_request(request_id.clone()).to_string();
        let deadline = Deadline::new(UtcMicros(10_000)).expect("deadline");
        let emitter = authority
            .begin_managed_test_run("file:///workspace".to_owned(), request_id, deadline.clone())
            .await
            .expect("managed test run");
        let admitted = authority
            .try_managed_test_run_activity("file:///workspace/")
            .expect("idle authority");
        assert_eq!(
            authority
                .managed_test_run_progress("file:///workspace", &operation_id)
                .await,
            Some(ManagedTestRunProgress {
                completed: 0,
                total: None,
                deadline: deadline.clone(),
            })
        );

        emitter
            .test_result("suite::passes".to_owned(), true)
            .await
            .expect("test result");
        emitter.progress(1, Some(1)).await.expect("test progress");
        assert_eq!(
            authority
                .managed_test_run_progress("file:///workspace/", &operation_id)
                .await,
            Some(ManagedTestRunProgress {
                completed: 1,
                total: Some(1),
                deadline: deadline.clone(),
            })
        );
        let progressed = authority
            .try_managed_test_run_activity("file:///workspace")
            .expect("idle authority");
        assert_ne!(progressed, admitted);
        assert_eq!(
            authority
                .managed_test_run_progress("file:///workspace/other", &operation_id)
                .await,
            None,
            "a run is reported only under the root that admitted it"
        );

        let receipt = OperationReceipt::completed(
            UtcMicros(1),
            UtcMicros(2),
            deadline,
            OperationBudgetUsage::default(),
        )
        .expect("receipt");
        emitter.terminal(receipt).await.expect("terminal receipt");
        assert_eq!(
            authority
                .managed_test_run_progress("file:///workspace", &operation_id)
                .await,
            None,
            "a run that published its terminal receipt no longer executes"
        );
        assert_ne!(
            authority
                .try_managed_test_run_activity("file:///workspace")
                .expect("idle authority"),
            progressed
        );
    }

    /// A daemon composition shutting down in the same process (the harness
    /// runs many) expires the shared authority between a managed run's
    /// admission and its first result. The accepted record must survive
    /// while its producer is alive; only producer-less frontiers expire.
    #[tokio::test]
    async fn operation_history_keeps_the_accepted_record_for_a_live_producer_across_expiry() {
        let authority = OperationEventAuthority::default();
        let live_request = RequestId::new("request.test-run.live-producer").expect("request id");
        let live = authority
            .begin_managed_test_run(
                "file:///workspace/live".to_owned(),
                live_request.clone(),
                Deadline::new(UtcMicros(10_000)).expect("deadline"),
            )
            .await
            .expect("live managed test run");
        drop(
            authority
                .begin_managed_test_run(
                    "file:///workspace/abandoned".to_owned(),
                    RequestId::new("request.test-run.abandoned-producer").expect("request id"),
                    Deadline::new(UtcMicros(10_000)).expect("deadline"),
                )
                .await
                .expect("abandoned managed test run"),
        );

        authority.expire_idle().await;

        let first_result = live
            .test_result("suite::first".to_owned(), true)
            .await
            .expect("first result after a peer composition expired idle frontiers");
        assert_eq!(first_result.sequence, 1);
        let state = authority.inner.state.lock().await;
        let history = &state.operations[&OperationId::from_request(live_request)].history;
        assert!(
            matches!(
                history.front().map(|event| (event.sequence, &event.kind)),
                Some((
                    0,
                    StreamEventKind::Item(OperationEventItem::Accepted { .. })
                ))
            ),
            "the accepted record must precede the first result: {history:?}"
        );
        assert_eq!(history.len(), 2);
        assert!(
            !state.operations.contains_key(&OperationId::from_request(
                RequestId::new("request.test-run.abandoned-producer").expect("request id"),
            )),
            "a frontier without a live producer expires"
        );
        assert_eq!(state.insertion_order.len(), 1);
        drop(state);
        assert_eq!(
            authority
                .try_managed_test_run_activity("file:///workspace/abandoned")
                .expect("idle authority"),
            None
        );
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
}
