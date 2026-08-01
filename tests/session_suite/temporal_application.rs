use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use tracedecay::application::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use tracedecay::application::session::{
    AuthorizationGrantId, AuthorizedTemporalExecutionRequest, SessionAccess,
    SessionAuthorizationError, SessionAuthorizationGrant, SessionDataFreshness,
    SessionRequestBinding, SessionRetrievalConfiguration, SessionRetrievalOutcome,
    SessionRetrievalScope, SessionRetrievalService, SessionScopeAuthorizationRequest,
    SessionScopeAuthorizer, SessionTemporalExecutionError, SessionTemporalExecutionPort,
    SessionTemporalExecutionReport, SessionTemporalQuery,
};
use tracedecay::query::temporal::context::{
    CompactContext, ContextBudget, TokenPolicy, VersionedTokenEstimator,
};
use tracedecay::query::temporal::cursor::CursorError;
use tracedecay::query::temporal::ports::{
    BindingDigest, ExecutionLimits, KernelVersions, TemporalExecutionSnapshot,
    TemporalRetrievalScope, TemporalWatermarks,
};
use tracedecay::query::temporal::ranking::DiversityLimits;
use tracedecay::query::temporal::resolution::{SummaryLineageRejection, SummaryOmission};
use tracedecay::query::temporal::{
    TemporalKernelError, TemporalKernelRequest, TemporalKernelResult,
};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{
    ActorId, CompactContextBundleV1, CompactContextOmissionV1, ContextOmissionReasonV1, ProjectId,
    RepositoryId, RetrievalAnchorId, RetrievalGrainV1, SessionId, SessionSummaryIdV1,
    TemporalCoverageCountsV1, TemporalModeV1, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

const DIGEST: [u8; 32] = [0x5a; 32];

struct AllowAuthorizer;

impl SessionScopeAuthorizer for AllowAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.temporal.application").unwrap(),
            7,
            context,
            binding,
            request,
        )
    }
}

struct DenyAuthorizer;

impl SessionScopeAuthorizer for DenyAuthorizer {
    fn authorize(
        &self,
        _context: &RequestContext,
        _binding: &SessionRequestBinding,
        _request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        Err(SessionAuthorizationError::Denied)
    }
}

#[derive(Clone, Copy)]
struct GrantAuthorizer {
    id: &'static str,
    revision: u64,
}

impl SessionScopeAuthorizer for GrantAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new(self.id).unwrap(),
            self.revision,
            context,
            binding,
            request,
        )
    }
}

struct MismatchedGrantAuthorizer;

impl SessionScopeAuthorizer for MismatchedGrantAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        let mismatched = SessionScopeAuthorizationRequest::new(
            request.actor_id().clone(),
            request.identity().clone(),
            SessionId::new("session.other").unwrap(),
            request.provider_scope().map(str::to_owned),
            request.temporal_mode(),
            request.grain(),
            request.access(),
        )?;
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.mismatched").unwrap(),
            1,
            context,
            binding,
            &mismatched,
        )
    }
}

#[derive(Clone)]
struct ReplayedGrantAuthorizer(SessionAuthorizationGrant);

impl SessionScopeAuthorizer for ReplayedGrantAuthorizer {
    fn authorize(
        &self,
        _context: &RequestContext,
        _binding: &SessionRequestBinding,
        _request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        Ok(self.0.clone())
    }
}

struct CancellingAuthorizer;

impl SessionScopeAuthorizer for CancellingAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        let grant = SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.cancel-during-authorization").unwrap(),
            1,
            context,
            binding,
            request,
        )?;
        binding.cancellation().cancel();
        Ok(grant)
    }
}

struct DelayingAuthorizer(Duration);

impl SessionScopeAuthorizer for DelayingAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        std::thread::sleep(self.0);
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.deadline-during-authorization").unwrap(),
            1,
            context,
            binding,
            request,
        )
    }
}

type CapturedTarget = (
    SessionRetrievalScope,
    Option<String>,
    TemporalModeV1,
    RetrievalGrainV1,
    SessionAccess,
);

struct CapturingAuthorizer {
    target: Arc<Mutex<Option<CapturedTarget>>>,
}

impl SessionScopeAuthorizer for CapturingAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        *self.target.lock().unwrap() = Some((
            request.retrieval_scope().clone(),
            request.provider_scope().map(str::to_owned),
            request.temporal_mode(),
            request.grain(),
            request.access(),
        ));
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.captured").unwrap(),
            1,
            context,
            binding,
            request,
        )
    }
}

#[derive(Clone, Copy)]
struct Words(&'static str);

impl VersionedTokenEstimator for Words {
    fn version(&self) -> &str {
        self.0
    }

    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
    }
}

type ExecutionFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<SessionTemporalExecutionReport, SessionTemporalExecutionError>>
            + Send
            + 'a,
    >,
>;

struct FakeExecutionPort {
    calls: AtomicUsize,
    request_digests: Mutex<Vec<String>>,
    access_digests: Mutex<Vec<String>>,
    retrieval_scopes: Mutex<Vec<TemporalRetrievalScope>>,
    coverage: TemporalCoverageCountsV1,
    ranked_count: usize,
}

impl FakeExecutionPort {
    fn empty() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            request_digests: Mutex::new(Vec::new()),
            access_digests: Mutex::new(Vec::new()),
            retrieval_scopes: Mutex::new(Vec::new()),
            coverage: TemporalCoverageCountsV1::default(),
            ranked_count: 0,
        }
    }
}

