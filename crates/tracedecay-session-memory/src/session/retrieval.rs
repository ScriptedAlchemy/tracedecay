use std::fmt;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_application::RequestContext;
use tracedecay_application::retrieval::SessionRetrievalBudgetStageV1;
use tracedecay_domain::canonical_text::{encode_lowercase_hex, encode_tagged_lowercase_hex};
use tracedecay_domain::{
    ContextOmissionReasonV1, CursorManifestLimitKindV1, RetrievalAnchorId, RetrievalGrainV1,
    SessionId, TemporalModeV1,
};
use tracedecay_temporal_query::context::{ContextBudget, ContextError, VersionedTokenEstimator};
use tracedecay_temporal_query::cursor::CursorError;
use tracedecay_temporal_query::hydration::HydrationError;
use tracedecay_temporal_query::ports::{
    ExecutionControl, ExecutionLimits, TemporalAuthorizedRoot, TemporalCandidateFilterV1,
    TemporalPortError, TemporalRetrievalScope,
};
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_temporal_query::resolution::SummaryLineageRejection;
use tracedecay_temporal_query::{TemporalKernelError, TemporalKernelResult};

use crate::context::{
    PolicyDigest, RequestInterruption, ResolvedSessionIdentity, SessionOwner,
    application_observed_at, application_request_interruption,
    run_application_request_interruptible,
};
use crate::session::ports::{
    AuthorizedTemporalExecutionRequest, SessionTemporalExecutionError, SessionTemporalExecutionPort,
};
use crate::session::types::{
    SessionAccess, SessionAuthorizationError, SessionDataFreshness, SessionFreshnessPolicy,
    SessionRequestBinding, SessionRetrievalOutcome, SessionRetrievalScope,
    SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
};

mod task_session;
pub use task_session::TaskSessionRetrievalOutcomeV1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionRetrievalConfiguration {
    schema_version: u32,
    ranking_version: u32,
}

impl SessionRetrievalConfiguration {
    pub fn new(
        schema_version: u32,
        ranking_version: u32,
    ) -> Result<Self, SessionTemporalQueryError> {
        if schema_version == 0 {
            return Err(SessionTemporalQueryError::ZeroVersion("schema"));
        }
        if ranking_version == 0 {
            return Err(SessionTemporalQueryError::ZeroVersion("ranking"));
        }
        Ok(Self {
            schema_version,
            ranking_version,
        })
    }

    #[hotpath::skip]
    pub const fn schema_version(self) -> u32 {
        self.schema_version
    }

