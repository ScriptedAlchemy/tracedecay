use tracedecay_domain::{GenerationDiagnosticV1, RetrievalAnchorId};
use tracedecay_lsp::{LspRuntimeFailure, LspRuntimeFuture};

/// Canonical managed-diagnostics read authority for one finding anchor.
pub trait LspFeedbackDiagnosticRecordPort: Send + Sync {
    fn diagnostic_by_anchor(
        &self,
        anchor: RetrievalAnchorId,
    ) -> LspRuntimeFuture<Result<Option<GenerationDiagnosticV1>, LspRuntimeFailure>>;
}
