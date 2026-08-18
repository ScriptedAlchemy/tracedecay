use std::sync::Arc;

use crate::code_index::{
    CodeIndexIgnoredDependencyAdmissionErrorV1, CodeIndexIgnoredDependencyAdmissionPortV1,
    CodeIndexIgnoredDependencyAdmissionRequestV1,
};
use tracedecay_application::retrieval::{
    PrimitiveFailure, PrimitiveFailureKind, SymbolGraphPortContext, SymbolGraphScope,
};
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphProjectionError,
};
use tracedecay_temporal_query::ports::TemporalExecutionSnapshot;

use super::{
    CanonicalSymbolGraphAdapter, MAX_COMPATIBILITY_RESULTS, OpenSymbolGraph, SymbolGraphCursorPort,
    SymbolGraphPageClaim,
};

impl<C> CanonicalSymbolGraphAdapter<C> {
    pub fn new(
        code_graph: Arc<dyn crate::graph::CodeGraphProjectionReadPort>,
        cursors: C,
        ignored_dependency_admission: Option<Arc<dyn CodeIndexIgnoredDependencyAdmissionPortV1>>,
    ) -> Self {
        Self {
            code_graph,
            cursors,
            ignored_dependency_admission,
        }
    }
}

impl SymbolGraphPageClaim {
    pub fn new(
        temporal: TemporalExecutionSnapshot,
        code_generation_id: tracedecay_domain::CodeGenerationId,
        offset: usize,
    ) -> Self {
        Self {
            snapshot: crate::primitives::concrete::SymbolGraphCursorSnapshot::new(
                temporal,
                code_generation_id,
            ),
            offset,
        }
    }

    pub fn code_generation_id(&self) -> &tracedecay_domain::CodeGenerationId {
        self.snapshot.code_generation_id()
    }

    /// Offset into the claimed generation's frozen result set.
    pub const fn offset(&self) -> usize {
        self.offset
    }
}

pub(super) fn validate_claim_generation(
    claim: &SymbolGraphPageClaim,
    reader: &CodeGraphInteractiveReader,
) -> Result<(), PrimitiveFailure> {
    if claim.code_generation_id() != reader.generation() {
        return Err(failure(
            PrimitiveFailureKind::Stale,
            "application.symbol-graph.claim-generation-stale",
            "the claimed symbol-graph generation does not match the verified reader",
        ));
    }
    Ok(())
}

pub(super) struct IgnoredDependencyRequest<'a> {
    pub(super) lane: &'a str,
    pub(super) claim: &'a SymbolGraphPageClaim,
    pub(super) normal_results_empty: bool,
    pub(super) requested: bool,
    pub(super) query: &'a str,
    pub(super) scope: &'a SymbolGraphScope,
}

pub(super) async fn admit_ignored_dependency(
    admission: Option<&Arc<dyn CodeIndexIgnoredDependencyAdmissionPortV1>>,
    context: SymbolGraphPortContext<'_>,
    graph: &OpenSymbolGraph,
    cursors: &dyn SymbolGraphCursorPort,
    request: IgnoredDependencyRequest<'_>,
) -> Result<(), PrimitiveFailure> {
    if !request.normal_results_empty || !request.requested {
        return Ok(());
    }
    let imports = graph
        .reader
        .external_type_import_candidates(
            request.query,
            request.scope.path_prefix.as_deref(),
            MAX_COMPATIBILITY_RESULTS,
            Arc::clone(&graph.cancellation),
        )
        .map_err(ignored_dependency_candidate_failure)?;
    let Some(import) = imports.first() else {
        return Ok(());
    };
    let Some(admission) = admission else {
        return Err(failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-scheduler-unavailable",
            "ignored dependency indexing scheduler is unavailable",
        ));
    };
    cursors
        .finish_page(
            context.request,
            request.lane,
            request.claim,
            request.claim.offset(),
            0,
            false,
            context.observed_at,
        )
        .await?;
    let source_generation = graph.reader.generation();
    let result = admission
        .admit(CodeIndexIgnoredDependencyAdmissionRequestV1::new(
            context.request,
            source_generation,
            std::slice::from_ref(import),
        ))
        .await;
    match result {
        Ok(active_generation) if &active_generation != source_generation => Err(failure(
            PrimitiveFailureKind::Stale,
            "application.symbol-graph.ignored-dependency-generation-advanced",
            "ignored dependency indexing advanced the graph generation; retry the request",
        )),
        Ok(_) => Err(failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-generation-not-advanced",
            "ignored dependency indexing did not publish a newer graph generation",
        )),
        Err(error) => Err(admission_failure(error)),
    }
}

pub(in crate::primitives) fn ignored_dependency_candidate_failure(
    error: CodeGraphProjectionError,
) -> PrimitiveFailure {
    match error {
        CodeGraphProjectionError::Cancelled => failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-candidate-read-cancelled",
            "ignored dependency candidate reading was cancelled",
        ),
        CodeGraphProjectionError::DeadlineExceeded => failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-candidate-read-timed-out",
            "ignored dependency candidate reading timed out",
        ),
        CodeGraphProjectionError::GenerationMismatch => failure(
            PrimitiveFailureKind::Stale,
            "application.symbol-graph.ignored-dependency-candidate-generation-stale",
            "ignored dependency candidates belong to a stale graph generation",
        ),
        CodeGraphProjectionError::BudgetExhausted => failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-candidate-read-budget-exhausted",
            "ignored dependency candidate reading exhausted its operation budget",
        ),
        CodeGraphProjectionError::Contract(_)
        | CodeGraphProjectionError::ProjectionMismatch { .. }
        | CodeGraphProjectionError::RecoveredGenerationMismatch { .. }
        | CodeGraphProjectionError::ResetRequired(_)
        | CodeGraphProjectionError::Corrupt(_) => failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-candidate-read-corrupt",
            "ignored dependency candidate evidence is corrupt",
        ),
        CodeGraphProjectionError::Conflict
        | CodeGraphProjectionError::Unavailable(_)
        | CodeGraphProjectionError::DurabilityUncertain(_)
        | CodeGraphProjectionError::Closed => failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-candidate-read-unavailable",
            "ignored dependency candidate evidence is unavailable",
        ),
    }
}

fn admission_failure(error: CodeIndexIgnoredDependencyAdmissionErrorV1) -> PrimitiveFailure {
    match error {
        CodeIndexIgnoredDependencyAdmissionErrorV1::Unavailable { .. } => failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-scheduler-unavailable",
            "ignored dependency indexing scheduler is unavailable",
        ),
        CodeIndexIgnoredDependencyAdmissionErrorV1::ReadOnly => failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-read-only",
            "ignored dependency indexing is unavailable in read-only mode",
        ),
        CodeIndexIgnoredDependencyAdmissionErrorV1::Cancelled => failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-cancelled",
            "ignored dependency indexing was cancelled",
        ),
        CodeIndexIgnoredDependencyAdmissionErrorV1::TimedOut => failure(
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-timed-out",
            "ignored dependency indexing timed out",
        ),
        CodeIndexIgnoredDependencyAdmissionErrorV1::Stale { .. } => failure(
            PrimitiveFailureKind::Stale,
            "application.symbol-graph.ignored-dependency-generation-stale",
            "ignored dependency indexing rejected a stale source generation",
        ),
    }
}

fn failure(
    kind: PrimitiveFailureKind,
    code: &'static str,
    message: &'static str,
) -> PrimitiveFailure {
    PrimitiveFailure {
        kind,
        code: code.to_owned(),
        message: message.to_owned(),
    }
}