    #[hotpath::skip]
    pub const fn ranking_version(self) -> u32 {
        self.ranking_version
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTemporalQuery {
    session_id: SessionId,
    retrieval_scope: SessionRetrievalScope,
    provider: Option<String>,
    query: String,
    direct_anchor: Option<RetrievalAnchorId>,
    compatibility_filter_digest: Option<String>,
    semantic_filter: TemporalCandidateFilterV1,
    cursor: Option<String>,
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
    limit: usize,
    diversity: DiversityLimits,
    context_budget: ContextBudget,
    execution_limits: ExecutionLimits,
    freshness_policy: SessionFreshnessPolicy,
}

impl SessionTemporalQuery {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: SessionId,
        provider: Option<String>,
        query: impl Into<String>,
        cursor: Option<String>,
        temporal_mode: TemporalModeV1,
        grain: RetrievalGrainV1,
        limit: usize,
        diversity: DiversityLimits,
        context_budget: ContextBudget,
    ) -> Result<Self, SessionTemporalQueryError> {
        if limit == 0 {
            return Err(SessionTemporalQueryError::ZeroLimit);
        }
        if provider.as_deref().is_some_and(|value| {
            value.is_empty()
                || value.trim() != value
                || value.len() > 512
                || value.chars().any(char::is_control)
        }) {
            return Err(SessionTemporalQueryError::InvalidProvider);
        }
        if context_budget.estimator_version.is_empty()
            || context_budget.estimator_version.len() > 512
            || context_budget
                .estimator_version
                .chars()
                .any(char::is_control)
        {
            return Err(SessionTemporalQueryError::InvalidEstimatorVersion);
        }
        let retrieval_scope = SessionRetrievalScope::Session(session_id.clone());
        Ok(Self {
            session_id,
            retrieval_scope,
            provider,
            query: query.into(),
            direct_anchor: None,
            compatibility_filter_digest: None,
            semantic_filter: TemporalCandidateFilterV1::default(),
            cursor,
            temporal_mode,
            grain,
            limit,
            diversity,
            context_budget,
            execution_limits: ExecutionLimits::default(),
            freshness_policy: SessionFreshnessPolicy::AllowStored,
        })
    }

    #[must_use]
    pub fn with_execution_limits(mut self, execution_limits: ExecutionLimits) -> Self {
        self.execution_limits = execution_limits;
        self
    }

    #[must_use]
    pub fn with_freshness_policy(mut self, freshness_policy: SessionFreshnessPolicy) -> Self {
        self.freshness_policy = freshness_policy;
        self
    }

    #[must_use]
    pub fn with_retrieval_scope(mut self, retrieval_scope: SessionRetrievalScope) -> Self {
        if let SessionRetrievalScope::Session(session_id) = &retrieval_scope {
            self.session_id = session_id.clone();
        }
        self.retrieval_scope = retrieval_scope;
        self
    }

    #[must_use]
    pub fn with_compatibility_filter_digest(mut self, digest: String) -> Self {
        self.compatibility_filter_digest = Some(digest);
        self
    }

    #[must_use]
    pub fn with_semantic_filter(mut self, semantic_filter: TemporalCandidateFilterV1) -> Self {
        self.semantic_filter = semantic_filter;
        self
    }

    #[must_use]
    pub fn with_direct_anchor(mut self, anchor_id: RetrievalAnchorId) -> Self {
        self.direct_anchor = Some(anchor_id);
        self
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn retrieval_scope(&self) -> &SessionRetrievalScope {
        &self.retrieval_scope
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn compatibility_filter_digest(&self) -> Option<&str> {
        self.compatibility_filter_digest.as_deref()
    }

    pub fn semantic_filter(&self) -> &TemporalCandidateFilterV1 {
        &self.semantic_filter
    }

    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    #[hotpath::skip]
    pub const fn temporal_mode(&self) -> TemporalModeV1 {
        self.temporal_mode
    }

    #[hotpath::skip]
    pub const fn grain(&self) -> RetrievalGrainV1 {
        self.grain
    }

    #[hotpath::skip]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    #[hotpath::skip]
    pub const fn diversity(&self) -> DiversityLimits {
        self.diversity
    }

    pub fn context_budget(&self) -> &ContextBudget {
        &self.context_budget
    }

    #[hotpath::skip]
    pub const fn execution_limits(&self) -> ExecutionLimits {
        self.execution_limits
    }

    #[hotpath::skip]
    pub const fn freshness_policy(&self) -> SessionFreshnessPolicy {
        self.freshness_policy
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTemporalQueryError {
    ZeroLimit,
    InvalidProvider,
    InvalidEstimatorVersion,
    ZeroVersion(&'static str),
}

impl fmt::Display for SessionTemporalQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroLimit => {
                formatter.write_str("temporal query limit must be greater than zero")
            }
            Self::InvalidProvider => {
                formatter.write_str("temporal query provider is not canonical")
            }
            Self::InvalidEstimatorVersion => {
                formatter.write_str("temporal query estimator version is not canonical")
            }
            Self::ZeroVersion(field) => {
                write!(formatter, "temporal query {field} version must be non-zero")
            }
        }
    }
}

impl std::error::Error for SessionTemporalQueryError {}

pub struct SessionRetrievalService<A, P, E> {
    authorizer: A,
    execution: P,
    estimator: E,
    configuration: SessionRetrievalConfiguration,
}

impl<A, P, E> SessionRetrievalService<A, P, E> {
    #[hotpath::skip]
    pub const fn new(
        authorizer: A,
        execution: P,
        estimator: E,
        configuration: SessionRetrievalConfiguration,
    ) -> Self {
        Self {
            authorizer,
            execution,
            estimator,
            configuration,
        }
    }
}

impl<A, P, E> SessionRetrievalService<A, P, E>
where
    A: SessionScopeAuthorizer,
    P: SessionTemporalExecutionPort,
    E: VersionedTokenEstimator + Sync,
{
    #[hotpath::measure(label = "usecases.session.retrieve")]
    pub async fn retrieve(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        query: SessionTemporalQuery,
    ) -> SessionRetrievalOutcome<TemporalKernelResult> {
        let admitted = match self.admit_execution(context, binding, &query) {
            Ok(admitted) => admitted,
            Err(failure) => return failure.into_outcome(),
        };
        let expected_execution = admitted.execution.clone();
        let result = hotpath::future!(
            run_application_request_interruptible(
                context,
                binding.cancellation(),
                self.execution.execute(admitted.execution, &self.estimator),
                || {
                    admitted.cancellation_control.cancel();
                },
            ),
            label = "usecases.session.execute"
        )
        .await;
        let result = match result {
            Ok(result) => result,
            Err(RequestInterruption::Cancelled) => return SessionRetrievalOutcome::Cancelled,
            Err(RequestInterruption::DeadlineExceeded) => {
                return SessionRetrievalOutcome::TimedOut;
            }
        };
        match result {
            Ok(report) if expected_execution.validates_report(&report) => {
                map_report(report, query.freshness_policy)
            }
            Ok(_) => SessionRetrievalOutcome::Unavailable,
            Err(error) => map_execution_error(error),
        }
    }
}

fn execution_deadline(context: &RequestContext) -> std::time::Instant {
    let terminal_at = context
        .deadline()
        .expires_at
        .0
        .min(context.grant().expires_at.0);
    let remaining_micros =
        u64::try_from(terminal_at.saturating_sub(application_observed_at().0)).unwrap_or(0);
    std::time::Instant::now() + std::time::Duration::from_micros(remaining_micros)
}

fn budget_exhausted<T>(stage: SessionRetrievalBudgetStageV1) -> SessionRetrievalOutcome<T> {
    crate::session::hotpath_observe::session_retrieval_budget_stage(stage);
    SessionRetrievalOutcome::BudgetExhausted { stage }
}

fn request_budget_refusal(
    binding: &SessionRequestBinding,
    query: &SessionTemporalQuery,
) -> Option<SessionRetrievalBudgetStageV1> {
    let budgets = binding.budgets();
    let limits = query.execution_limits;
    if !u64::try_from(query.limit).is_ok_and(|limit| limit <= budgets.max_results()) {
        return Some(SessionRetrievalBudgetStageV1::RequestResultLimit);
    }
    if query.limit > limits.hydration_limit {
        return Some(SessionRetrievalBudgetStageV1::RequestHydrationLimit);
    }
    if query.context_budget.max_bytes > budgets.max_bytes() {
        return Some(SessionRetrievalBudgetStageV1::RequestContextBytes);
    }
    for (bytes, stage) in [
        (
            limits.candidate_total_bytes,
            SessionRetrievalBudgetStageV1::RequestCandidateBytes,
        ),
        (
            limits.record_total_bytes,
            SessionRetrievalBudgetStageV1::RequestRecordBytes,
        ),
        (
            limits.hydration_total_bytes,
            SessionRetrievalBudgetStageV1::RequestHydrationBytes,
        ),
    ] {
        if !u64::try_from(bytes).is_ok_and(|bytes| bytes <= budgets.max_bytes()) {
            return Some(stage);
        }
    }
    None
}

fn temporal_retrieval_scope(scope: &SessionRetrievalScope) -> TemporalRetrievalScope {
    match scope {
        SessionRetrievalScope::Session(session_id) => {
            TemporalRetrievalScope::Session(session_id.clone())
        }
        SessionRetrievalScope::AllSessionsInAuthorizedRoot => {
            TemporalRetrievalScope::AllSessionsInAuthorizedRoot
        }
    }
}

fn temporal_authorized_root(
    identity: &ResolvedSessionIdentity,
) -> Result<TemporalAuthorizedRoot, TemporalPortError> {
    match identity.owner() {
        SessionOwner::Profile { .. } => TemporalAuthorizedRoot::profile(
            identity.profile_id().as_str(),
            identity.store_id().as_str(),
            identity.root_id().as_str(),
        ),
        SessionOwner::Project { project_id, .. } => TemporalAuthorizedRoot::project(
            identity.profile_id().as_str(),
            project_id.as_str(),
            identity.store_id().as_str(),
            identity.root_id().as_str(),
        ),
    }
}

fn map_report(
    report: crate::session::ports::SessionTemporalExecutionReport,
    freshness_policy: SessionFreshnessPolicy,
) -> SessionRetrievalOutcome<TemporalKernelResult> {
    let (result, freshness) = report.into_parts();
    let coverage = result.coverage;
    let omissions = &result.context.bundle.omissions;
    let coverage_omitted = coverage
        .hidden
        .saturating_add(coverage.unknown)
        .saturating_add(coverage.redacted);
    let explicit_omitted = u64::try_from(
        omissions
            .len()
            .saturating_add(result.summary_omissions.len()),
    )
    .unwrap_or(u64::MAX);
    let freshness_omitted = match freshness {
        SessionDataFreshness::Partial { generation_lag } => generation_lag.max(1),
        SessionDataFreshness::Fresh | SessionDataFreshness::Stored { .. } => 0,
    };
    let omitted = coverage_omitted
        .max(explicit_omitted)
        .max(freshness_omitted);
    let has_partial_coverage =
        omitted != 0 || !omissions.is_empty() || !result.summary_omissions.is_empty();
    if result.ranked.is_empty() {
        let summary_rejections = result
            .summary_omissions
            .iter()
            .map(|omission| &omission.rejection);
        if omissions
            .iter()
            .any(|omission| omission.reason == ContextOmissionReasonV1::Unauthorized)
            || summary_rejections.clone().any(|rejection| {
                matches!(
                    rejection,
                    SummaryLineageRejection::UnauthorizedSource { .. }
                        | SummaryLineageRejection::SessionMismatch
                )
            })
            || coverage.hidden != 0
        {
            return SessionRetrievalOutcome::Denied;
        }
        if omissions
            .iter()
            .any(|omission| omission.reason == ContextOmissionReasonV1::Locked)
            || summary_rejections
                .clone()
                .any(|rejection| matches!(rejection, SummaryLineageRejection::LockedSource { .. }))
        {
            return SessionRetrievalOutcome::Locked;
        }
        if omissions.iter().any(|omission| {
            matches!(
                omission.reason,
                ContextOmissionReasonV1::Deleted | ContextOmissionReasonV1::RetentionExpired
            )
        }) || summary_rejections.clone().any(|rejection| {
            matches!(
                rejection,
                SummaryLineageRejection::DeletedSource { .. }
                    | SummaryLineageRejection::ExpiredSource { .. }
            )
        }) {
            return SessionRetrievalOutcome::Deleted;
        }
        if omissions
            .iter()
            .any(|omission| omission.reason == ContextOmissionReasonV1::Redacted)
            || summary_rejections.clone().any(|rejection| {
                matches!(rejection, SummaryLineageRejection::RedactedSource { .. })
            })
            || coverage.redacted != 0
        {
            return SessionRetrievalOutcome::Redacted;
        }
        if omissions.iter().any(|omission| {
            matches!(
                omission.reason,
                ContextOmissionReasonV1::ByteBudget | ContextOmissionReasonV1::TokenBudget
            )
        }) {
            return budget_exhausted(SessionRetrievalBudgetStageV1::ContextBytes);
        }
        if !freshness_policy.accepts(freshness) {
            return SessionRetrievalOutcome::Stale { freshness };
        }
        if matches!(freshness, SessionDataFreshness::Partial { .. }) {
            return SessionRetrievalOutcome::Partial {
                items: vec![result],
                freshness,
                omitted,
            };
        }
        if has_partial_coverage || result.next_cursor.is_some() || coverage.visible != 0 {
            return SessionRetrievalOutcome::Unavailable;
        }
        // Authorized empty roots are searchable zero-hit results, not unavailable.
        return SessionRetrievalOutcome::CompleteZero { freshness };
    }
    if !freshness_policy.accepts(freshness) {
        return SessionRetrievalOutcome::Stale { freshness };
    }
    if has_partial_coverage || result.next_cursor.is_some() {
        // A continuation cursor keeps a windowed read visibly partial, but it
        // is pagination, not omission: only genuine coverage gaps carry a
        // non-zero omitted count. Fabricating `omitted = 1` for a paged read
        // made every multi-record session describe a partial description.
        let omitted = if has_partial_coverage {
            omitted.max(1)
        } else {
            omitted
        };
        return SessionRetrievalOutcome::Partial {
            items: vec![result],
            freshness,
            omitted,
        };
    }
    SessionRetrievalOutcome::Complete {
        items: vec![result],
        freshness,
    }
}

fn map_execution_error(
    error: SessionTemporalExecutionError,
) -> SessionRetrievalOutcome<TemporalKernelResult> {
    match error {
        SessionTemporalExecutionError::WrongScope => SessionRetrievalOutcome::WrongScope,
        SessionTemporalExecutionError::Stale { generation_lag } => SessionRetrievalOutcome::Stale {
            freshness: SessionDataFreshness::Stored { generation_lag },
        },
        SessionTemporalExecutionError::Locked => SessionRetrievalOutcome::Locked,
        SessionTemporalExecutionError::Redacted => SessionRetrievalOutcome::Redacted,
        SessionTemporalExecutionError::Deleted => SessionRetrievalOutcome::Deleted,
        SessionTemporalExecutionError::Denied => SessionRetrievalOutcome::Denied,
        SessionTemporalExecutionError::Unavailable => SessionRetrievalOutcome::Unavailable,
        SessionTemporalExecutionError::ResetRequired => SessionRetrievalOutcome::ResetRequired,
        SessionTemporalExecutionError::Empty { freshness } => {
            SessionRetrievalOutcome::CompleteZero { freshness }
        }
        SessionTemporalExecutionError::BudgetExhausted => {
            budget_exhausted(SessionRetrievalBudgetStageV1::ExecutionWorkExhausted)
        }
        SessionTemporalExecutionError::Cancelled => SessionRetrievalOutcome::Cancelled,
        SessionTemporalExecutionError::Kernel(error) => map_kernel_error(error),
    }
}

fn map_kernel_error(error: TemporalKernelError) -> SessionRetrievalOutcome<TemporalKernelResult> {
    match error {
        TemporalKernelError::InvalidLimit => {
            budget_exhausted(SessionRetrievalBudgetStageV1::KernelResultLimit)
        }
        TemporalKernelError::BudgetExceeded => {
            budget_exhausted(SessionRetrievalBudgetStageV1::ExecutionWorkExhausted)
        }
        TemporalKernelError::Cancelled => SessionRetrievalOutcome::Cancelled,
        TemporalKernelError::DeadlineExceeded => SessionRetrievalOutcome::TimedOut,
        TemporalKernelError::Port(error) => match error {
            TemporalPortError::Cancelled => SessionRetrievalOutcome::Cancelled,
            TemporalPortError::DeadlineExceeded => SessionRetrievalOutcome::TimedOut,
            TemporalPortError::BudgetExceeded { .. } => {
                budget_exhausted(SessionRetrievalBudgetStageV1::ExecutionWorkExhausted)
            }
            TemporalPortError::ParticipantLimitExceeded { observed, maximum } => {
                crate::session::hotpath_observe::session_retrieval_budget_stage(
                    SessionRetrievalBudgetStageV1::ParticipantManifestParticipants,
                );
                SessionRetrievalOutcome::CursorManifestLimitExceeded {
                    kind: CursorManifestLimitKindV1::Participants,
                    observed,
                    maximum,
                }
            }
            TemporalPortError::ParticipantManifestBytesExceeded { observed, maximum } => {
                crate::session::hotpath_observe::session_retrieval_budget_stage(
                    SessionRetrievalBudgetStageV1::ParticipantManifestCanonicalBytes,
                );
                SessionRetrievalOutcome::CursorManifestLimitExceeded {
                    kind: CursorManifestLimitKindV1::CanonicalBytes,
                    observed,
                    maximum,
                }
            }
            TemporalPortError::UnauthorizedSnapshot => SessionRetrievalOutcome::Denied,
            TemporalPortError::EmptyParticipantManifest => SessionRetrievalOutcome::Unavailable,
            TemporalPortError::ResetRequired { .. } => SessionRetrievalOutcome::ResetRequired,
            TemporalPortError::InvalidBinding { .. }
            | TemporalPortError::DuplicateParticipant
            | TemporalPortError::ZeroGeneration
            | TemporalPortError::ZeroVersion { .. }
            | TemporalPortError::Read { .. } => SessionRetrievalOutcome::Unavailable,
        },
        TemporalKernelError::Cursor(error) => match error {
            CursorError::RootMismatch
            | CursorError::SessionMismatch
            | CursorError::WrongAccess
            | CursorError::FilterMismatch
            | CursorError::TemporalModeMismatch
            | CursorError::GrainMismatch => SessionRetrievalOutcome::WrongScope,
            CursorError::Malformed
            | CursorError::Tampered
            | CursorError::Expired
            | CursorError::UnknownOrExpiredKey
            | CursorError::SortKeyMismatch => SessionRetrievalOutcome::Denied,
            CursorError::WrongRequest
            | CursorError::SchemaMismatch
            | CursorError::RankingMismatch
            | CursorError::ConfigurationMismatch
            | CursorError::GenerationMismatch
            | CursorError::ParticipantManifestMismatch
            | CursorError::EpochMismatch
            | CursorError::SourceWatermarkMismatch
            | CursorError::ProjectionWatermarkMismatch
            | CursorError::IndexWatermarkMismatch
            | CursorError::SummaryWatermarkMismatch
            | CursorError::KeyIdMismatch
            | CursorError::KeyVersionMismatch
            | CursorError::KeyUnavailable
            | CursorError::InvalidKeyMaterial => SessionRetrievalOutcome::Unavailable,
            CursorError::CandidateCohortMismatch => SessionRetrievalOutcome::CursorStale,
        },
        TemporalKernelError::Hydration(error) => match error {
            HydrationError::BudgetExceeded { .. } => {
                budget_exhausted(SessionRetrievalBudgetStageV1::HydrationBytes)
            }
            HydrationError::Interrupted(TemporalPortError::Cancelled) => {
                SessionRetrievalOutcome::Cancelled
            }
            HydrationError::Interrupted(TemporalPortError::DeadlineExceeded) => {
                SessionRetrievalOutcome::TimedOut
            }
            HydrationError::Interrupted(TemporalPortError::BudgetExceeded { .. }) => {
                budget_exhausted(SessionRetrievalBudgetStageV1::HydrationBytes)
            }
            HydrationError::ResetRequired { .. }
            | HydrationError::Interrupted(TemporalPortError::ResetRequired { .. }) => {
                SessionRetrievalOutcome::ResetRequired
            }
            HydrationError::Unavailable
            | HydrationError::InvalidDenial
            | HydrationError::Interrupted(_) => SessionRetrievalOutcome::Unavailable,
        },
        TemporalKernelError::Context(error) => match error {
            ContextError::BudgetExceeded { resource } => {
                budget_exhausted(context_budget_stage(resource))
            }
            ContextError::Interrupted(TemporalPortError::Cancelled) => {
                SessionRetrievalOutcome::Cancelled
            }
            ContextError::Interrupted(TemporalPortError::DeadlineExceeded) => {
                SessionRetrievalOutcome::TimedOut
            }
            ContextError::Interrupted(TemporalPortError::BudgetExceeded { .. }) => {
                budget_exhausted(SessionRetrievalBudgetStageV1::ContextBytes)
            }
            ContextError::Interrupted(TemporalPortError::ResetRequired { .. }) => {
                SessionRetrievalOutcome::ResetRequired
            }
            ContextError::EstimatorVersionMismatch
            | ContextError::InvalidBundle(_)
            | ContextError::Interrupted(_) => SessionRetrievalOutcome::Unavailable,
        },
        TemporalKernelError::Ranking(_) | TemporalKernelError::CandidateExportContract(_) => {
            SessionRetrievalOutcome::Unavailable
        }
    }
}

fn context_budget_stage(resource: &'static str) -> SessionRetrievalBudgetStageV1 {
    match resource {
        "token" => SessionRetrievalBudgetStageV1::ContextTokens,
        _ => SessionRetrievalBudgetStageV1::ContextBytes,
    }
}

fn digest_root(identity: &ResolvedSessionIdentity) -> String {
    #[derive(Serialize)]
    struct RootBinding<'a> {
        profile_id: &'a str,
        project_id: Option<&'a str>,
        store_id: &'a str,
        root_id: &'a str,
        repository_id: Option<&'a str>,
        worktree_id: Option<&'a str>,
        branch_id: Option<&'a str>,
        owner_kind: &'static str,
    }

    let route = identity.git_route();
    sha256_json(&RootBinding {
        profile_id: identity.profile_id().as_str(),
        project_id: identity
            .project_id()
            .map(tracedecay_domain::ProjectId::as_str),
        store_id: identity.store_id().as_str(),
        root_id: identity.root_id().as_str(),
        repository_id: route.map(|value| value.repository_id().as_str()),
        worktree_id: route.map(|value| value.worktree_id().as_str()),
        branch_id: route.map(|value| value.branch_id().as_str()),
        owner_kind: match identity.owner() {
            SessionOwner::Profile { .. } => "profile",
            SessionOwner::Project { .. } => "project",
        },
    })
}

fn digest_grant(grant: &crate::session::types::SessionAuthorizationGrant) -> String {
    #[derive(Serialize)]
    struct AccessBinding<'a> {
        format_version: u8,
        actor_id: &'a str,
        grant_id: &'a str,
        grant_revision: u64,
        capability_digest: String,
        policy_digest: String,
        configuration_digest: String,
        access: &'static str,
        scope_kind: &'static str,
        session_id: Option<&'a str>,
        provider: Option<&'a str>,
        temporal_mode: &'static str,
        cutoff_micros: Option<i64>,
        grain: &'static str,
        root_digest: String,
    }

    sha256_json(&AccessBinding {
        format_version: 2,
        actor_id: grant.scope().actor_id().as_str(),
        grant_id: grant.id().as_str(),
        grant_revision: grant.revision(),
        capability_digest: encode_lowercase_hex(grant.capability_digest().as_bytes()),
        policy_digest: encode_lowercase_hex(grant.policy_digest().as_bytes()),
        configuration_digest: encode_lowercase_hex(grant.configuration_digest().as_bytes()),
        access: match grant.scope().access() {
            SessionAccess::Read => "read",
            SessionAccess::Search => "search",
            SessionAccess::Hydrate => "hydrate",
        },
        scope_kind: grant.scope().retrieval_scope().kind(),
        session_id: grant
            .scope()
            .retrieval_scope()
            .session_id()
            .map(SessionId::as_str),
        provider: grant.scope().provider_scope(),
        temporal_mode: grant.scope().temporal_mode().as_str(),
        cutoff_micros: match grant.scope().temporal_mode() {
            TemporalModeV1::AsOf { cutoff } => Some(cutoff.0),
            TemporalModeV1::Current | TemporalModeV1::Evolution | TemporalModeV1::Forensic => None,
        },
        grain: grant.scope().grain().as_str(),
        root_digest: digest_root(grant.scope().authorized_root().identity()),
    })
}

fn digest_policy(policy_digest: PolicyDigest) -> String {
    encode_tagged_lowercase_hex("sha256:", policy_digest.as_bytes())
}

fn digest_request(
    context: &RequestContext,
    binding: &SessionRequestBinding,
    query: &SessionTemporalQuery,
    grant: &crate::session::types::SessionAuthorizationGrant,
    configuration: SessionRetrievalConfiguration,
    root_digest: &str,
    grant_digest: &str,
) -> String {
    #[derive(Serialize)]
    struct RequestBinding<'a> {
        format_version: u8,
        actor_id: &'a str,
        grant_id: &'a str,
        grant_revision: u64,
        grant_capability_digest: String,
        grant_policy_digest: String,
        grant_configuration_digest: String,
        access: &'static str,
        scope_kind: &'static str,
        session_id: Option<&'a str>,
        provider: Option<&'a str>,
        query: &'a str,
        direct_anchor: Option<String>,
        compatibility_filter_digest: Option<&'a str>,
        semantic_filter: &'a TemporalCandidateFilterV1,
        temporal_mode: &'static str,
        cutoff_micros: Option<i64>,
        grain: &'static str,
        root_digest: &'a str,
        grant_digest: &'a str,
        limit: usize,
        diversity: [usize; 5],
        context_budget: (u64, u64, &'a str),
        execution_limits: [usize; 15],
        request_budgets: [u64; 3],
        freshness_policy: &'static str,
        schema_version: u32,
        ranking_version: u32,
        configuration_version: String,
    }

    let limits = query.execution_limits;
    sha256_json(&RequestBinding {
        format_version: 5,
        actor_id: context.actor().as_str(),
        grant_id: grant.id().as_str(),
        grant_revision: grant.revision(),
        grant_capability_digest: encode_lowercase_hex(grant.capability_digest().as_bytes()),
        grant_policy_digest: encode_lowercase_hex(grant.policy_digest().as_bytes()),
        grant_configuration_digest: encode_lowercase_hex(grant.configuration_digest().as_bytes()),
        access: "hydrate",
        scope_kind: query.retrieval_scope.kind(),
        session_id: query.retrieval_scope.session_id().map(SessionId::as_str),
        provider: query.provider.as_deref(),
        query: &query.query,
        direct_anchor: query.direct_anchor.as_ref().map(ToString::to_string),
        compatibility_filter_digest: query.compatibility_filter_digest.as_deref(),
        semantic_filter: &query.semantic_filter,
        temporal_mode: query.temporal_mode.as_str(),
        cutoff_micros: match query.temporal_mode {
            TemporalModeV1::AsOf { cutoff } => Some(cutoff.0),
            TemporalModeV1::Current | TemporalModeV1::Evolution | TemporalModeV1::Forensic => None,
        },
        grain: query.grain.as_str(),
        root_digest,
        grant_digest,
        limit: query.limit,
        diversity: [
            query.diversity.per_logical_message,
            query.diversity.per_turn,
            query.diversity.per_session,
            query.diversity.per_source,
            query.diversity.per_evidence_role,
        ],
        context_budget: (
            query.context_budget.max_bytes,
            query.context_budget.max_tokens,
            &query.context_budget.estimator_version,
        ),
        execution_limits: [
            limits.candidate_limit,
            limits.candidate_total_bytes,
            limits.candidate_item_bytes,
            limits.candidate_key_bytes,
            limits.candidate_stable_id_bytes,
            limits.candidate_anchor_id_bytes,
            limits.candidate_metadata_field_bytes,
            limits.record_limit,
            limits.record_total_bytes,
            limits.record_item_bytes,
            limits.record_key_bytes,
            limits.hydration_limit,
            limits.hydration_total_bytes,
            limits.hydration_payload_bytes,
            limits.hydration_chunk_bytes,
        ],
        request_budgets: [
            binding.budgets().max_results(),
            binding.budgets().max_bytes(),
            binding.budgets().max_work_units(),
        ],
        freshness_policy: match query.freshness_policy {
            SessionFreshnessPolicy::AllowStored => "allow_stored",
            SessionFreshnessPolicy::RequireFresh => "require_fresh",
        },
        schema_version: configuration.schema_version,
        ranking_version: configuration.ranking_version,
        configuration_version: encode_lowercase_hex(binding.configuration_digest().as_bytes()),
    })
}

fn digest_filters(query: &SessionTemporalQuery) -> String {
    #[derive(Serialize)]
    struct FilterBinding<'a> {
        format_version: u8,
        query: &'a str,
        scope_kind: &'static str,
        session_id: Option<&'a str>,
        provider: Option<&'a str>,
        temporal_mode: &'static str,
        cutoff_micros: Option<i64>,
        grain: &'static str,
        direct_anchor: Option<String>,
        compatibility_filter_digest: Option<&'a str>,
        semantic_filter: &'a TemporalCandidateFilterV1,
    }