impl SessionTemporalExecutionPort for FakeExecutionPort {
    fn execute<'a, E>(
        &'a self,
        request: AuthorizedTemporalExecutionRequest,
        _estimator: &'a E,
    ) -> ExecutionFuture<'a>
    where
        E: VersionedTokenEstimator + Sync + 'a,
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.retrieval_scopes
            .lock()
            .unwrap()
            .push(request.snapshot_request().retrieval_scope().clone());
        self.request_digests.lock().unwrap().push(
            request
                .snapshot_request()
                .request_digest()
                .as_str()
                .to_owned(),
        );
        self.access_digests.lock().unwrap().push(
            request
                .snapshot_request()
                .access_digest()
                .as_str()
                .to_owned(),
        );
        if request.cursor() == Some("forged") {
            return Box::pin(async {
                Err(SessionTemporalExecutionError::Kernel(
                    tracedecay::query::temporal::TemporalKernelError::Cursor(
                        tracedecay::query::temporal::cursor::CursorError::Tampered,
                    ),
                ))
            });
        }
        let execution_error = match request.query() {
            "execution-wrong-scope" => Some(SessionTemporalExecutionError::WrongScope),
            "execution-stale" => Some(SessionTemporalExecutionError::Stale { generation_lag: 3 }),
            "locked" => Some(SessionTemporalExecutionError::Locked),
            "execution-redacted" => Some(SessionTemporalExecutionError::Redacted),
            "execution-deleted" => Some(SessionTemporalExecutionError::Deleted),
            "execution-denied" => Some(SessionTemporalExecutionError::Denied),
            "execution-unavailable" => Some(SessionTemporalExecutionError::Unavailable),
            "execution-budget" => Some(SessionTemporalExecutionError::BudgetExhausted),
            "execution-cancelled" => Some(SessionTemporalExecutionError::Cancelled),
            _ => None,
        };
        if let Some(error) = execution_error {
            return Box::pin(async move { Err(error) });
        }
        if request.query() == "kernel-budget" {
            return Box::pin(async {
                Err(SessionTemporalExecutionError::Kernel(
                    TemporalKernelError::BudgetExceeded,
                ))
            });
        }
        if request.query() == "kernel-cancelled" {
            return Box::pin(async {
                Err(SessionTemporalExecutionError::Kernel(
                    TemporalKernelError::Cancelled,
                ))
            });
        }
        let cursor_error = match request.query() {
            "cursor-root" => Some(CursorError::RootMismatch),
            "cursor-session" => Some(CursorError::SessionMismatch),
            "cursor-access" => Some(CursorError::WrongAccess),
            "cursor-mode" => Some(CursorError::TemporalModeMismatch),
            "cursor-grain" => Some(CursorError::GrainMismatch),
            "cursor-malformed" => Some(CursorError::Malformed),
            "cursor-tampered" => Some(CursorError::Tampered),
            "cursor-sort-key" => Some(CursorError::SortKeyMismatch),
            "cursor-request" => Some(CursorError::WrongRequest),
            "cursor-schema" => Some(CursorError::SchemaMismatch),
            "cursor-ranking" => Some(CursorError::RankingMismatch),
            "cursor-configuration" => Some(CursorError::ConfigurationMismatch),
            "cursor-generation" => Some(CursorError::GenerationMismatch),
            "cursor-source" => Some(CursorError::SourceWatermarkMismatch),
            "cursor-projection" => Some(CursorError::ProjectionWatermarkMismatch),
            "cursor-index" => Some(CursorError::IndexWatermarkMismatch),
            "cursor-summary" => Some(CursorError::SummaryWatermarkMismatch),
            "cursor-key-id" => Some(CursorError::KeyIdMismatch),
            "cursor-key-version" => Some(CursorError::KeyVersionMismatch),
            "cursor-key-unavailable" => Some(CursorError::KeyUnavailable),
            "cursor-invalid-key" => Some(CursorError::InvalidKeyMaterial),
            _ => None,
        };
        if let Some(error) = cursor_error {
            return Box::pin(async move {
                Err(SessionTemporalExecutionError::Kernel(
                    TemporalKernelError::Cursor(error),
                ))
            });
        }
        let query = request.query().to_owned();
        let estimator_version = request.context_budget().estimator_version.clone();
        let mut coverage = self.coverage;
        let ranked_count = self.ranked_count;
        Box::pin(async move {
            let anchor = RetrievalAnchorId::new("anchor-omitted").unwrap();
            let mut omissions = Vec::new();
            let mut summary_omissions = Vec::new();
            let mut next_cursor = None;
            let mut freshness = SessionDataFreshness::Fresh;
            let omission_reason = match query.as_str() {
                "deleted" | "stored-deleted" => Some(ContextOmissionReasonV1::Deleted),
                "expired" => Some(ContextOmissionReasonV1::RetentionExpired),
                "redacted" => Some(ContextOmissionReasonV1::Redacted),
                "denied" => Some(ContextOmissionReasonV1::Unauthorized),
                "hydration-locked" => Some(ContextOmissionReasonV1::Locked),
                "unavailable" => Some(ContextOmissionReasonV1::Unavailable),
                "budget-bytes" => Some(ContextOmissionReasonV1::ByteBudget),
                _ => None,
            };
            if let Some(reason) = omission_reason {
                omissions.push(CompactContextOmissionV1 {
                    anchor_id: Some(anchor.clone()),
                    reason,
                });
                match reason {
                    ContextOmissionReasonV1::Unauthorized => coverage.hidden = 1,
                    ContextOmissionReasonV1::Redacted
                    | ContextOmissionReasonV1::Deleted
                    | ContextOmissionReasonV1::RetentionExpired => coverage.redacted = 1,
                    ContextOmissionReasonV1::ByteBudget
                    | ContextOmissionReasonV1::TokenBudget
                    | ContextOmissionReasonV1::Locked
                    | ContextOmissionReasonV1::Unavailable
                    | ContextOmissionReasonV1::SummaryHorizonMismatch
                    | ContextOmissionReasonV1::DuplicateRepresentative => coverage.unknown = 1,
                }
            }
            let summary_rejection = match query.as_str() {
                "summary-locked" => Some(SummaryLineageRejection::LockedSource {
                    anchor_id: anchor.clone(),
                }),
                "summary-deleted" => Some(SummaryLineageRejection::DeletedSource {
                    anchor_id: anchor.clone(),
                }),
                "summary-expired" => Some(SummaryLineageRejection::ExpiredSource {
                    anchor_id: anchor.clone(),
                }),
                "summary-redacted" => Some(SummaryLineageRejection::RedactedSource {
                    anchor_id: anchor.clone(),
                }),
                "summary-denied" => Some(SummaryLineageRejection::UnauthorizedSource {
                    anchor_id: anchor.clone(),
                }),
                "summary-stale" => Some(SummaryLineageRejection::StaleSource {
                    anchor_id: anchor.clone(),
                }),
                "summary-unavailable" => Some(SummaryLineageRejection::UnavailableSource {
                    anchor_id: anchor.clone(),
                }),
                "summary-missing" => Some(SummaryLineageRejection::MissingSource {
                    anchor_id: anchor.clone(),
                }),
                "summary-cycle" => Some(SummaryLineageRejection::CycleSource {
                    anchor_id: anchor.clone(),
                }),
                _ => None,
            };
            if let Some(rejection) = summary_rejection {
                summary_omissions.push(SummaryOmission {
                    summary_id: SessionSummaryIdV1::new("summary-omitted").unwrap(),
                    anchor_id: anchor.clone(),
                    rejection,
                });
                coverage.unknown = 1;
            }
            if query == "multiple-summary" {
                for index in 0..2 {
                    summary_omissions.push(SummaryOmission {
                        summary_id: SessionSummaryIdV1::new(format!("summary-omitted-{index}"))
                            .unwrap(),
                        anchor_id: RetrievalAnchorId::new(format!("anchor-omitted-{index}"))
                            .unwrap(),
                        rejection: SummaryLineageRejection::UnavailableSource {
                            anchor_id: RetrievalAnchorId::new(format!("anchor-omitted-{index}"))
                                .unwrap(),
                        },
                    });
                }
                coverage.unknown = 1;
            }
            if query == "partial-cursor" {
                next_cursor = Some("cursor.next".to_owned());
            }
            if matches!(query.as_str(), "stored" | "stored-deleted") {
                freshness = SessionDataFreshness::Stored { generation_lag: 2 };
            }
            let snapshot = TemporalExecutionSnapshot::new_authorized(
                request.snapshot_request().clone(),
                TemporalWatermarks {
                    generation: 1,
                    source: 2,
                    projection: 3,
                    index: 3,
                    summary: 4,
                },
                KernelVersions {
                    schema: request.schema_version(),
                    ranking: request.ranking_version(),
                    configuration_digest: BindingDigest::new(
                        "configuration_digest",
                        request.configuration_digest().to_owned(),
                    )
                    .unwrap(),
                },
                None,
                tracedecay::query::temporal::resolution::ValidatedAuthorization::Authorized,
            )
            .unwrap();
            let mut ranked = Vec::new();
            for index in 0..ranked_count {
                ranked.push(tracedecay::query::temporal::ranking::RankedCandidate {
                    stable_id: format!("candidate-{index}"),
                    anchor_id: tracedecay_domain::RetrievalAnchorId::new(format!("anchor-{index}"))
                        .unwrap(),
                    normalized_score_micros: 1,
                    knowledge_at_micros: 1,
                    logical_message: None,
                    turn: None,
                    session: None,
                    source: None,
                    evidence_role: None,
                    contributions: Vec::new(),
                });
            }
            Ok(SessionTemporalExecutionReport::new(
                TemporalKernelResult {
                    snapshot,
                    ranked,
                    hydrated: Vec::new(),
                    context: CompactContext {
                        rendered: String::new(),
                        bundle: CompactContextBundleV1 {
                            omissions,
                            coverage,
                            ..CompactContextBundleV1::default()
                        },
                        accounted_bytes: 0,
                        estimated_tokens: 0,
                        estimator_version,
                    },
                    coverage,
                    conflicts: Vec::new(),
                    lineage: Vec::new(),
                    summary_omissions,
                    next_cursor,
                },
                freshness,
            ))
        })
    }
}

