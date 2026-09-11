//! The narrow ports the source-edit slice consumes from its composition root.
//!
//! The root injects one implementation of [`SourceEditRuntimePort`] (authorized
//! worktree identity plus optional post-edit diagnostics) and supplies graph
//! evidence per request as one admitted, generation-pinned
//! [`SourceEditGraphReadV1`]. Planning, primitives, preview capture,
//! journaling, rollback, recovery, and reconciliation are owned here and never
//! delegated back through the port.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_code_index::graph_projection::CodeGraphInteractiveReader;
use tracedecay_domain::errors::Result;
use tracedecay_graph_db::GraphCancellation;
use tracedecay_runtime_core::storage::StoreLayout;

pub type SourceEditFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// One application-admitted immutable graph generation used for a source-edit
/// plan and its exact preview/apply identity.
#[derive(Clone)]
pub struct SourceEditGraphReadV1 {
    reader: CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
}

impl SourceEditGraphReadV1 {
    pub fn new(
        reader: CodeGraphInteractiveReader,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Self {
        Self {
            reader,
            cancellation,
        }
    }

    pub fn reader(&self) -> &CodeGraphInteractiveReader {
        &self.reader
    }

    pub fn cancellation(&self) -> Arc<dyn GraphCancellation> {
        Arc::clone(&self.cancellation)
    }
}

#[derive(Debug, Clone)]
pub struct EditDiagnosticRecord {
    pub file: String,
    pub line_start: u32,
    pub level: String,
    pub code: Option<String>,
    pub message: String,
}

/// Narrow root-owned worktree and diagnostics authority used by the
/// source-edit application.
///
/// Graph evidence is supplied separately as one admitted, generation-pinned
/// [`SourceEditGraphReadV1`]. This port must not grow graph-query, primitive,
/// journal, or recovery methods.
pub trait SourceEditRuntimePort: Send + Sync {
    fn project_root(&self) -> &Path;
    fn store_layout(&self) -> &StoreLayout;
    fn run_diagnostics<'a>(
        &'a self,
        file: &'a str,
    ) -> SourceEditFuture<'a, Vec<EditDiagnosticRecord>>;
}

pub type SourceEditRuntime = dyn SourceEditRuntimePort;
