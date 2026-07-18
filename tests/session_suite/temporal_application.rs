use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay::application::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, MonotonicDeadline,
    PolicyDigest, ProfileId, RequestBudgets, RequestContext, RequestId, ResolvedGitRoute,
    ResolvedSessionIdentity, SessionRootId, SessionStoreId,
};
use tracedecay::application::session::{
    AuthorizationGrantId, AuthorizedTemporalExecutionRequest, SessionAccess,
    SessionAuthorizationError, SessionAuthorizationGrant, SessionDataFreshness,
    SessionRetrievalConfiguration, SessionRetrievalOutcome, SessionRetrievalScope,
    SessionRetrievalService, SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
    SessionTemporalExecutionError, SessionTemporalExecutionPort, SessionTemporalExecutionReport,
    SessionTemporalQuery,
};
use tracedecay::global_db::GlobalDbSessionTemporalExecution;
use tracedecay::query::temporal::context::{
    CompactContext, ContextBudget, VersionedTokenEstimator,
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
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    ActorId, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CompactContextBundleV1, CompactContextOmissionV1, ContextOmissionReasonV1,
    DurableObservationV1, MessageOccurrenceIdV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectId, ProjectionGenerationId, ProjectionOutputOrdinalV1, ProviderId,
    RepositoryId, RetentionClass, RetrievalAnchorId, RetrievalAnchorRecord, RetrievalGrainV1,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId, SessionSummaryIdV1, TemporalCoverageCountsV1, TemporalModeV1,
    UtcMicros, WorktreeId,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationStore, ObservationWrite,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use crate::common::{isolated_lcm_db_path, open_lcm_db};

const DIGEST: [u8; 32] = [0x5a; 32];

struct AllowAuthorizer;

impl SessionScopeAuthorizer for AllowAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.temporal.application").unwrap(),
            7,
            context,
            request,
        )
    }
}

struct DenyAuthorizer;

impl SessionScopeAuthorizer for DenyAuthorizer {
    fn authorize(
        &self,
        _context: &RequestContext,
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
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new(self.id).unwrap(),
            self.revision,
            context,
            request,
        )
    }
}

struct MismatchedGrantAuthorizer;

impl SessionScopeAuthorizer for MismatchedGrantAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
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
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        let grant = SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.cancel-during-authorization").unwrap(),
            1,
            context,
            request,
        )?;
        context.cancellation().cancel();
        Ok(grant)
    }
}

struct DelayingAuthorizer(Duration);

