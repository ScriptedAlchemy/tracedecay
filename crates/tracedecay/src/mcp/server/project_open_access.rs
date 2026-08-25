//! Project-open authority accessors installed on an MCP server.

use super::{
    CodeGraphProjectionReadPort, CodeIndexIgnoredDependencyAdmissionPort, McpServer,
    SourceEditExecutor, SourceEditReconciliationExecutor, SourceEditRollbackExecutor,
};

impl McpServer {
    /// Installs the sole source-edit invocation owner resolved during
    /// project-open admission. Reinstallation is rejected so a later caller
    /// cannot replace the authority behind an already-serving MCP instance.
    pub(crate) fn install_source_edit_executor(
        &self,
        executor: SourceEditExecutor,
    ) -> std::result::Result<(), SourceEditExecutor> {
        self.source_edit_executor
            .set(executor)
            .map_err(|error| match error {
                tokio::sync::SetError::AlreadyInitializedError(executor)
                | tokio::sync::SetError::InitializingError(executor) => executor,
            })
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
        reader: crate::runtime_telemetry::GenerationCensusReader,
    ) -> std::result::Result<(), crate::runtime_telemetry::GenerationCensusReader> {
        self.generation_census_reader
            .set(reader)
            .map_err(|error| match error {
                tokio::sync::SetError::AlreadyInitializedError(reader)
                | tokio::sync::SetError::InitializingError(reader) => reader,
            })
    }

    pub(crate) fn generation_census_reader(
        &self,
    ) -> Option<crate::runtime_telemetry::GenerationCensusReader> {
        self.generation_census_reader.get().cloned()
    }

    pub(crate) fn install_source_edit_reconciliation_executor(
        &self,
        executor: SourceEditReconciliationExecutor,
    ) -> std::result::Result<(), SourceEditReconciliationExecutor> {
        self.source_edit_reconciliation_executor
            .set(executor)
            .map_err(|error| match error {
                tokio::sync::SetError::AlreadyInitializedError(executor)
                | tokio::sync::SetError::InitializingError(executor) => executor,
            })
    }

    pub(crate) fn install_source_edit_rollback_executor(
        &self,
        executor: SourceEditRollbackExecutor,
    ) -> std::result::Result<(), SourceEditRollbackExecutor> {
        self.source_edit_rollback_executor
            .set(executor)
            .map_err(|error| match error {
                tokio::sync::SetError::AlreadyInitializedError(executor)
                | tokio::sync::SetError::InitializingError(executor) => executor,
            })
    }
}