struct PendingExecutionPort {
    dropped_after_cancel: Arc<AtomicBool>,
}

struct PendingExecution {
    control: tracedecay::query::temporal::ports::ExecutionControl,
    dropped_after_cancel: Arc<AtomicBool>,
}

impl Future for PendingExecution {
    type Output = Result<SessionTemporalExecutionReport, SessionTemporalExecutionError>;

    fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Pending
    }
}

impl Drop for PendingExecution {
    fn drop(&mut self) {
        self.dropped_after_cancel.store(
            matches!(
                self.control.checkpoint(),
                Err(tracedecay::query::temporal::ports::TemporalPortError::Cancelled)
            ),
            Ordering::SeqCst,
        );
    }
}

impl SessionTemporalExecutionPort for PendingExecutionPort {
    fn execute<'a, E>(
        &'a self,
        request: AuthorizedTemporalExecutionRequest,
        _estimator: &'a E,
    ) -> ExecutionFuture<'a>
    where
        E: VersionedTokenEstimator + Sync + 'a,
    {
        Box::pin(PendingExecution {
            control: request.snapshot_request().execution_control().clone(),
            dropped_after_cancel: Arc::clone(&self.dropped_after_cancel),
        })
    }
}

#[derive(Clone)]
struct TestRequestContext {
    request: RequestContext,
    binding: SessionRequestBinding,
}

impl TestRequestContext {
    fn binding(&self) -> &SessionRequestBinding {
        &self.binding
    }

    fn actor_id(&self) -> &ActorId {
        self.request.actor()
    }

    fn identity(&self) -> &ResolvedSessionIdentity {
        self.binding.identity()
    }

    fn capability_digest(&self) -> CapabilityDigest {
        self.binding.capability_digest()
    }

    fn policy_digest(&self) -> PolicyDigest {
        self.binding.policy_digest()
    }

    fn configuration_digest(&self) -> ConfigurationDigest {
        self.binding.configuration_digest()
    }

    fn deadline(&self) -> &Deadline {
        self.request.deadline()
    }

    fn cancellation(&self) -> &CancellationToken {
        self.binding.cancellation()
    }

    fn budgets(&self) -> RequestBudgets {
        self.binding.budgets()
    }
}

impl std::ops::Deref for TestRequestContext {
    type Target = RequestContext;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

fn context(root: &str) -> TestRequestContext {
    context_with(
        root,
        "request.temporal.application",
        RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
        DIGEST,
    )
}

fn context_with_controls(
    template: &TestRequestContext,
    request_id: &str,
    expires_at: UtcMicros,
    cancellation: CancellationToken,
    budgets: RequestBudgets,
) -> TestRequestContext {
    context_for_identity_with_controls(
        template.actor_id().as_str(),
        request_id,
        template.identity().clone(),
        template.capability_digest(),
        template.policy_digest(),
        template.configuration_digest(),
        cancellation,
        budgets,
        expires_at,
    )
}

fn context_with(
    root: &str,
    request_id: &str,
    budgets: RequestBudgets,
    configuration_digest: [u8; 32],
) -> TestRequestContext {
    context_with_policy(root, request_id, budgets, DIGEST, configuration_digest)
}

fn context_with_policy(
    root: &str,
    request_id: &str,
    budgets: RequestBudgets,
    policy_digest: [u8; 32],
    configuration_digest: [u8; 32],
) -> TestRequestContext {
    context_with_auth_digests(
        root,
        request_id,
        budgets,
        DIGEST,
        policy_digest,
        configuration_digest,
    )
}

fn context_with_auth_digests(
    root: &str,
    request_id: &str,
    budgets: RequestBudgets,
    capability_digest: [u8; 32],
    policy_digest: [u8; 32],
    configuration_digest: [u8; 32],
) -> TestRequestContext {
    context_for_actor_with_auth_digests(
        "actor.cursor",
        root,
        request_id,
        budgets,
        capability_digest,
        policy_digest,
        configuration_digest,
    )
}

#[allow(clippy::too_many_arguments)]
fn context_for_actor_with_auth_digests(
    actor_id: &str,
    root: &str,
    request_id: &str,
    budgets: RequestBudgets,
    capability_digest: [u8; 32],
    policy_digest: [u8; 32],
    configuration_digest: [u8; 32],
) -> TestRequestContext {
    context_for_identity(
        actor_id,
        request_id,
        ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.primary").unwrap(),
            ProjectId::new("project.tracedecay").unwrap(),
            SessionStoreId::new("store.project.tracedecay").unwrap(),
            SessionRootId::new(root).unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.tracedecay").unwrap(),
                WorktreeId::new("worktree.main").unwrap(),
                BranchId::new("branch.temporal-application").unwrap(),
            ),
        ),
        budgets,
        capability_digest,
        policy_digest,
        configuration_digest,
    )
}

#[allow(clippy::too_many_arguments)]
fn context_for_identity(
    actor_id: &str,
    request_id: &str,
    identity: ResolvedSessionIdentity,
    budgets: RequestBudgets,
    capability_digest: [u8; 32],
    policy_digest: [u8; 32],
    configuration_digest: [u8; 32],
) -> TestRequestContext {
    let request_id = RequestId::new(request_id).unwrap();
    context_for_identity_with_controls(
        actor_id,
        request_id.as_str(),
        identity,
        CapabilityDigest::new(capability_digest),
        PolicyDigest::new(policy_digest),
        ConfigurationDigest::new(configuration_digest),
        CancellationToken::for_application_request(request_id.as_str()),
        budgets,
        UtcMicros(application_observed_at().0.saturating_add(30_000_000)),
    )
}