impl SessionScopeAuthorizer for DelayingAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        std::thread::sleep(self.0);
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.deadline-during-authorization").unwrap(),
            1,
            context,
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

    fn estimate(&self, text: &str) -> u64 {
        text.split_whitespace().count() as u64
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
                });
            }
            Ok(SessionTemporalExecutionReport::new(
                TemporalKernelResult {
                    snapshot,
                    ranked,
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

fn context(root: &str) -> RequestContext {
    context_with(
        root,
        "request.temporal.application",
        RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
        DIGEST,
    )
}

fn context_with_controls(
    template: &RequestContext,
    request_id: &str,
    deadline: MonotonicDeadline,
    cancellation: CancellationToken,
    budgets: RequestBudgets,
) -> RequestContext {
    RequestContext::new(
        template.actor_id().clone(),
        RequestId::new(request_id).unwrap(),
        template.identity().clone(),
        template.capability_digest(),
        template.policy_digest(),
        template.configuration_digest(),
        deadline,
        cancellation,
        budgets,
    )
}

fn context_with(
    root: &str,
    request_id: &str,
    budgets: RequestBudgets,
    configuration_digest: [u8; 32],
) -> RequestContext {
    context_with_policy(root, request_id, budgets, DIGEST, configuration_digest)
}

fn context_with_policy(
    root: &str,
    request_id: &str,
    budgets: RequestBudgets,
    policy_digest: [u8; 32],
    configuration_digest: [u8; 32],
) -> RequestContext {
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
) -> RequestContext {
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
) -> RequestContext {
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
) -> RequestContext {
    RequestContext::new(
        ActorId::new(actor_id).unwrap(),
        RequestId::new(request_id).unwrap(),
        identity,
        CapabilityDigest::new(capability_digest),
        PolicyDigest::new(policy_digest),
        ConfigurationDigest::new(configuration_digest),
        MonotonicDeadline::at(Instant::now() + Duration::from_secs(30)),
        CancellationToken::new(),
        budgets,
    )
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

async fn recorded_digest<A: SessionScopeAuthorizer>(
    authorizer: A,
    context: RequestContext,
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
    context: RequestContext,
    query: SessionTemporalQuery,
    estimator: Words,
    configuration: SessionRetrievalConfiguration,
) -> (String, String) {
    let port = FakeExecutionPort::empty();
    let service = SessionRetrievalService::new(authorizer, &port, estimator, configuration);
    let _ = service.retrieve(&context, query).await;
    (
        port.request_digests.lock().unwrap()[0].clone(),
        port.access_digests.lock().unwrap()[0].clone(),
    )
}

fn fixture_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn authoritative_fixture_hashes(root: &Path) -> BTreeMap<String, String> {
    let mut hashes = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with("-shm"))
            {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            hashes.insert(relative, fixture_hash(&fs::read(path).unwrap()));
        }
    }
    hashes
}

fn fixture_receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.application-fixture.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn fixture_observation(ordinal: u64) -> DurableObservationV1 {
    fixture_observation_for(
        ordinal,
        "session.temporal.application",
        "provider.application",
        &format!("message-{ordinal}"),
        &format!("record-{ordinal}"),
        &format!("receipt-{ordinal}"),
        &format!("fixture payload {ordinal}"),
    )
}

#[allow(clippy::too_many_arguments)]
fn fixture_observation_for(
    ordinal: u64,
    session_id: &str,
    provider: &str,
    message_id: &str,
    record_id: &str,
    receipt_id: &str,
    content: &str,
) -> DurableObservationV1 {
    let session_id = SessionId::new(session_id).unwrap();
    let provider = ProviderId::new(provider).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
    let message_id = ObservationId::new(message_id).unwrap();
    let record_id = ObservationId::new(record_id).unwrap();
    let relations = CanonicalObservationRelationsV1::new(session_id).with_message_id(message_id);
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": content}),
            model: None,
            timestamp: Some(ordinal as i64),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        fixture_receipt(receipt_id, &payload),
        RetentionClass::new("retention.application-fixture").unwrap(),
        payload,
    )
    .unwrap()
}

async fn persist_fixture_anchor(
    db: &tracedecay::global_db::GlobalDb,
    ordinal: u64,
) -> (DurableObservationV1, RetrievalAnchorRecord) {
    let observation = fixture_observation(ordinal);
    persist_fixture_observation(db, ordinal, observation).await
}

async fn persist_fixture_observation(
    db: &tracedecay::global_db::GlobalDb,
    ordinal: u64,
    observation: DurableObservationV1,
) -> (DurableObservationV1, RetrievalAnchorRecord) {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    let expected_cursor = (ordinal > 1).then(|| {
        ObservationSourceCursorV1::for_ordering(
            observation.source().clone(),
            observation.scope().clone(),
            identity.generation(),
            identity.ordering_domain(),
            ordinal,
        )
        .unwrap()
    });
    let write = ObservationWrite::new(observation.clone(), expected_cursor, next_cursor).unwrap();
    let projection = ProjectionGenerationId::new("projection.application-fixture.v1").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "application-fixture")
            .unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    GlobalDbObservationStore::new(db)
        .persist_observation(
            AnchoredObservationWrite::new(write, anchor.clone(), projection).unwrap(),
        )
        .await
        .unwrap();
    (observation, anchor)
}

fn policy_digest_bytes(anchor: &RetrievalAnchorRecord) -> [u8; 32] {
    let encoded = anchor
        .authorization()
        .access_policy_digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap();
    let decoded = hex::decode(encoded).unwrap();
    decoded.try_into().unwrap()
}

