use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::CursorManifestLimitKindV1;

use crate::context::RequestContext;
use crate::handlers::ApplicationOperation;
use crate::result::RetrievalEvidence;

use super::{
    AffectedTestsRequest, AffectedTestsResult, AnchorExpandRequest, AnchorExpandResult,
    GraphCallersRequest, GraphCallersResult, GraphImpactRequest, GraphImpactResult,
    HealthReadRequest, HealthReadResult, SessionLookupRequest, SessionLookupResult,
    SourceLinesRequest, SourceLinesResult, SymbolSearchRequest, SymbolSearchResult,
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

/// Plan-05 graph-impact query boundary used by feedback orchestration.
/// It is intentionally distinct from the legacy callers projection because a
/// feedback result needs the query kernel's file, caller, and anchor evidence
/// as one bounded snapshot.
pub trait GraphImpactRetrievalPort {
    fn graph_impact(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &GraphImpactRequest,
    ) -> RetrievalPortOutcome<GraphImpactResult>;
}

pub trait AffectedTestsRetrievalPort {
    fn affected_tests(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &AffectedTestsRequest,
    ) -> RetrievalPortOutcome<AffectedTestsResult>;
}

/// Structural budget boundary that rejected a session retrieval request.
/// These causes are non-retryable request corrections; concurrent permit or
/// queue pressure remains a separate capacity-saturation failure.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionRetrievalBudgetStageV1 {
    RequestResultLimit,
    RequestHydrationLimit,
    RequestContextBytes,
    RequestCandidateBytes,
    RequestRecordBytes,
    RequestHydrationBytes,
    EstimatorVersionMismatch,
    ExecutionWorkExhausted,
    KernelResultLimit,
    ParticipantManifestParticipants,
    ParticipantManifestCanonicalBytes,
    HydrationBytes,
    ContextBytes,
    ContextTokens,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "refusal", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionRetrievalStructuralRefusalV1 {
    CursorManifestLimitExceeded {
        #[schemars(with = "String")]
        kind: CursorManifestLimitKindV1,
        observed: usize,
        maximum: usize,
    },
    BudgetExhausted {
        stage: SessionRetrievalBudgetStageV1,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemporalRetrievalFailure {
    Unavailable,
    ResetRequired,
    StructuralRefusal(SessionRetrievalStructuralRefusalV1),
}

pub type TemporalRetrievalFuture<'a> = Pin<
    Box<
        dyn Future<
                Output = Result<
                    RetrievalPortOutcome<SessionLookupResult>,
                    TemporalRetrievalFailure,
                >,
            > + Send
            + 'a,
    >,
>;

pub trait TemporalRetrievalPort {
    fn session_lookup<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SessionLookupRequest,
    ) -> TemporalRetrievalFuture<'a>;
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