#[allow(clippy::too_many_arguments)]
fn context_for_identity_with_controls(
    actor_id: &str,
    request_id: &str,
    identity: ResolvedSessionIdentity,
    capability: CapabilityDigest,
    policy: PolicyDigest,
    configuration: ConfigurationDigest,
    cancellation: CancellationToken,
    budgets: RequestBudgets,
    expires_at: UtcMicros,
) -> TestRequestContext {
    let actor = ActorId::new(actor_id).unwrap();
    let request_id = RequestId::new(request_id).unwrap();
    let scope = identity.application_scope().unwrap();
    let observed_at = application_observed_at();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.temporal.application.context").unwrap(),
        1,
        session_application_grant_digest(capability, policy, configuration, &cancellation, budgets)
            .unwrap(),
        actor.clone(),
        observed_at,
        UtcMicros(i64::MAX - 1),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.session.temporal-retrieval").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.session.temporal-retrieval").unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    let request = RequestContext::new(
        actor,
        scope,
        grant,
        request_id,
        Deadline::new(expires_at).unwrap(),
        CancellationContext::active(cancellation.application_token_id().unwrap()).unwrap(),
    )
    .unwrap();
    let binding = SessionRequestBinding::new(
        identity,
        capability,
        policy,
        configuration,
        cancellation,
        budgets,
    );
    TestRequestContext { request, binding }
}

fn query(text: &str) -> SessionTemporalQuery {
    query_with_mode(text, None, TemporalModeV1::Current)
}

fn query_with_mode(
    text: &str,
    cursor: Option<String>,
    temporal_mode: TemporalModeV1,
) -> SessionTemporalQuery {
    SessionTemporalQuery::new(
        SessionId::new("session.temporal.application").unwrap(),
        None,
        text,
        cursor,
        temporal_mode,
        RetrievalGrainV1::LogicalMessage,
        8,
        DiversityLimits::default(),
        ContextBudget {
            max_bytes: 64_000,
            max_tokens: 16_000,
            estimator_version: "words-v1".to_owned(),
        },
    )
    .unwrap()
}

#[derive(Clone)]
struct QuerySpec {
    session_id: &'static str,
    provider: Option<&'static str>,
    text: &'static str,
    cursor: Option<&'static str>,
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
    limit: usize,
    diversity: DiversityLimits,
    context_budget: ContextBudget,
    execution_limits: ExecutionLimits,
    freshness_policy: tracedecay::application::session::SessionFreshnessPolicy,
    retrieval_scope: Option<SessionRetrievalScope>,
}

impl Default for QuerySpec {
    fn default() -> Self {
        Self {
            session_id: "session.temporal.application",
            provider: None,
            text: "alpha",
            cursor: None,
            temporal_mode: TemporalModeV1::Current,
            grain: RetrievalGrainV1::LogicalMessage,
            limit: 8,
            diversity: DiversityLimits::default(),
            context_budget: ContextBudget {
                max_bytes: 64_000,
                max_tokens: 16_000,
                estimator_version: "words-v1".to_owned(),
            },
            execution_limits: ExecutionLimits::default(),
            freshness_policy: tracedecay::application::session::SessionFreshnessPolicy::AllowStored,
            retrieval_scope: None,
        }
    }
}

fn query_from_spec(spec: QuerySpec) -> SessionTemporalQuery {
    let query = SessionTemporalQuery::new(
        SessionId::new(spec.session_id).unwrap(),
        spec.provider.map(str::to_owned),
        spec.text,
        spec.cursor.map(str::to_owned),
        spec.temporal_mode,
        spec.grain,
        spec.limit,
        spec.diversity,
        spec.context_budget,
    )
    .unwrap()
    .with_execution_limits(spec.execution_limits)
    .with_freshness_policy(spec.freshness_policy);
    match spec.retrieval_scope {
        Some(scope) => query.with_retrieval_scope(scope),
        None => query,
    }
}

fn configuration() -> SessionRetrievalConfiguration {
    SessionRetrievalConfiguration::new(3, 5).unwrap()
}

async fn retrieve<A, P, E>(
    service: &SessionRetrievalService<A, P, E>,
    context: &TestRequestContext,
    query: SessionTemporalQuery,
) -> SessionRetrievalOutcome<TemporalKernelResult>
where
    A: SessionScopeAuthorizer,
    P: SessionTemporalExecutionPort,
    E: VersionedTokenEstimator + Sync,
{
    service.retrieve(context, context.binding(), query).await
}

async fn recorded_digest<A: SessionScopeAuthorizer>(
    authorizer: A,
    context: TestRequestContext,
    query: SessionTemporalQuery,
    estimator: Words,
    configuration: SessionRetrievalConfiguration,
) -> String {
    recorded_digests(authorizer, context, query, estimator, configuration)
        .await
        .0
}

async fn recorded_digests<A: SessionScopeAuthorizer>(
    authorizer: A,
    context: TestRequestContext,
    query: SessionTemporalQuery,
    estimator: Words,
    configuration: SessionRetrievalConfiguration,
) -> (String, String) {
    let port = FakeExecutionPort::empty();
    let service = SessionRetrievalService::new(authorizer, &port, estimator, configuration);
    let _ = retrieve(&service, &context, query).await;
    (
        port.request_digests.lock().unwrap()[0].clone(),
        port.access_digests.lock().unwrap()[0].clone(),
    )
}

#[tokio::test]
async fn canonical_request_digest_drifts_for_query_and_root_changes() {
    let port = FakeExecutionPort::empty();
    let service =
        SessionRetrievalService::new(AllowAuthorizer, &port, Words("words-v1"), configuration());

    let first = retrieve(&service, &context("root.one"), query("alpha")).await;
    let second = retrieve(&service, &context("root.one"), query("beta")).await;
    let third = retrieve(&service, &context("root.two"), query("alpha")).await;

    assert!(matches!(
        first,
        SessionRetrievalOutcome::CompleteZero { .. }
    ));
    assert!(matches!(
        second,
        SessionRetrievalOutcome::CompleteZero { .. }
    ));
    assert!(matches!(
        third,
        SessionRetrievalOutcome::CompleteZero { .. }
    ));
    let digests = port.request_digests.lock().unwrap();
    assert_ne!(digests[0], digests[1]);
    assert_ne!(digests[0], digests[2]);
}