#[tokio::test]
async fn canonical_request_digest_drifts_for_query_and_root_changes() {
    let port = FakeExecutionPort::empty();
    let service =
        SessionRetrievalService::new(AllowAuthorizer, &port, Words("words-v1"), configuration());

    let first = service.retrieve(&context("root.one"), query("alpha")).await;
    let second = service.retrieve(&context("root.one"), query("beta")).await;
    let third = service.retrieve(&context("root.two"), query("alpha")).await;

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

    let outcome = service
        .retrieve(&context("root.one"), query_from_spec(spec))
        .await;

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
        rejected_service
            .retrieve(&context("root.one"), query("alpha"))
            .await,
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
        service.retrieve(&context("root.one"), query).await,
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
        service.retrieve(&context("root.one"), query("alpha")).await,
        SessionRetrievalOutcome::Denied
    ));
    assert!(matches!(
        service
            .retrieve(
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
        .authorize(&issued_context, &authorization)
        .unwrap();

    let replay_contexts = [
        context_with_controls(
            &issued_context,
            "request.replay-deadline",
            MonotonicDeadline::at(issued_context.deadline().instant() + Duration::from_secs(1)),
            issued_context.cancellation().clone(),
            issued_context.budgets(),
        ),
        context_with_controls(
            &issued_context,
            "request.replay-cancellation",
            issued_context.deadline(),
            CancellationToken::new(),
            issued_context.budgets(),
        ),
        context_with_controls(
            &issued_context,
            "request.replay-budgets",
            issued_context.deadline(),
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
            service.retrieve(&replay_context, query("alpha")).await,
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
        cancellation_service
            .retrieve(&context("root.one"), query("alpha"))
            .await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert_eq!(cancellation_port.calls.load(Ordering::SeqCst), 0);

    let template = context("root.one");
    let deadline_context = context_with_controls(
        &template,
        "request.deadline-during-authorization",
        MonotonicDeadline::at(Instant::now() + Duration::from_millis(1)),
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
        deadline_service
            .retrieve(&deadline_context, query("alpha"))
            .await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert!(deadline_context.deadline().is_elapsed_at(Instant::now()));
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
        service.retrieve(&constrained, query("alpha")).await,
        SessionRetrievalOutcome::BudgetExhausted
    ));
    assert_eq!(port.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mode_cutoff_and_forged_cursor_are_bound_and_typed() {
    let port = FakeExecutionPort::empty();
    let service =
        SessionRetrievalService::new(AllowAuthorizer, &port, Words("words-v1"), configuration());

    let _ = service
        .retrieve(
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
    let _ = service
        .retrieve(
            &context("root.one"),
            query_with_mode("alpha", None, TemporalModeV1::Evolution),
        )
        .await;
    {
        let digests = port.request_digests.lock().unwrap();
        assert_ne!(digests[0], digests[1]);
    }
    let _ = service.retrieve(&context("root.one"), query("alpha")).await;
    assert!(matches!(
        service
            .retrieve(
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
        partial_service
            .retrieve(&context("root.one"), query("alpha"))
            .await,
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
        locked_service
            .retrieve(&context("root.one"), query("locked"))
            .await,
        SessionRetrievalOutcome::Locked
    ));

    let cancelled_context = context("root.one");
    cancelled_context.cancellation().cancel();
    assert!(matches!(
        locked_service
            .retrieve(&cancelled_context, query("alpha"))
            .await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert!(matches!(
        locked_service
            .retrieve(
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
            service.retrieve(&context("root.one"), query(text)).await,
            SessionRetrievalOutcome::Deleted
        ));
    }
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("execution-deleted"))
            .await,
        SessionRetrievalOutcome::Deleted
    ));
    for text in ["redacted", "summary-redacted"] {
        assert!(matches!(
            service.retrieve(&context("root.one"), query(text)).await,
            SessionRetrievalOutcome::Redacted
        ));
    }
    for text in ["denied", "summary-denied"] {
        assert!(matches!(
            service.retrieve(&context("root.one"), query(text)).await,
            SessionRetrievalOutcome::Denied
        ));
    }
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("execution-redacted"))
            .await,
        SessionRetrievalOutcome::Redacted
    ));
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("execution-denied"))
            .await,
        SessionRetrievalOutcome::Denied
    ));
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("summary-locked"))
            .await,
        SessionRetrievalOutcome::Locked
    ));
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("hydration-locked"))
            .await,
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
            service.retrieve(&context("root.one"), query(text)).await,
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
            service.retrieve(&context("root.one"), query(text)).await,
            SessionRetrievalOutcome::WrongScope
        ));
    }
    for text in ["cursor-malformed", "cursor-tampered", "cursor-sort-key"] {
        assert!(matches!(
            service.retrieve(&context("root.one"), query(text)).await,
            SessionRetrievalOutcome::Denied
        ));
    }
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("budget-bytes"))
            .await,
        SessionRetrievalOutcome::BudgetExhausted
    ));
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("kernel-budget"))
            .await,
        SessionRetrievalOutcome::BudgetExhausted
    ));
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("kernel-cancelled"))
            .await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("execution-wrong-scope"))
            .await,
        SessionRetrievalOutcome::WrongScope
    ));
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("execution-stale"))
            .await,
        SessionRetrievalOutcome::Stale {
            freshness: SessionDataFreshness::Stored { generation_lag: 3 }
        }
    ));
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("execution-unavailable"))
            .await,
        SessionRetrievalOutcome::Unavailable
    ));
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("execution-budget"))
            .await,
        SessionRetrievalOutcome::BudgetExhausted
    ));
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("execution-cancelled"))
            .await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert!(matches!(
        service
            .retrieve(
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
        incomplete_service
            .retrieve(&context("root.one"), query("alpha"))
            .await,
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
        partial_service
            .retrieve(&context("root.one"), query("partial-cursor"))
            .await,
        SessionRetrievalOutcome::Partial { .. }
    ));
    assert!(matches!(
        partial_service
            .retrieve(&context("root.one"), query("multiple-summary"))
            .await,
        SessionRetrievalOutcome::Partial { omitted: 2, .. }
    ));
    assert!(matches!(
        partial_service
            .retrieve(
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
        pending_service
            .retrieve(&pending_context, query("alpha"))
            .await,
        SessionRetrievalOutcome::Cancelled
    ));
    assert!(dropped_after_cancel.load(Ordering::SeqCst));
}

#[tokio::test]
async fn production_root_scope_isolated_filtered_restartable_and_read_only() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let duplicate_payload = "duplicate root-wide payload";
    let fixtures = [
        (
            "session.root.a",
            "provider.application",
            "record-root-a",
            "receipt-root-a",
        ),
        (
            "session.root.b",
            "provider.other",
            "record-root-b",
            "receipt-root-b",
        ),
    ];
    let mut persisted = Vec::new();
    for (session_id, provider, record_id, receipt_id) in fixtures {
        let observation = fixture_observation_for(
            1,
            session_id,
            provider,
            "duplicate-message",
            record_id,
            receipt_id,
            duplicate_payload,
        );
        persisted.push((
            session_id,
            provider,
            receipt_id,
            persist_fixture_observation(&db, 1, observation).await,
        ));
    }
    let policy_digest = policy_digest_bytes(&persisted[0].3.1);
    assert_eq!(policy_digest, policy_digest_bytes(&persisted[1].3.1));

    let fixture_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let fixture = fixture_db.connect().unwrap();
    fixture
        .execute(
            "INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES ('application-root-key', 1, ?1, 1, NULL)",
            [vec![0x45_u8; 32]],
        )
        .await
        .unwrap();
    let frozen = json!({
        "active_generation": 1,
        "cursor_key": {"key_id": "application-root-key", "version": 1},
        "projection_frontier": 0,
        "source_frontier": 0,
        "summary_frontier": 0
    })
    .to_string();
    for (session_id, provider, receipt_id, (observation, anchor)) in &persisted {
        fixture
            .execute(
                "INSERT INTO sessions (provider, session_id, project_key, project_path)
                 VALUES (?1, ?2, 'user', '/fixture')",
                libsql::params![*provider, *session_id],
            )
            .await
            .unwrap();
        fixture
            .execute(
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref,
                    snippet_text, index_text, legacy_source, legacy_truncated
                 ) VALUES (
                    ?1, 'duplicate-message', ?2, 'assistant', 1, 1,
                    ?3, ?4, 'inline', NULL, ?3, ?3, 0, 0
                 )",
                libsql::params![
                    *provider,
                    *session_id,
                    duplicate_payload,
                    fixture_hash(duplicate_payload.as_bytes())
                ],
            )
            .await
            .unwrap();
        fixture
            .execute(
                "INSERT INTO session_temporal_generations (
                    session_id, generation, state, frozen_watermarks_json, created_at,
                    ready_at, activated_at, completed_at
                 ) VALUES (?1, 1, 'building', ?2, 1, NULL, NULL, NULL)",
                libsql::params![*session_id, frozen.as_str()],
            )
            .await
            .unwrap();
        fixture
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'ready', ready_at = 1
                 WHERE session_id = ?1 AND generation = 1",
                [*session_id],
            )
            .await
            .unwrap();
        fixture
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'active', activated_at = 1
                 WHERE session_id = ?1 AND generation = 1",
                [*session_id],
            )
            .await
            .unwrap();

        let occurrence_id = MessageOccurrenceIdV1::derive(
            observation.observation_id(),
            ProjectionOutputOrdinalV1::new(0),
        );
        let evidence = json!({
            "authority": "provider_native",
            "evidence_class": "provider_declared",
            "source_anchor_id": anchor.anchor_id(),
            "sanitization_receipt": {
                "receipt_id": receipt_id,
                "sanitizer_version": "sanitizer.application-fixture.v1"
            }
        })
        .to_string();
        fixture
            .execute(
                "INSERT INTO session_occurrences (
                    session_id, generation, occurrence_id, source_observation_id,
                    projection_output_ordinal, retrieval_anchor_id, message_id,
                    role, knowledge_at, valid_time_json, evidence_json,
                    snippet_text, index_text
                 ) VALUES (
                    ?1, 1, ?2, ?3, 0, ?4, 'duplicate-message',
                    'assistant', 1, ?5, ?6, ?7, ?7
                 )",
                libsql::params![
                    *session_id,
                    occurrence_id.as_str(),
                    observation.observation_id().as_str(),
                    anchor.anchor_id().as_str(),
                    json!({"kind": "known", "valid_at": 1}).to_string(),
                    evidence,
                    duplicate_payload
                ],
            )
            .await
            .unwrap();
        fixture
            .execute(
                "INSERT INTO session_current_entities (
                    session_id, generation, entity_kind, entity_id,
                    current_assertion_id, current_occurrence_id, coverage_json
                 ) VALUES (
                    ?1, 1, 'occurrence_anchor', ?2, NULL, ?3,
                    '{\"occurrence_count\":1}'
                 )",
                libsql::params![
                    *session_id,
                    anchor.anchor_id().as_str(),
                    occurrence_id.as_str()
                ],
            )
            .await
            .unwrap();
    }
    drop(fixture);
    drop(fixture_db);

    let before = Sha256::digest(fs::read(&db_path).unwrap());
    let before_files = authoritative_fixture_hashes(tmp.path());
    let root_context = context_with_policy(
        "root.one",
        "request.production-root",
        RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
        policy_digest,
        DIGEST,
    );
    let execution = GlobalDbSessionTemporalExecution::new(&db);
    let service = SessionRetrievalService::new(
        AllowAuthorizer,
        execution,
        Words("words-v1"),
        configuration(),
    );
    let root_query =
        |anchor_session: &str, provider: Option<&str>, cursor: Option<String>, limit| {
            SessionTemporalQuery::new(
                SessionId::new(anchor_session).unwrap(),
                provider.map(str::to_owned),
                "duplicate",
                cursor,
                TemporalModeV1::Current,
                RetrievalGrainV1::Occurrence,
                limit,
                DiversityLimits::unbounded(),
                ContextBudget {
                    max_bytes: 64_000,
                    max_tokens: 16_000,
                    estimator_version: "words-v1".to_owned(),
                },
            )
            .unwrap()
            .with_retrieval_scope(SessionRetrievalScope::AllSessionsInAuthorizedRoot)
        };

    let all = service
        .retrieve(&root_context, root_query("session.root.a", None, None, 8))
        .await;
    let SessionRetrievalOutcome::Complete { items, .. } = all else {
        panic!("root-wide retrieval was not complete: {all:?}");
    };
    assert_eq!(items[0].ranked.len(), 2);
    assert_eq!(items[0].coverage.visible, 2);
    assert_eq!(items[0].coverage.unknown, 0);
    assert!(items[0].lineage.is_empty());
    assert!(items[0].context.rendered.contains(duplicate_payload));
    let all_anchors = items[0]
        .ranked
        .iter()
        .map(|item| item.anchor_id.to_string())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        items[0]
            .ranked
            .iter()
            .filter_map(|item| item.session.as_deref())
            .collect::<std::collections::BTreeSet<_>>(),
        ["session.root.a", "session.root.b"].into()
    );

    let filtered = service
        .retrieve(
            &root_context,
            root_query("session.root.a", Some("provider.application"), None, 8),
        )
        .await;
    let SessionRetrievalOutcome::Complete { items, .. } = filtered else {
        panic!("provider-filtered root retrieval was not complete: {filtered:?}");
    };
    assert_eq!(items[0].ranked.len(), 1);
    assert_eq!(
        items[0].ranked[0].session.as_deref(),
        Some("session.root.a")
    );
    assert!(items[0].context.rendered.contains(duplicate_payload));
    let filtered_anchor = items[0].ranked[0].anchor_id.clone();

    let exact = service
        .retrieve(
            &root_context,
            query_from_spec(QuerySpec {
                session_id: "session.root.a",
                provider: Some("provider.application"),
                text: "duplicate",
                grain: RetrievalGrainV1::Occurrence,
                diversity: DiversityLimits::unbounded(),
                ..QuerySpec::default()
            }),
        )
        .await;
    let SessionRetrievalOutcome::Complete { items, .. } = exact else {
        panic!("exact-session parity retrieval was not complete: {exact:?}");
    };
    assert_eq!(items[0].ranked.len(), 1);
    assert_eq!(items[0].ranked[0].anchor_id, filtered_anchor);
    assert!(items[0].context.rendered.contains(duplicate_payload));

    let first_page = service
        .retrieve(&root_context, root_query("session.root.a", None, None, 1))
        .await;
    let SessionRetrievalOutcome::Partial { items, omitted, .. } = first_page else {
        panic!("first root page was not partial: {first_page:?}");
    };
    assert_eq!(omitted, 1);
    let first_anchor = items[0].ranked[0].anchor_id.to_string();
    let cursor = items[0].next_cursor.clone().expect("root continuation");

    let restarted_execution = GlobalDbSessionTemporalExecution::new(&db);
    let restarted = SessionRetrievalService::new(
        AllowAuthorizer,
        restarted_execution,
        Words("words-v1"),
        configuration(),
    );
    let resumed = restarted
        .retrieve(
            &root_context,
            root_query("session.root.b", None, Some(cursor.clone()), 1),
        )
        .await;
    let SessionRetrievalOutcome::Complete { items, .. } = resumed else {
        panic!("resumed root page was not complete: {resumed:?}");
    };
    let resumed_anchor = items[0].ranked[0].anchor_id.to_string();
    assert_eq!(
        [first_anchor, resumed_anchor]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        all_anchors
    );

    assert!(matches!(
        restarted
            .retrieve(
                &root_context,
                SessionTemporalQuery::new(
                    SessionId::new("session.root.a").unwrap(),
                    None,
                    "duplicate",
                    Some(cursor.clone()),
                    TemporalModeV1::Current,
                    RetrievalGrainV1::Occurrence,
                    1,
                    DiversityLimits::unbounded(),
                    ContextBudget {
                        max_bytes: 64_000,
                        max_tokens: 16_000,
                        estimator_version: "words-v1".to_owned(),
                    },
                )
                .unwrap(),
            )
            .await,
        SessionRetrievalOutcome::WrongScope
    ));
    let other_root = context_with_policy(
        "root.other",
        "request.production-root-drift",
        RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
        policy_digest,
        DIGEST,
    );
    assert!(matches!(
        restarted
            .retrieve(
                &other_root,
                root_query("session.root.b", None, Some(cursor), 1),
            )
            .await,
        SessionRetrievalOutcome::WrongScope
    ));
    assert_eq!(Sha256::digest(fs::read(&db_path).unwrap()), before);
    assert_eq!(authoritative_fixture_hashes(tmp.path()), before_files);
}

