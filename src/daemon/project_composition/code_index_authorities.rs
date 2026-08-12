//! Code-index authorities shared by the core server, full server, and primitive runtime.

use std::path::Path;
use std::sync::Arc;

use tracedecay_application::ResolvedScope;

use crate::daemon::{DaemonInvocationState, project_open_owners};

pub(super) struct ProjectCodeIndexAuthoritiesV1 {
    publication_identity: crate::mcp::server::CodeIndexPublicationIdentityResolver,
    pub(super) graph_projection_read: crate::mcp::server::CodeGraphProjectionReadPort,
    ignored_dependency_admission: crate::mcp::server::CodeIndexIgnoredDependencyAdmissionPort,
    pub(super) generation_census: crate::runtime_telemetry::GenerationCensusReader,
}

impl ProjectCodeIndexAuthoritiesV1 {
    pub(super) fn mount(
        &self,
        context: crate::mcp::server::McpServerConstructionContext,
    ) -> crate::mcp::server::McpServerConstructionContext {
        context
            .with_code_index_publication_identity(Arc::clone(&self.publication_identity))
            .with_code_graph_projection_read_port(Arc::clone(&self.graph_projection_read))
            .with_code_index_ignored_dependency_admission(Arc::clone(
                &self.ignored_dependency_admission,
            ))
    }

    pub(super) fn install_generation_census(
        &self,
        server: &crate::mcp::McpServer,
        server_kind: &str,
    ) -> crate::errors::Result<()> {
        server
            .install_generation_census_reader(Arc::clone(&self.generation_census))
            .map_err(|_| crate::errors::TraceDecayError::Config {
                message: format!(
                    "{server_kind} MCP generation census authority was already installed"
                ),
            })
    }
}

pub(super) fn project_code_index_authorities(
    invocation: &DaemonInvocationState,
    canonical_project_root: &Path,
    scope: &ResolvedScope,
    database_writable: bool,
) -> ProjectCodeIndexAuthoritiesV1 {
    let schedulers = invocation.code_index_schedulers.clone();
    let project_root = canonical_project_root.to_path_buf();
    ProjectCodeIndexAuthoritiesV1 {
        publication_identity: Arc::new(schedulers.clone()),
        graph_projection_read: project_open_owners::project_code_graph_projection_read_port(
            schedulers.clone(),
            project_root.clone(),
            scope.clone(),
        ),
        ignored_dependency_admission:
            project_open_owners::project_code_index_ignored_dependency_admission_port(
                schedulers.clone(),
                project_root.clone(),
                scope.clone(),
                database_writable,
            ),
        generation_census: project_open_owners::project_code_index_generation_census_reader(
            schedulers,
            project_root,
            scope.clone(),
        ),
    }
}