#[tokio::test]
async fn application_authorizes_and_validates_the_exact_retrieval_target() {
    let captured = Arc::new(Mutex::new(None));
    let port = FakeExecutionPort::empty();
    let service = SessionRetrievalService::new(
        CapturingAuthorizer {
            target: Arc::clone(&captured),
        },
        &port,
        Words("words-v1"),
        configuration(),
    );
    let spec = QuerySpec {
        provider: Some("cursor"),
        temporal_mode: TemporalModeV1::AsOf {
            cutoff: UtcMicros(77),
        },
        grain: RetrievalGrainV1::Summary,
        ..QuerySpec::default()
    };

    let outcome = retrieve(&service, &context("root.one"), query_from_spec(spec)).await;

    assert!(matches!(
        outcome,
        SessionRetrievalOutcome::CompleteZero { .. }
    ));
    assert_eq!(
        captured.lock().unwrap().clone().unwrap(),
        (
            SessionRetrievalScope::Session(SessionId::new("session.temporal.application").unwrap()),
            Some("cursor".to_owned()),
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(77)
            },
            RetrievalGrainV1::Summary,
            SessionAccess::Hydrate,
        )
    );

    let rejected_port = FakeExecutionPort::empty();
    let rejected_service = SessionRetrievalService::new(
        MismatchedGrantAuthorizer,
        &rejected_port,
        Words("words-v1"),
        configuration(),
    );
    assert!(matches!(
        retrieve(&rejected_service, &context("root.one"), query("alpha")).await,
        SessionRetrievalOutcome::WrongScope
    ));
    assert_eq!(rejected_port.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn application_binds_and_freezes_root_wide_retrieval_scope() {
    let captured = Arc::new(Mutex::new(None));
    let port = FakeExecutionPort::empty();
    let service = SessionRetrievalService::new(
        CapturingAuthorizer {
            target: Arc::clone(&captured),
        },
        &port,
        Words("words-v1"),
        configuration(),
    );
    let query = query_from_spec(QuerySpec {
        provider: Some("cursor"),
        retrieval_scope: Some(SessionRetrievalScope::AllSessionsInAuthorizedRoot),
        ..QuerySpec::default()
    });

    assert!(matches!(
        retrieve(&service, &context("root.one"), query).await,
        SessionRetrievalOutcome::CompleteZero { .. }
    ));
    assert!(matches!(
        captured.lock().unwrap().as_ref().unwrap().0,
        SessionRetrievalScope::AllSessionsInAuthorizedRoot
    ));
    assert_eq!(
        port.retrieval_scopes.lock().unwrap().as_slice(),
        &[TemporalRetrievalScope::AllSessionsInAuthorizedRoot]
    );
}

#[tokio::test]
async fn canonical_digest_binds_every_semantic_input_and_excludes_resume_ephemera() {
    let (baseline, baseline_access) = recorded_digests(
        GrantAuthorizer {
            id: "grant.baseline",
            revision: 1,
        },
        context("root.one"),
        query_from_spec(QuerySpec::default()),
        Words("words-v1"),
        configuration(),
    )
    .await;

    let root_wide = recorded_digest(
        GrantAuthorizer {
            id: "grant.baseline",
            revision: 1,
        },
        context("root.one"),
        query_from_spec(QuerySpec {
            retrieval_scope: Some(SessionRetrievalScope::AllSessionsInAuthorizedRoot),
            ..QuerySpec::default()
        }),
        Words("words-v1"),
        configuration(),
    )
    .await;
    assert_ne!(baseline, root_wide);
    assert_eq!(
        root_wide,
        recorded_digest(
            GrantAuthorizer {
                id: "grant.baseline",
                revision: 1,
            },
            context("root.one"),
            query_from_spec(QuerySpec {
                session_id: "session.compatibility-anchor.changed",
                retrieval_scope: Some(SessionRetrievalScope::AllSessionsInAuthorizedRoot),
                ..QuerySpec::default()
            }),
            Words("words-v1"),
            configuration(),
        )
        .await
    );

    for authorizer in [
        GrantAuthorizer {
            id: "grant.changed",
            revision: 1,
        },
        GrantAuthorizer {
            id: "grant.baseline",
            revision: 2,
        },
    ] {
        let (request_digest, access_digest) = recorded_digests(
            authorizer,
            context("root.one"),
            query_from_spec(QuerySpec::default()),
            Words("words-v1"),
            configuration(),
        )
        .await;
        assert_ne!(baseline, request_digest);
        assert_eq!(baseline_access, access_digest);
    }

    let mut semantic_variants = Vec::new();
    semantic_variants.push(QuerySpec {
        session_id: "session.changed",
        ..QuerySpec::default()
    });
    semantic_variants.push(QuerySpec {
        provider: Some("cursor"),
        ..QuerySpec::default()
    });
    semantic_variants.push(QuerySpec {
        text: "beta",
        ..QuerySpec::default()
    });
    for temporal_mode in [
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(17),
        },
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(18),
        },
    ] {
        semantic_variants.push(QuerySpec {
            temporal_mode,
            ..QuerySpec::default()
        });
    }
    semantic_variants.push(QuerySpec {
        grain: RetrievalGrainV1::Occurrence,
        ..QuerySpec::default()
    });
    semantic_variants.push(QuerySpec {
        limit: 9,
        ..QuerySpec::default()
    });
    semantic_variants.push(QuerySpec {
        freshness_policy: tracedecay::application::session::SessionFreshnessPolicy::RequireFresh,
        ..QuerySpec::default()
    });

    for index in 0..5 {
        let mut diversity = DiversityLimits::default();
        match index {
            0 => diversity.per_logical_message += 1,
            1 => diversity.per_turn += 1,
            2 => diversity.per_session += 1,
            3 => diversity.per_source += 1,
            4 => diversity.per_evidence_role += 1,
            _ => unreachable!(),
        }
        semantic_variants.push(QuerySpec {
            diversity,
            ..QuerySpec::default()
        });
    }

    for index in 0..15 {
        let mut limits = ExecutionLimits::default();
        match index {
            0 => limits.candidate_limit += 1,
            1 => limits.candidate_total_bytes += 1,
            2 => limits.candidate_item_bytes += 1,
            3 => limits.candidate_key_bytes += 1,
            4 => limits.candidate_stable_id_bytes += 1,
            5 => limits.candidate_anchor_id_bytes += 1,
            6 => limits.candidate_metadata_field_bytes += 1,
            7 => limits.record_limit += 1,
            8 => limits.record_total_bytes += 1,
            9 => limits.record_item_bytes += 1,
            10 => limits.record_key_bytes += 1,
            11 => limits.hydration_limit += 1,
            12 => limits.hydration_total_bytes += 1,
            13 => limits.hydration_payload_bytes += 1,
            14 => limits.hydration_chunk_bytes += 1,
            _ => unreachable!(),
        }
        semantic_variants.push(QuerySpec {
            execution_limits: limits,
            ..QuerySpec::default()
        });
    }
    semantic_variants.push(QuerySpec {
        context_budget: ContextBudget {
            max_bytes: 64_001,
            ..QuerySpec::default().context_budget
        },
        ..QuerySpec::default()
    });
    semantic_variants.push(QuerySpec {
        context_budget: ContextBudget {
            max_tokens: 16_001,
            ..QuerySpec::default().context_budget
        },
        ..QuerySpec::default()
    });
    semantic_variants.push(QuerySpec {
        context_budget: ContextBudget {
            estimator_version: "words-v2".to_owned(),
            ..QuerySpec::default().context_budget
        },
        ..QuerySpec::default()
    });

    for spec in semantic_variants {
        let estimator = Words(if spec.context_budget.estimator_version == "words-v2" {
            "words-v2"
        } else {
            "words-v1"
        });
        assert_ne!(
            baseline,
            recorded_digest(
                GrantAuthorizer {
                    id: "grant.baseline",
                    revision: 1,
                },
                context("root.one"),
                query_from_spec(spec),
                estimator,
                configuration(),
            )
            .await
        );
    }

    for (budgets, configuration_digest) in [
        (
            RequestBudgets::new(65, 64 * 1024 * 1024, 10_000).unwrap(),
            DIGEST,
        ),
        (
            RequestBudgets::new(64, 64 * 1024 * 1024 + 1, 10_000).unwrap(),
            DIGEST,
        ),
        (
            RequestBudgets::new(64, 64 * 1024 * 1024, 10_001).unwrap(),
            DIGEST,
        ),
        (
            RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
            [0x5b; 32],
        ),
    ] {
        assert_ne!(
            baseline,
            recorded_digest(
                GrantAuthorizer {
                    id: "grant.baseline",
                    revision: 1,
                },
                context_with(
                    "root.one",
                    "request.semantic",
                    budgets,
                    configuration_digest
                ),
                query_from_spec(QuerySpec::default()),
                Words("words-v1"),
                configuration(),
            )
            .await
        );
    }
    for (capability_digest, policy_digest) in [([0x5b; 32], DIGEST), (DIGEST, [0x5b; 32])] {
        let (request_digest, access_digest) = recorded_digests(
            GrantAuthorizer {
                id: "grant.baseline",
                revision: 1,
            },
            context_with_auth_digests(
                "root.one",
                "request.semantic",
                RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
                capability_digest,
                policy_digest,
                DIGEST,
            ),
            query_from_spec(QuerySpec::default()),
            Words("words-v1"),
            configuration(),
        )
        .await;
        assert_ne!(baseline, request_digest);
        if policy_digest == DIGEST {
            assert_eq!(baseline_access, access_digest);
        } else {
            assert_ne!(baseline_access, access_digest);
        }
    }
    for configuration in [
        SessionRetrievalConfiguration::new(4, 5).unwrap(),
        SessionRetrievalConfiguration::new(3, 6).unwrap(),
    ] {
        assert_ne!(
            baseline,
            recorded_digest(
                GrantAuthorizer {
                    id: "grant.baseline",
                    revision: 1,
                },
                context("root.one"),
                query_from_spec(QuerySpec::default()),
                Words("words-v1"),
                configuration,
            )
            .await
        );
    }
    assert_ne!(
        baseline,
        recorded_digest(
            GrantAuthorizer {
                id: "grant.baseline",
                revision: 1,
            },
            context("root.two"),
            query_from_spec(QuerySpec::default()),
            Words("words-v1"),
            configuration(),
        )
        .await
    );
    let identity_variants = [
        ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.other").unwrap(),
            ProjectId::new("project.tracedecay").unwrap(),
            SessionStoreId::new("store.project.tracedecay").unwrap(),
            SessionRootId::new("root.one").unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.tracedecay").unwrap(),
                WorktreeId::new("worktree.main").unwrap(),
                BranchId::new("branch.temporal-application").unwrap(),
            ),
        ),
        ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.primary").unwrap(),
            ProjectId::new("project.other").unwrap(),
            SessionStoreId::new("store.project.tracedecay").unwrap(),
            SessionRootId::new("root.one").unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.tracedecay").unwrap(),
                WorktreeId::new("worktree.main").unwrap(),
                BranchId::new("branch.temporal-application").unwrap(),
            ),
        ),
        ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.primary").unwrap(),
            ProjectId::new("project.tracedecay").unwrap(),
            SessionStoreId::new("store.project.other").unwrap(),
            SessionRootId::new("root.one").unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.tracedecay").unwrap(),
                WorktreeId::new("worktree.main").unwrap(),
                BranchId::new("branch.temporal-application").unwrap(),
            ),
        ),
        ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.primary").unwrap(),
            ProjectId::new("project.tracedecay").unwrap(),
            SessionStoreId::new("store.project.tracedecay").unwrap(),
            SessionRootId::new("root.one").unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.other").unwrap(),
                WorktreeId::new("worktree.main").unwrap(),
                BranchId::new("branch.temporal-application").unwrap(),
            ),
        ),
        ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.primary").unwrap(),
            ProjectId::new("project.tracedecay").unwrap(),
            SessionStoreId::new("store.project.tracedecay").unwrap(),
            SessionRootId::new("root.one").unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.tracedecay").unwrap(),
                WorktreeId::new("worktree.other").unwrap(),
                BranchId::new("branch.temporal-application").unwrap(),
            ),
        ),
        ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.primary").unwrap(),
            ProjectId::new("project.tracedecay").unwrap(),
            SessionStoreId::new("store.project.tracedecay").unwrap(),
            SessionRootId::new("root.one").unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.tracedecay").unwrap(),
                WorktreeId::new("worktree.main").unwrap(),
                BranchId::new("branch.other").unwrap(),
            ),
        ),
        ResolvedSessionIdentity::for_profile(
            ProfileId::new("profile.primary").unwrap(),
            SessionStoreId::new("store.project.tracedecay").unwrap(),
            SessionRootId::new("root.one").unwrap(),
        ),
    ];
    for identity in identity_variants {
        assert_ne!(
            baseline,
            recorded_digest(
                GrantAuthorizer {
                    id: "grant.baseline",
                    revision: 1,
                },
                context_for_identity(
                    "actor.cursor",
                    "request.identity-semantic",
                    identity,
                    RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
                    DIGEST,
                    DIGEST,
                    DIGEST,
                ),
                query_from_spec(QuerySpec::default()),
                Words("words-v1"),
                configuration(),
            )
            .await
        );
    }

    let (other_actor_request, other_actor_access) = recorded_digests(
        GrantAuthorizer {
            id: "grant.baseline",
            revision: 1,
        },
        context_for_actor_with_auth_digests(
            "actor.other",
            "root.one",
            "request.semantic",
            RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
            DIGEST,
            DIGEST,
            DIGEST,
        ),
        query_from_spec(QuerySpec::default()),
        Words("words-v1"),
        configuration(),
    )
    .await;
    assert_ne!(baseline, other_actor_request);
    assert_eq!(baseline_access, other_actor_access);

    let ephemeral_context = context_with(
        "root.one",
        "request.ephemeral",
        RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
        DIGEST,
    );
    let resumed = QuerySpec {
        cursor: Some("opaque-resume-cursor"),
        ..QuerySpec::default()
    };
    assert_eq!(
        baseline,
        recorded_digest(
            GrantAuthorizer {
                id: "grant.baseline",
                revision: 1,
            },
            ephemeral_context,
            query_from_spec(resumed),
            Words("words-v1"),
            configuration(),
        )
        .await
    );
}

