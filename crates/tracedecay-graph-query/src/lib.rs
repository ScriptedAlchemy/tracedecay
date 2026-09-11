//! Generation-pinned verified code-graph queries over daemon-resolved
//! projections, plus the code-index-backed source readers
//! (`context::{read_modes, source_read, markdown_sections}`) that hydrate
//! source evidence for those queries.
//!
//! This crate sits below the transport adapters (`tracedecay-mcp`, the root
//! composition crate) and above the projection/store kernels
//! (`tracedecay-code-index`, `tracedecay-graph-db`,
//! `tracedecay-runtime-core`). It owns the [`VerifiedGraphQuery`] authority:
//! admission, source binding, and every analytical read run through the one
//! generation-pinned reader opened by [`open_verified_graph_query`], including
//! the redundancy and test-risk scans that consume that admitted handle.

use std::path::{Path, PathBuf};

use tracedecay_runtime_core::db::Database;

pub mod context;
pub mod health;
mod projection;
pub mod queries;
pub mod redundancy_scan;
pub mod scc;
mod source_authority;
pub mod test_risk;
mod verified_query;

pub use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
pub use tracedecay_code_index::graph_projection::{
    CodeGraphImpactBatchV1, CodeGraphSemanticEdgeV1, CodeGraphSymbolPageV1,
    CodeGraphSymbolSummaryV1,
};
pub use tracedecay_code_index::lineage::LineageSymbolRecordV1;

pub use projection::{
    CodeGraphProjectionReadPort, CodeGraphReadAdmissionFuture, CodeGraphReadAdmissionPort,
    CodeGraphReadAdmissionRequest, CodeGraphReadError, CodeGraphReadFreshnessV1,
    CodeGraphReadFuture, CodeGraphReadRequest, VerifiedCodeGraphRead,
    application_graph_cancellation, map_code_graph_read_runtime_error, map_projection_error,
    request_graph_cancellation,
};
pub use queries::{
    FileAdjacencyScan, GraphQueryManager, NodeMetrics, VerifiedHealthFileAggregateV1,
};
pub use source_authority::{
    CodeGraphSourceAuthorityPort, CodeGraphSourceBindFuture, CodeGraphSourceBindRequest,
};
#[cfg(any(test, feature = "test-helpers"))]
pub use verified_query::admitted_verified_graph_query_port;
pub use verified_query::{
    AdmittedVerifiedGraphQueryPort, VerifiedGraphQuery, VerifiedGraphQueryFuture,
    VerifiedGraphQueryPort, VerifiedGraphQueryRequest,
    admitted_verified_graph_query_port_with_source, open_verified_graph_query,
};

/// Immutable filesystem and cache values supplied for one admitted source
/// binding.
#[derive(Clone)]
pub struct SourceReadContext {
    project_root: PathBuf,
    db: Database,
    read_only: bool,
    project_id: String,
}

impl SourceReadContext {
    pub fn new(project_root: PathBuf, db: Database, read_only: bool, project_id: String) -> Self {
        Self {
            project_root,
            db,
            read_only,
            project_id,
        }
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }
}

/// Installs the registered global/session schema into the kernel's
/// fail-closed port for this crate's test process. The real schema is owned
/// by `tracedecay-global-db`; the port keeps the first registration, so every
/// fixture entry point can call this unconditionally.
#[cfg(test)]
pub(crate) fn register_test_schema_installer() {
    tracedecay_global_db::register_test_schema_installer();
}

#[cfg(test)]
mod verified_query_deadline_tests;
#[cfg(test)]
mod verified_query_source_tests;
#[cfg(test)]
mod verified_query_test_support;

#[cfg(test)]
mod source_read_context_tests {
    use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

    use super::SourceReadContext;

    #[tokio::test]
    async fn source_read_context_owns_exact_bound_values() {
        crate::register_test_schema_installer();
        let directory = tempfile::tempdir().expect("source read context");
        let database_path = directory.path().join("source.db");
        let authority = DatabaseAuthority::acquire_test(&database_path, "source read context")
            .expect("database authority");
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("source database");
        let project_root = directory.path().join("project");

        let source = SourceReadContext::new(
            project_root.clone(),
            database.clone(),
            true,
            "project.source-context".to_owned(),
        );

        assert_eq!(source.project_root(), project_root);
        assert_eq!(
            source.db().canonical_database_path(),
            database.canonical_database_path()
        );
        assert!(source.is_read_only());
        assert_eq!(source.project_id(), "project.source-context");
    }
}
