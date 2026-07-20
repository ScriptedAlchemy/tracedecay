use crate::context::RequestContext;
use crate::handlers::ApplicationOperation;
use crate::result::RetrievalEvidence;

use super::{
    AffectedTestsRequest, AffectedTestsResult, AnchorExpandRequest, AnchorExpandResult,
    GraphCallersRequest, GraphCallersResult, HealthReadRequest, HealthReadResult,
    SessionLookupRequest, SessionLookupResult, SourceLinesRequest, SourceLinesResult,
    SymbolSearchRequest, SymbolSearchResult,
};

/// Context supplied to exactly one named retrieval port after admission.
#[derive(Clone, Copy, Debug)]
pub struct RetrievalPortContext<'a> {
    pub request: &'a RequestContext,
    pub operation: &'a ApplicationOperation,
}

/// Typed terminal output from one named port. The application invokes one
/// concrete method; this is not a universal query or planner interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetrievalPortOutcome<T> {
    Completed(RetrievalEvidence<T>),
    Partial(RetrievalEvidence<T>),
    Cancelled(RetrievalEvidence<T>),
    TimedOut(RetrievalEvidence<T>),
    Failed(RetrievalEvidence<T>),
    Unavailable(RetrievalEvidence<T>),
}

impl<T> RetrievalPortOutcome<T> {
    pub fn evidence(&self) -> &RetrievalEvidence<T> {
        match self {
            Self::Completed(evidence)
            | Self::Partial(evidence)
            | Self::Cancelled(evidence)
            | Self::TimedOut(evidence)
            | Self::Failed(evidence)
            | Self::Unavailable(evidence) => evidence,
        }
    }
}

pub trait SymbolRetrievalPort {
    fn symbol_search(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &SymbolSearchRequest,
    ) -> RetrievalPortOutcome<SymbolSearchResult>;
}

pub trait SourceRetrievalPort {
    fn source_lines(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &SourceLinesRequest,
    ) -> RetrievalPortOutcome<SourceLinesResult>;
}

pub trait GraphRetrievalPort {
    fn graph_callers(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &GraphCallersRequest,
    ) -> RetrievalPortOutcome<GraphCallersResult>;
}

pub trait AffectedTestsRetrievalPort {
    fn affected_tests(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &AffectedTestsRequest,
    ) -> RetrievalPortOutcome<AffectedTestsResult>;
}

pub use AffectedTestsRetrievalPort as TestRetrievalPort;

pub trait TemporalRetrievalPort {
    fn session_lookup(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &SessionLookupRequest,
    ) -> RetrievalPortOutcome<SessionLookupResult>;
}

pub trait AnchorHydrationPort {
    fn anchor_expand(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &AnchorExpandRequest,
    ) -> RetrievalPortOutcome<AnchorExpandResult>;
}

pub trait OperationalRetrievalPort {
    fn health_read(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &HealthReadRequest,
    ) -> RetrievalPortOutcome<HealthReadResult>;
}
