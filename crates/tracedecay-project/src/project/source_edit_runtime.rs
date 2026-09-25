//! Direct graph mutations are crate-internal adapters; external callers must
//! use the canonical source-edit transaction.

use std::path::Path;

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::storage::StoreLayout;
use tracedecay_source_edit::{EditDiagnosticRecord, SourceEditFuture, SourceEditRuntimePort};

use super::TraceDecay;

impl SourceEditRuntimePort for TraceDecay {
    fn project_root(&self) -> &Path {
        TraceDecay::project_root(self)
    }

    fn store_layout(&self) -> &StoreLayout {
        TraceDecay::store_layout(self)
    }

    fn run_diagnostics<'a>(
        &'a self,
        _file: &'a str,
    ) -> SourceEditFuture<'a, Vec<EditDiagnosticRecord>> {
        Box::pin(async {
            Err(TraceDecayError::project_route(
                "source_edit_diagnostics_unavailable",
                true,
                "source-edit verification requires the daemon-owned LSP diagnostics authority",
            ))
        })
    }
}