    sha256_json(&FilterBinding {
        format_version: 3,
        query: &query.query,
        scope_kind: query.retrieval_scope.kind(),
        session_id: query.retrieval_scope.session_id().map(SessionId::as_str),
        provider: query.provider.as_deref(),
        temporal_mode: query.temporal_mode.as_str(),
        cutoff_micros: match query.temporal_mode {
            TemporalModeV1::AsOf { cutoff } => Some(cutoff.0),
            TemporalModeV1::Current | TemporalModeV1::Evolution | TemporalModeV1::Forensic => None,
        },
        grain: query.grain.as_str(),
        direct_anchor: query.direct_anchor.as_ref().map(ToString::to_string),
        compatibility_filter_digest: query.compatibility_filter_digest.as_deref(),
        semantic_filter: &query.semantic_filter,
    })
}

fn sha256_json(value: &impl Serialize) -> String {
    // The canonical request binding is plain serializable data; encoding it cannot fail.
    #[allow(clippy::expect_used)]
    let encoded = serde_json::to_vec(value).expect("canonical request binding is serializable");
    sha256_binding(&encoded)
}

fn sha256_binding(bytes: &[u8]) -> String {
    encode_tagged_lowercase_hex("sha256:", &Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_manifest_limits_remain_typed_application_outcomes() {
        assert_eq!(
            map_kernel_error(TemporalKernelError::Port(
                TemporalPortError::ParticipantLimitExceeded {
                    observed: 257,
                    maximum: 256,
                },
            )),
            SessionRetrievalOutcome::CursorManifestLimitExceeded {
                kind: CursorManifestLimitKindV1::Participants,
                observed: 257,
                maximum: 256,
            }
        );
        assert_eq!(
            map_kernel_error(TemporalKernelError::Port(
                TemporalPortError::ParticipantManifestBytesExceeded {
                    observed: 65_537,
                    maximum: 65_536,
                },
            )),
            SessionRetrievalOutcome::CursorManifestLimitExceeded {
                kind: CursorManifestLimitKindV1::CanonicalBytes,
                observed: 65_537,
                maximum: 65_536,
            }
        );
    }

    #[test]
    fn deadline_and_cancellation_remain_distinct_application_outcomes() {
        assert_eq!(
            map_kernel_error(TemporalKernelError::DeadlineExceeded),
            SessionRetrievalOutcome::TimedOut
        );
        assert_eq!(
            map_kernel_error(TemporalKernelError::Port(
                TemporalPortError::DeadlineExceeded,
            )),
            SessionRetrievalOutcome::TimedOut
        );
        assert_eq!(
            map_kernel_error(TemporalKernelError::Cancelled),
            SessionRetrievalOutcome::Cancelled
        );
    }

    #[test]
    fn structural_refusals_preserve_their_source() {
        for (error, expected) in [
            (
                TemporalKernelError::InvalidLimit,
                SessionRetrievalOutcome::BudgetExhausted {
                    stage: SessionRetrievalBudgetStageV1::KernelResultLimit,
                },
            ),
            (
                TemporalKernelError::BudgetExceeded,
                SessionRetrievalOutcome::BudgetExhausted {
                    stage: SessionRetrievalBudgetStageV1::ExecutionWorkExhausted,
                },
            ),
            (
                TemporalKernelError::Port(TemporalPortError::ParticipantLimitExceeded {
                    observed: 257,
                    maximum: 256,
                }),
                SessionRetrievalOutcome::CursorManifestLimitExceeded {
                    kind: CursorManifestLimitKindV1::Participants,
                    observed: 257,
                    maximum: 256,
                },
            ),
            (
                TemporalKernelError::Port(TemporalPortError::ParticipantManifestBytesExceeded {
                    observed: 4_097,
                    maximum: 4_096,
                }),
                SessionRetrievalOutcome::CursorManifestLimitExceeded {
                    kind: CursorManifestLimitKindV1::CanonicalBytes,
                    observed: 4_097,
                    maximum: 4_096,
                },
            ),
        ] {
            assert_eq!(map_kernel_error(error), expected);
        }
    }

    #[test]
    fn evidence_bearing_empty_execution_maps_to_complete_zero() {
        assert_eq!(
            map_execution_error(SessionTemporalExecutionError::Empty {
                freshness: SessionDataFreshness::Fresh,
            }),
            SessionRetrievalOutcome::CompleteZero {
                freshness: SessionDataFreshness::Fresh,
            }
        );
        assert_eq!(
            map_kernel_error(TemporalKernelError::Port(
                TemporalPortError::EmptyParticipantManifest,
            )),
            SessionRetrievalOutcome::Unavailable
        );
    }

    #[test]
    fn persisted_reset_and_unavailable_remain_distinct() {
        assert_eq!(
            map_kernel_error(TemporalKernelError::Port(
                TemporalPortError::ResetRequired {
                    resource: "session relation projection",
                },
            )),
            SessionRetrievalOutcome::ResetRequired
        );
        assert_eq!(
            map_kernel_error(TemporalKernelError::Port(TemporalPortError::Read {
                operation: "read temporal candidates",
                message: "unavailable".to_owned(),
            })),
            SessionRetrievalOutcome::Unavailable
        );
    }

    #[test]
    fn candidate_cohort_cursor_mismatch_requires_cursorless_restart() {
        assert_eq!(
            map_kernel_error(TemporalKernelError::Cursor(
                CursorError::CandidateCohortMismatch,
            )),
            SessionRetrievalOutcome::CursorStale
        );
    }
}