#[tokio::test]
async fn denial_never_reaches_temporal_execution_or_payload_hydration() {
    let port = Arc::new(FakeExecutionPort::empty());
    let service = SessionRetrievalService::new(
        DenyAuthorizer,
        Arc::clone(&port),
        Words("words-v1"),
        configuration(),
    );

    assert!(matches!(
        retrieve(&service, &context("root.one"), query("alpha")).await,
        SessionRetrievalOutcome::Denied
    ));
    assert!(matches!(
        retrieve(
            &service,
            &context("root.one"),
            query_from_spec(QuerySpec {
                retrieval_scope: Some(SessionRetrievalScope::AllSessionsInAuthorizedRoot),
                ..QuerySpec::default()
            }),
        )
        .await,
        SessionRetrievalOutcome::Denied
    ));
    assert_eq!(port.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn replayed_grant_cannot_escape_its_deadline_cancellation_or_budgets() {
    let issued_context = context("root.one");
    let authorization = SessionScopeAuthorizationRequest::new(
        issued_context.actor_id().clone(),
        issued_context.identity().clone(),
        SessionId::new("session.temporal.application").unwrap(),
        None,
        TemporalModeV1::Current,
        RetrievalGrainV1::LogicalMessage,
        SessionAccess::Hydrate,
    )
    .unwrap();
    let grant = AllowAuthorizer
        .authorize(&issued_context, issued_context.binding(), &authorization)
        .unwrap();

    let replay_contexts = [
        context_with_controls(
            &issued_context,
            "request.replay-deadline",
            UtcMicros(issued_context.deadline().expires_at.0.saturating_add(1)),
            issued_context.cancellation().clone(),
            issued_context.budgets(),
        ),
        context_with_controls(
            &issued_context,
            "request.replay-cancellation",
            issued_context.deadline().expires_at,
            CancellationToken::for_application_request(
                RequestId::new("request.replay-cancellation")
                    .unwrap()
                    .as_str(),
            ),
            issued_context.budgets(),
        ),
        context_with_controls(
            &issued_context,
            "request.replay-budgets",
            issued_context.deadline().expires_at,
            issued_context.cancellation().clone(),
            RequestBudgets::new(65, 64 * 1024 * 1024, 10_000).unwrap(),
        ),
    ];

    for replay_context in replay_contexts {
        let port = FakeExecutionPort::empty();
        let service = SessionRetrievalService::new(
            ReplayedGrantAuthorizer(grant.clone()),
            &port,
            Words("words-v1"),
            configuration(),
        );
        assert!(matches!(
            retrieve(&service, &replay_context, query("alpha")).await,
            SessionRetrievalOutcome::Denied
        ));
        assert_eq!(port.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn cancellation_or_deadline_during_authorization_prevents_execution_construction() {
    let cancellation_port = FakeExecutionPort::empty();
    let cancellation_service = SessionRetrievalService::new(
        CancellingAuthorizer,
        &cancellation_port,
        Words("words-v1"),
        configuration(),
    );
    assert!(matches!(
        retrieve(&cancellation_service, &context("root.one"), query("alpha")).await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert_eq!(cancellation_port.calls.load(Ordering::SeqCst), 0);

    let template = context("root.one");
    let deadline_context = context_with_controls(
        &template,
        "request.deadline-during-authorization",
        UtcMicros(application_observed_at().0.saturating_add(1_000)),
        template.cancellation().clone(),
        template.budgets(),
    );
    let deadline_port = FakeExecutionPort::empty();
    let deadline_service = SessionRetrievalService::new(
        DelayingAuthorizer(Duration::from_millis(10)),
        &deadline_port,
        Words("words-v1"),
        configuration(),
    );
    assert!(matches!(
        retrieve(&deadline_service, &deadline_context, query("alpha")).await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert!(application_observed_at() >= deadline_context.deadline().expires_at);
    assert_eq!(deadline_port.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn request_budget_preflight_rejects_before_execution() {
    let port = FakeExecutionPort::empty();
    let service =
        SessionRetrievalService::new(AllowAuthorizer, &port, Words("words-v1"), configuration());
    let constrained = context_with(
        "root.one",
        "request.constrained-budget",
        RequestBudgets::new(1, 64 * 1024 * 1024, 10_000).unwrap(),
        DIGEST,
    );

    assert!(matches!(
        retrieve(&service, &constrained, query("alpha")).await,
        SessionRetrievalOutcome::BudgetExhausted
    ));
    assert_eq!(port.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mode_cutoff_and_forged_cursor_are_bound_and_typed() {
    let port = FakeExecutionPort::empty();
    let service =
        SessionRetrievalService::new(AllowAuthorizer, &port, Words("words-v1"), configuration());

    let _ = retrieve(
        &service,
        &context("root.one"),
        query_with_mode(
            "alpha",
            None,
            TemporalModeV1::AsOf {
                cutoff: tracedecay_domain::UtcMicros(17),
            },
        ),
    )
    .await;
    let _ = retrieve(
        &service,
        &context("root.one"),
        query_with_mode("alpha", None, TemporalModeV1::Evolution),
    )
    .await;
    {
        let digests = port.request_digests.lock().unwrap();
        assert_ne!(digests[0], digests[1]);
    }
    let _ = retrieve(&service, &context("root.one"), query("alpha")).await;
    assert!(matches!(
        retrieve(
            &service,
            &context("root.one"),
            query_with_mode("alpha", Some("forged".to_owned()), TemporalModeV1::Current),
        )
        .await,
        SessionRetrievalOutcome::Denied
    ));
    let digests = port.request_digests.lock().unwrap();
    assert_eq!(digests[2], digests[3]);
}

#[tokio::test]
async fn coverage_matrix_preserves_partial_locked_and_cancelled_outcomes() {
    let partial_port = FakeExecutionPort {
        calls: AtomicUsize::new(0),
        request_digests: Mutex::new(Vec::new()),
        access_digests: Mutex::new(Vec::new()),
        retrieval_scopes: Mutex::new(Vec::new()),
        coverage: TemporalCoverageCountsV1 {
            visible: 1,
            hidden: 0,
            unknown: 2,
            redacted: 0,
        },
        ranked_count: 1,
    };
    let partial_service = SessionRetrievalService::new(
        AllowAuthorizer,
        &partial_port,
        Words("words-v1"),
        configuration(),
    );
    assert!(matches!(
        retrieve(&partial_service, &context("root.one"), query("alpha")).await,
        SessionRetrievalOutcome::Partial { omitted: 2, .. }
    ));

    let locked_port = FakeExecutionPort::empty();
    let locked_service = SessionRetrievalService::new(
        AllowAuthorizer,
        &locked_port,
        Words("words-v1"),
        configuration(),
    );
    assert!(matches!(
        retrieve(&locked_service, &context("root.one"), query("locked")).await,
        SessionRetrievalOutcome::Locked
    ));

    let cancelled_context = context("root.one");
    cancelled_context.cancellation().cancel();
    assert!(matches!(
        retrieve(&locked_service, &cancelled_context, query("alpha")).await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert!(matches!(
        retrieve(
            &locked_service,
            &cancelled_context,
            query_from_spec(QuerySpec {
                retrieval_scope: Some(SessionRetrievalScope::AllSessionsInAuthorizedRoot),
                ..QuerySpec::default()
            }),
        )
        .await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert_eq!(locked_port.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn typed_omission_and_cursor_states_do_not_collapse_to_complete_zero_or_wrong_scope() {
    let port = FakeExecutionPort::empty();
    let service =
        SessionRetrievalService::new(AllowAuthorizer, &port, Words("words-v1"), configuration());

    for text in ["deleted", "expired", "summary-deleted", "summary-expired"] {
        assert!(matches!(
            retrieve(&service, &context("root.one"), query(text)).await,
            SessionRetrievalOutcome::Deleted
        ));
    }
    assert!(matches!(
        retrieve(&service, &context("root.one"), query("execution-deleted")).await,
        SessionRetrievalOutcome::Deleted
    ));
    for text in ["redacted", "summary-redacted"] {
        assert!(matches!(
            retrieve(&service, &context("root.one"), query(text)).await,
            SessionRetrievalOutcome::Redacted
        ));
    }
    for text in ["denied", "summary-denied"] {
        assert!(matches!(
            retrieve(&service, &context("root.one"), query(text)).await,
            SessionRetrievalOutcome::Denied
        ));
    }
    assert!(matches!(
        retrieve(&service, &context("root.one"), query("execution-redacted")).await,
        SessionRetrievalOutcome::Redacted
    ));
    assert!(matches!(
        retrieve(&service, &context("root.one"), query("execution-denied")).await,
        SessionRetrievalOutcome::Denied
    ));
    assert!(matches!(
        retrieve(&service, &context("root.one"), query("summary-locked")).await,
        SessionRetrievalOutcome::Locked
    ));
    assert!(matches!(
        retrieve(&service, &context("root.one"), query("hydration-locked")).await,
        SessionRetrievalOutcome::Locked
    ));
    for text in [
        "unavailable",
        "summary-stale",
        "summary-unavailable",
        "summary-missing",
        "summary-cycle",
        "cursor-request",
        "cursor-schema",
        "cursor-ranking",
        "cursor-configuration",
        "cursor-generation",
        "cursor-source",
        "cursor-projection",
        "cursor-index",
        "cursor-summary",
        "cursor-key-id",
        "cursor-key-version",
        "cursor-key-unavailable",
        "cursor-invalid-key",
    ] {
        assert!(matches!(
            retrieve(&service, &context("root.one"), query(text)).await,
            SessionRetrievalOutcome::Unavailable
        ));
    }
    for text in [
        "cursor-root",
        "cursor-session",
        "cursor-access",
        "cursor-mode",
        "cursor-grain",
    ] {
        assert!(matches!(
            retrieve(&service, &context("root.one"), query(text)).await,
            SessionRetrievalOutcome::WrongScope
        ));
    }
    for text in ["cursor-malformed", "cursor-tampered", "cursor-sort-key"] {
        assert!(matches!(
            retrieve(&service, &context("root.one"), query(text)).await,
            SessionRetrievalOutcome::Denied
        ));
    }
    assert!(matches!(
        retrieve(&service, &context("root.one"), query("budget-bytes")).await,
        SessionRetrievalOutcome::BudgetExhausted
    ));
    assert!(matches!(
        retrieve(&service, &context("root.one"), query("kernel-budget")).await,
        SessionRetrievalOutcome::BudgetExhausted
    ));
    assert!(matches!(
        retrieve(&service, &context("root.one"), query("kernel-cancelled")).await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert!(matches!(
        retrieve(
            &service,
            &context("root.one"),
            query("execution-wrong-scope")
        )
        .await,
        SessionRetrievalOutcome::WrongScope
    ));
    assert!(matches!(
        retrieve(&service, &context("root.one"), query("execution-stale")).await,
        SessionRetrievalOutcome::Stale {
            freshness: SessionDataFreshness::Stored { generation_lag: 3 }
        }
    ));
    assert!(matches!(
        retrieve(
            &service,
            &context("root.one"),
            query("execution-unavailable")
        )
        .await,
        SessionRetrievalOutcome::Unavailable
    ));
    assert!(matches!(
        retrieve(&service, &context("root.one"), query("execution-budget")).await,
        SessionRetrievalOutcome::BudgetExhausted
    ));
    assert!(matches!(
        retrieve(&service, &context("root.one"), query("execution-cancelled")).await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert!(matches!(
        retrieve(
            &service,
            &context("root.one"),
            query_from_spec(QuerySpec {
                text: "stored-deleted",
                freshness_policy:
                    tracedecay::application::session::SessionFreshnessPolicy::RequireFresh,
                ..QuerySpec::default()
            }),
        )
        .await,
        SessionRetrievalOutcome::Deleted
    ));

    let visible_without_items = FakeExecutionPort {
        calls: AtomicUsize::new(0),
        request_digests: Mutex::new(Vec::new()),
        access_digests: Mutex::new(Vec::new()),
        retrieval_scopes: Mutex::new(Vec::new()),
        coverage: TemporalCoverageCountsV1 {
            visible: 1,
            ..TemporalCoverageCountsV1::default()
        },
        ranked_count: 0,
    };
    let incomplete_service = SessionRetrievalService::new(
        AllowAuthorizer,
        &visible_without_items,
        Words("words-v1"),
        configuration(),
    );
    assert!(matches!(
        retrieve(&incomplete_service, &context("root.one"), query("alpha")).await,
        SessionRetrievalOutcome::Unavailable
    ));
}

#[tokio::test]
async fn partial_freshness_and_cancellation_race_preserve_application_ownership() {
    let partial_port = FakeExecutionPort {
        calls: AtomicUsize::new(0),
        request_digests: Mutex::new(Vec::new()),
        access_digests: Mutex::new(Vec::new()),
        retrieval_scopes: Mutex::new(Vec::new()),
        coverage: TemporalCoverageCountsV1 {
            visible: 1,
            ..TemporalCoverageCountsV1::default()
        },
        ranked_count: 1,
    };
    let partial_service = SessionRetrievalService::new(
        AllowAuthorizer,
        &partial_port,
        Words("words-v1"),
        configuration(),
    );
    assert!(matches!(
        retrieve(
            &partial_service,
            &context("root.one"),
            query("partial-cursor")
        )
        .await,
        SessionRetrievalOutcome::Partial { .. }
    ));
    assert!(matches!(
        retrieve(
            &partial_service,
            &context("root.one"),
            query("multiple-summary")
        )
        .await,
        SessionRetrievalOutcome::Partial { omitted: 2, .. }
    ));
    assert!(matches!(
        retrieve(
            &partial_service,
            &context("root.one"),
            query_from_spec(QuerySpec {
                text: "stored",
                freshness_policy:
                    tracedecay::application::session::SessionFreshnessPolicy::RequireFresh,
                ..QuerySpec::default()
            }),
        )
        .await,
        SessionRetrievalOutcome::Stale {
            freshness: SessionDataFreshness::Stored { generation_lag: 2 }
        }
    ));

    let dropped_after_cancel = Arc::new(AtomicBool::new(false));
    let pending = PendingExecutionPort {
        dropped_after_cancel: Arc::clone(&dropped_after_cancel),
    };
    let pending_service =
        SessionRetrievalService::new(AllowAuthorizer, pending, Words("words-v1"), configuration());
    let pending_context = context("root.one");
    let cancellation = pending_context.cancellation().clone();
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        cancellation.cancel();
    });

    assert!(matches!(
        retrieve(&pending_service, &pending_context, query("alpha")).await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert!(dropped_after_cancel.load(Ordering::SeqCst));
}

/// Compile-time only: the temporal request and session-access types must stay
/// nameable and constructible from outside the crate. Nothing here observes
/// behaviour, so this test fails by failing to compile.
#[test]
fn temporal_application_api_is_publicly_composed_at_compile_time() {
    fn assert_request(_: &TemporalKernelRequest) {}
    let _ = assert_request;
    let _: SessionAccess = SessionAccess::Hydrate;
}