#[tokio::test]
async fn production_occurrence_hydration_is_nonempty_and_external_payload_is_immutable() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let (inline_observation, inline_anchor) = persist_fixture_anchor(&db, 1).await;
    let (external_observation, external_anchor) = persist_fixture_anchor(&db, 2).await;
    let (_, authority_anchor) = persist_fixture_anchor(&db, 3).await;
    let policy_digest = policy_digest_bytes(&inline_anchor);
    assert_eq!(
        policy_digest,
        policy_digest_bytes(&external_anchor),
        "one authority namespace must produce one access policy"
    );

    let fixture_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let fixture = fixture_db.connect().unwrap();
    fixture
        .execute(
            "INSERT INTO sessions (provider, session_id, project_key, project_path)
             VALUES ('provider.application', 'session.temporal.application', 'user', '/fixture')",
            (),
        )
        .await
        .unwrap();
    let inline_payload = "non-empty inline occurrence payload";
    fixture
        .execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref,
                snippet_text, index_text, legacy_source, legacy_truncated
             ) VALUES (
                'provider.application', 'message-1', 'session.temporal.application',
                'assistant', 1, 1, ?1, ?2, 'inline', NULL, ?1, ?1, 0, 0
             )",
            libsql::params![inline_payload, fixture_hash(inline_payload.as_bytes())],
        )
        .await
        .unwrap();

    let external_payload = "non-empty external occurrence payload";
    let external_hash = fixture_hash(external_payload.as_bytes());
    let payload_ref = "application-fixture.bin";
    let payload_dir = db_path.parent().unwrap().join("lcm-payloads");
    fs::create_dir(&payload_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&payload_dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let payload_path = payload_dir.join(payload_ref);
    fs::write(&payload_path, external_payload).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&payload_path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fixture
        .execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref,
                snippet_text, index_text, legacy_source, legacy_truncated
             ) VALUES (
                'provider.application', 'message-2', 'session.temporal.application',
                'assistant', 2, 2, NULL, ?1, 'external', ?2, ?3, ?3, 0, 0
             )",
            libsql::params![external_hash.as_str(), payload_ref, external_payload],
        )
        .await
        .unwrap();
    fixture
        .execute(
            "INSERT INTO lcm_external_payloads (
                payload_ref, provider, session_id, message_id, kind,
                content_hash, byte_count, char_count, created_at
             ) VALUES (
                ?1, 'provider.application', 'session.temporal.application',
                'message-2', 'message', ?2, ?3, ?4, 1
             )",
            libsql::params![
                payload_ref,
                external_hash.as_str(),
                i64::try_from(external_payload.len()).unwrap(),
                i64::try_from(external_payload.chars().count()).unwrap()
            ],
        )
        .await
        .unwrap();
    let external_manifest = json!({
        "provider": "provider.application",
        "session_id": "session.temporal.application",
        "message_id": "message-2",
        "byte_count": external_payload.len(),
        "char_count": external_payload.chars().count()
    })
    .to_string();
    let authority_publication = json!({
        "receipt_id": "receipt-3",
        "payloads": [{
            "payload_ref": payload_ref,
            "digest": external_hash,
            "manifest_json": external_manifest
        }]
    })
    .to_string();
    fixture
        .execute(
            "INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text,
                index_text, source_horizon_json, publication_json, created_at
             ) VALUES (
                'summary-external-authority', 'session.temporal.application', ?1,
                'payload authority', 'payload authority', '{}', ?2, 1
             )",
            libsql::params![
                authority_anchor.anchor_id().as_str(),
                authority_publication.as_str()
            ],
        )
        .await
        .unwrap();
    fixture
        .execute(
            "INSERT INTO session_external_payload_manifests (
                payload_ref, session_id, payload_digest, manifest_json, receipt_id, created_at
             ) VALUES (?1, 'session.temporal.application', ?2, ?3, 'receipt-3', 1)",
            libsql::params![
                payload_ref,
                external_hash.as_str(),
                external_manifest.as_str()
            ],
        )
        .await
        .unwrap();
    fixture
        .execute(
            "INSERT INTO session_query_cursor_keys (
                key_id, key_version, key_material, created_at, retired_at
             ) VALUES ('application-key', 1, ?1, 1, NULL)",
            [vec![0x44_u8; 32]],
        )
        .await
        .unwrap();
    let frozen = json!({
        "active_generation": 1,
        "cursor_key": {"key_id": "application-key", "version": 1},
        "projection_frontier": 0,
        "source_frontier": 0,
        "summary_frontier": 0
    })
    .to_string();
    fixture
        .execute(
            "INSERT INTO session_temporal_generations (
                session_id, generation, state, frozen_watermarks_json, created_at,
                ready_at, activated_at, completed_at
             ) VALUES (
                'session.temporal.application', 1, 'building', ?1, 1,
                NULL, NULL, NULL
             )",
            [frozen],
        )
        .await
        .unwrap();
    fixture
        .execute(
            "UPDATE session_temporal_generations
             SET state = 'ready', ready_at = 1
             WHERE session_id = 'session.temporal.application' AND generation = 1",
            (),
        )
        .await
        .unwrap();
    fixture
        .execute(
            "UPDATE session_temporal_generations
             SET state = 'active', activated_at = 1
             WHERE session_id = 'session.temporal.application' AND generation = 1",
            (),
        )
        .await
        .unwrap();

    for (ordinal, observation, anchor, message_id, payload) in [
        (
            1_i64,
            &inline_observation,
            &inline_anchor,
            "message-1",
            inline_payload,
        ),
        (
            2_i64,
            &external_observation,
            &external_anchor,
            "message-2",
            external_payload,
        ),
    ] {
        let occurrence_id = MessageOccurrenceIdV1::derive(
            observation.observation_id(),
            ProjectionOutputOrdinalV1::new(0),
        );
        let evidence = json!({
            "authority": "provider_native",
            "evidence_class": "provider_declared",
            "source_anchor_id": anchor.anchor_id(),
            "sanitization_receipt": {
                "receipt_id": format!("receipt-{ordinal}"),
                "sanitizer_version": "sanitizer.application-fixture.v1"
            }
        })
        .to_string();
        fixture
            .execute(
                "INSERT INTO session_occurrences (
                    session_id, generation, occurrence_id, source_observation_id,
                    projection_output_ordinal, retrieval_anchor_id, message_id,
                    role, knowledge_at, valid_time_json, evidence_json,
                    snippet_text, index_text
                 ) VALUES (
                    'session.temporal.application', 1, ?1, ?2, 0, ?3, ?4,
                    'assistant', ?5, ?6, ?7, ?8, ?8
                 )",
                libsql::params![
                    occurrence_id.as_str(),
                    observation.observation_id().as_str(),
                    anchor.anchor_id().as_str(),
                    message_id,
                    ordinal,
                    json!({"kind": "known", "valid_at": ordinal}).to_string(),
                    evidence,
                    payload
                ],
            )
            .await
            .unwrap();
        fixture
            .execute(
                "INSERT INTO session_current_entities (
                    session_id, generation, entity_kind, entity_id,
                    current_assertion_id, current_occurrence_id, coverage_json
                 ) VALUES (
                    'session.temporal.application', 1, 'occurrence_anchor', ?1,
                    NULL, ?2, '{\"occurrence_count\":1}'
                 )",
                libsql::params![anchor.anchor_id().as_str(), occurrence_id.as_str()],
            )
            .await
            .unwrap();
    }
    drop(fixture);
    drop(fixture_db);

    let before_db = Sha256::digest(fs::read(&db_path).unwrap());
    let before_payload = Sha256::digest(fs::read(&payload_path).unwrap());
    let before_files = authoritative_fixture_hashes(tmp.path());
    let execution = GlobalDbSessionTemporalExecution::new(&db);
    let service = SessionRetrievalService::new(
        AllowAuthorizer,
        execution,
        Words("words-v1"),
        configuration(),
    );
    for temporal_mode in [
        TemporalModeV1::Current,
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(10),
        },
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
    ] {
        let outcome = service
            .retrieve(
                &context_with_policy(
                    "root.one",
                    "request.production-fixture",
                    RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
                    policy_digest,
                    DIGEST,
                ),
                query_from_spec(QuerySpec {
                    provider: Some("provider.application"),
                    text: "inline",
                    temporal_mode,
                    grain: RetrievalGrainV1::Occurrence,
                    ..QuerySpec::default()
                }),
            )
            .await;
        let SessionRetrievalOutcome::Complete { items, .. } = outcome else {
            panic!("production non-empty retrieval failed for {temporal_mode:?}: {outcome:?}");
        };
        assert_eq!(items[0].ranked.len(), 1);
        assert!(items[0].context.rendered.contains(inline_payload));
    }
    let external = service
        .retrieve(
            &context_with_policy(
                "root.one",
                "request.production-external-payload",
                RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
                policy_digest,
                DIGEST,
            ),
            query_from_spec(QuerySpec {
                provider: Some("provider.application"),
                text: "external",
                grain: RetrievalGrainV1::Occurrence,
                ..QuerySpec::default()
            }),
        )
        .await;
    let SessionRetrievalOutcome::Complete { items, .. } = external else {
        panic!("production external-payload retrieval was not complete: {external:?}");
    };
    assert_eq!(items[0].ranked.len(), 1);
    assert!(items[0].context.rendered.contains(external_payload));
    assert_eq!(Sha256::digest(fs::read(&db_path).unwrap()), before_db);
    assert_eq!(
        Sha256::digest(fs::read(&payload_path).unwrap()),
        before_payload
    );
    assert_eq!(authoritative_fixture_hashes(tmp.path()), before_files);
}

