//! Typed port giving the semantic runtime the project's Grafeo code-graph
//! runtime, which owns the durable semantic-vector projection.
//!
//! `tracedecay-usecases` cannot see daemon session-registry types, so the
//! daemon implements this port over its retained code-graph runtime and
//! threads it through [`super::SavedGenerationScheduleHookParametersV1`]
//! (docs/plans/tracedecay-v2/39-embedded-grafeo-graph-database.md Task 4).

use std::sync::Arc;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_graph_db::{GraphCancellation, GraphDb};
use tracedecay_store::RetainedGraphStoreLeaseV1;

use super::ports::SemanticRuntimeFuture;

/// Why a code-graph runtime could not be retained for semantic-vector use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorGraphErrorV1 {
    /// No mounted code-graph runtime currently serves the project scope.
    Unavailable(String),
    /// The graph authority rejected the retention request.
    Rejected(String),
}

impl std::fmt::Display for SemanticVectorGraphErrorV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => {
                write!(formatter, "semantic vector graph unavailable: {reason}")
            }
            Self::Rejected(reason) => {
                write!(formatter, "semantic vector graph rejected: {reason}")
            }
        }
    }
}

impl std::error::Error for SemanticVectorGraphErrorV1 {}

/// A code-graph runtime retained for semantic-vector reads and writes.
///
/// The held authority lease pins the serving code generation for as long as
/// the handle lives, so vector reads never race generation retirement.
pub struct RetainedSemanticVectorGraphV1 {
    graph: Arc<GraphDb>,
    cancellation: Arc<dyn GraphCancellation>,
    _authority: Arc<dyn RetainedGraphStoreLeaseV1>,
}

impl RetainedSemanticVectorGraphV1 {
    pub fn new(
        graph: Arc<GraphDb>,
        cancellation: Arc<dyn GraphCancellation>,
        authority: Arc<dyn RetainedGraphStoreLeaseV1>,
    ) -> Self {
        Self {
            graph,
            cancellation,
            _authority: authority,
        }
    }

    pub fn graph(&self) -> &Arc<GraphDb> {
        &self.graph
    }

    pub fn cancellation(&self) -> &Arc<dyn GraphCancellation> {
        &self.cancellation
    }
}

/// Daemon-implemented resolution from code-generation identity to the retained
/// graph runtime that stores that scope's semantic vectors.
pub trait SemanticVectorGraphProviderV1: Send + Sync {
    /// Retain the graph runtime serving `generation`'s repository scope.
    fn graph_for_generation<'a>(
        &'a self,
        generation: &'a CodeIndexPublishedGenerationV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>,
    >;

    /// Retain the graph runtime serving the project's current code generation.
    fn graph_for_current(
        &self,
    ) -> SemanticRuntimeFuture<
        '_,
        Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>,
    >;
}
