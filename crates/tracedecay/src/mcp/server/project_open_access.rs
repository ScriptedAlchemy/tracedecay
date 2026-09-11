//! Project-open authority accessors installed on an MCP server.

use super::{CodeGraphProjectionReadPort, CodeIndexIgnoredDependencyAdmissionPort, McpServer};

impl McpServer {
    pub(crate) fn daemon_invocation_service(
        &self,
    ) -> Option<&tracedecay_daemon_service::DaemonInvocationService> {
        self.daemon_invocation_service.as_ref()
    }

    pub(crate) fn code_graph_projection_read_port(&self) -> Option<CodeGraphProjectionReadPort> {
        self.code_graph_projection_read_port.clone()
    }

    pub(crate) fn code_index_ignored_dependency_admission(
        &self,
    ) -> Option<CodeIndexIgnoredDependencyAdmissionPort> {
        self.code_index_ignored_dependency_admission.clone()
    }

    pub(crate) fn install_generation_census_reader(
        &self,
        reader: tracedecay_session_memory::runtime_telemetry::GenerationCensusReader,
    ) -> std::result::Result<(), tracedecay_session_memory::runtime_telemetry::GenerationCensusReader>
    {
        self.generation_census_reader
            .set(reader)
            .map_err(|error| match error {
                tokio::sync::SetError::AlreadyInitializedError(reader)
                | tokio::sync::SetError::InitializingError(reader) => reader,
            })
    }

    pub(crate) fn generation_census_reader(
        &self,
    ) -> Option<tracedecay_session_memory::runtime_telemetry::GenerationCensusReader> {
        self.generation_census_reader.get().cloned()
    }
}
