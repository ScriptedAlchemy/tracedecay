use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_domain::{RetrievalAnchorId, SessionSourceCoverageReceiptV1};

use crate::application::session::SessionDataFreshness;
use tracedecay_temporal_query::context::{ContextBudget, VersionedTokenEstimator};
use tracedecay_temporal_query::ports::{ExecutionLimits, TemporalSnapshotRequest};
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_temporal_query::{TemporalKernelError, TemporalKernelResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedTemporalExecutionRequest {
    snapshot_request: TemporalSnapshotRequest,
    query: String,
    direct_anchor: Option<RetrievalAnchorId>,
    cursor: Option<String>,
    limit: usize,
    diversity: DiversityLimits,
    context_budget: ContextBudget,
    schema_version: u32,
    ranking_version: u32,
    configuration_digest: String,
}

impl AuthorizedTemporalExecutionRequest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        snapshot_request: TemporalSnapshotRequest,
        query: String,
        cursor: Option<String>,
        limit: usize,
        diversity: DiversityLimits,
        context_budget: ContextBudget,
        schema_version: u32,
        ranking_version: u32,
        configuration_digest: String,
    ) -> Self {
        Self {
            snapshot_request,
            query,
            direct_anchor: None,
            cursor,
            limit,
            diversity,
            context_budget,
            schema_version,
            ranking_version,
            configuration_digest,
        }
    }

    pub fn snapshot_request(&self) -> &TemporalSnapshotRequest {
        &self.snapshot_request
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    #[must_use]
    pub(crate) fn with_direct_anchor(mut self, anchor_id: RetrievalAnchorId) -> Self {
        self.direct_anchor = Some(anchor_id);
        self
    }

    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    pub const fn limit(&self) -> usize {
        self.limit
    }

    pub const fn diversity(&self) -> DiversityLimits {
        self.diversity
    }

    pub fn context_budget(&self) -> &ContextBudget {
        &self.context_budget
    }

    pub const fn execution_limits(&self) -> ExecutionLimits {
        self.snapshot_request.limits()
    }

    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub const fn ranking_version(&self) -> u32 {
        self.ranking_version
    }

    pub fn configuration_digest(&self) -> &str {
        &self.configuration_digest
    }

    pub(crate) fn into_kernel_request(
        self,
        snapshot: tracedecay_temporal_query::ports::TemporalExecutionSnapshot,
    ) -> tracedecay_temporal_query::TemporalKernelRequest {
        tracedecay_temporal_query::TemporalKernelRequest {
            snapshot,
            query: self.query,
            direct_anchor: self.direct_anchor,
            cursor: self.cursor,
            limit: self.limit,
            diversity: self.diversity,
            context_budget: self.context_budget,
        }
    }

    pub(crate) fn validates_report(&self, report: &SessionTemporalExecutionReport) -> bool {
        let snapshot = &report.result().snapshot;
        let actual = snapshot.request();
        actual.root_digest() == self.snapshot_request.root_digest()
            && actual.request_digest() == self.snapshot_request.request_digest()
            && actual.filter_digest() == self.snapshot_request.filter_digest()
            && actual.access_digest() == self.snapshot_request.access_digest()
            && actual.retrieval_scope() == self.snapshot_request.retrieval_scope()
            && actual.authorized_root() == self.snapshot_request.authorized_root()
            && actual.provider_scope() == self.snapshot_request.provider_scope()
            && actual.temporal_mode() == self.snapshot_request.temporal_mode()
            && actual.grain() == self.snapshot_request.grain()
            && actual.limits() == self.snapshot_request.limits()
            && snapshot.authorization().is_authorized()
            && snapshot.versions().schema == self.schema_version
            && snapshot.versions().ranking == self.ranking_version
            && snapshot.versions().configuration_digest.as_str() == self.configuration_digest
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTemporalExecutionReport {
    result: TemporalKernelResult,
    freshness: SessionDataFreshness,
    source_coverage: Option<SessionSourceCoverageReceiptV1>,
}

impl SessionTemporalExecutionReport {
    pub fn new(result: TemporalKernelResult, freshness: SessionDataFreshness) -> Self {
        let source_coverage = result.snapshot.source_coverage().ok();
        Self {
            result,
            freshness,
            source_coverage,
        }
    }

    pub fn from_source_coverage(
        result: TemporalKernelResult,
        source_coverage: SessionSourceCoverageReceiptV1,
    ) -> Self {
        let freshness = SessionDataFreshness::from_source_coverage(&source_coverage);
        Self {
            result,
            freshness,
            source_coverage: Some(source_coverage),
        }
    }

    pub fn result(&self) -> &TemporalKernelResult {
        &self.result
    }

    pub const fn freshness(&self) -> SessionDataFreshness {
        self.freshness
    }

    pub fn source_coverage(&self) -> Option<&SessionSourceCoverageReceiptV1> {
        self.source_coverage.as_ref()
    }

    pub fn into_parts(self) -> (TemporalKernelResult, SessionDataFreshness) {
        (self.result, self.freshness)
    }
}

#[derive(Debug)]
pub enum SessionTemporalExecutionError {
    WrongScope,
    Stale { generation_lag: u64 },
    Locked,
    Redacted,
    Deleted,
    Denied,
    Unavailable,
    Empty { freshness: SessionDataFreshness },
    BudgetExhausted,
    Cancelled,
    Kernel(TemporalKernelError),
}

impl fmt::Display for SessionTemporalExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::WrongScope => "temporal execution scope does not match the authorization grant",
            Self::Stale { .. } => "temporal execution snapshot is stale",
            Self::Locked => "temporal payload is locked",
            Self::Redacted => "temporal payload is redacted",
            Self::Deleted => "temporal payload was deleted",
            Self::Denied => "temporal execution was denied",
            Self::Unavailable => "temporal execution is unavailable",
            Self::Empty { .. } => "temporal execution root is authoritatively empty",
            Self::BudgetExhausted => "temporal execution budget was exhausted",
            Self::Cancelled => "temporal execution was cancelled",
            Self::Kernel(_) => "temporal kernel failed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SessionTemporalExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Kernel(error) => Some(error),
            _ => None,
        }
    }
}

pub type TemporalExecutionFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<SessionTemporalExecutionReport, SessionTemporalExecutionError>>
            + Send
            + 'a,
    >,
>;

pub trait SessionTemporalExecutionPort: Send + Sync {
    fn execute<'a, E>(
        &'a self,
        request: AuthorizedTemporalExecutionRequest,
        estimator: &'a E,
    ) -> TemporalExecutionFuture<'a>
    where
        E: VersionedTokenEstimator + Sync + 'a;
}

impl<T> SessionTemporalExecutionPort for &T
where
    T: SessionTemporalExecutionPort + ?Sized,
{
    fn execute<'a, E>(
        &'a self,
        request: AuthorizedTemporalExecutionRequest,
        estimator: &'a E,
    ) -> TemporalExecutionFuture<'a>
    where
        E: VersionedTokenEstimator + Sync + 'a,
    {
        (**self).execute(request, estimator)
    }
}

impl<T> SessionTemporalExecutionPort for Arc<T>
where
    T: SessionTemporalExecutionPort + ?Sized,
{
    fn execute<'a, E>(
        &'a self,
        request: AuthorizedTemporalExecutionRequest,
        estimator: &'a E,
    ) -> TemporalExecutionFuture<'a>
    where
        E: VersionedTokenEstimator + Sync + 'a,
    {
        (**self).execute(request, estimator)
    }
}