#[tokio::test]
async fn production_complete_zero_preserves_authoritative_database_hash() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let fixture_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let fixture = fixture_db.connect().unwrap();
    fixture
        .execute(
            "INSERT INTO session_query_cursor_keys
                 (key_id, key_version, key_material, created_at, retired_at)
             VALUES (?1, 1, ?2, 1, NULL)",
            libsql::params!["application-key", vec![0x44_u8; 32]],
        )
        .await
        .unwrap();
    fixture
        .execute_batch(&format!(
            "INSERT INTO session_temporal_generations
                 (session_id, generation, state, frozen_watermarks_json, created_at,
                  ready_at, activated_at, completed_at)
             VALUES ('session.temporal.application', 1, 'building', '{}', 1, NULL, NULL, NULL);
             UPDATE session_temporal_generations
             SET state = 'ready', ready_at = 1,
                 frozen_watermarks_json = '{}'
             WHERE session_id = 'session.temporal.application' AND generation = 1;
             UPDATE session_temporal_generations
             SET state = 'active', activated_at = 1
             WHERE session_id = 'session.temporal.application' AND generation = 1;",
            serde_json::json!({
                "active_generation": 1,
                "cursor_key": {"key_id": "application-key", "version": 1},
                "projection_frontier": 0,
                "source_frontier": 0,
                "summary_frontier": 0
            }),
            serde_json::json!({
                "active_generation": 1,
                "cursor_key": {"key_id": "application-key", "version": 1},
                "projection_frontier": 0,
                "source_frontier": 0,
                "summary_frontier": 0
            })
        ))
        .await
        .unwrap();
    drop(fixture);
    drop(fixture_db);
    let before = Sha256::digest(std::fs::read(&db_path).unwrap());
    let before_files = authoritative_fixture_hashes(tmp.path());

    let execution = GlobalDbSessionTemporalExecution::new(&db);
    let service = SessionRetrievalService::new(
        AllowAuthorizer,
        execution,
        Words("words-v1"),
        configuration(),
    );
    assert!(matches!(
        service
            .retrieve(&context("root.one"), query("absent"))
            .await,
        SessionRetrievalOutcome::CompleteZero { .. }
    ));

    let after = Sha256::digest(std::fs::read(&db_path).unwrap());
    assert_eq!(before, after);
    assert_eq!(authoritative_fixture_hashes(tmp.path()), before_files);
}

#[test]
fn temporal_application_api_is_publicly_composed() {
    fn assert_request(_: &TemporalKernelRequest) {}
    let _ = assert_request;
    assert_eq!(SessionAccess::Hydrate, SessionAccess::Hydrate);
}
