//! Primitive runtime ownership installed after full project admission.

use std::path::Path;
use std::sync::Arc;

use tracedecay_application::primitives::{
    ProductionPrimitiveCodeAuthoritiesV1, ProductionPrimitiveOpenRequestV1,
    admitted_root_uri_for_project,
};
use tracedecay_application::source_authorization::ProjectSourceAccessSnapshot;

use crate::daemon::DaemonInvocationState;
use crate::mcp::McpServer;
use tracedecay_contracts::retrieval::{
    RetrievalPortContext, SessionLookupRequest, TemporalRetrievalFailure, TemporalRetrievalFuture,
    TemporalRetrievalPort,
};
use tracedecay_daemon_service::DaemonPrimitiveRuntimeRegistrationError;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::SourceReadContext;

/// Session lookups on a route whose session store is held in its typed
/// reset-required state: every lookup answers that refusal.
pub(super) struct ResetRequiredSessionLookupV1;

impl TemporalRetrievalPort for ResetRequiredSessionLookupV1 {
    fn session_lookup<'a>(
        &'a self,
        _context: RetrievalPortContext<'a>,
        _request: &'a SessionLookupRequest,
    ) -> TemporalRetrievalFuture<'a> {
        Box::pin(async { Err(TemporalRetrievalFailure::ResetRequired) })
    }
}

#[tracing::instrument(name = "daemon.project.owners.primitive", level = "trace", skip_all)]
pub(super) async fn open_and_register_project_primitive_runtime(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    source: SourceReadContext,
    server: &McpServer,
    session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    temporal: Arc<dyn TemporalRetrievalPort + Send + Sync>,
    access: ProjectSourceAccessSnapshot,
) -> Result<String> {
    let admitted_root_uri =
        admitted_root_uri_for_project(project_root).map_err(|error| TraceDecayError::Config {
            message: format!("project-open admitted root URI denied: {error}"),
        })?;
    let code_graph = server
        .code_graph_projection_read_port()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open primitive runtime requires the production code-graph projection authority"
                .to_owned(),
        })?;
    let ignored_dependency_admission = server
        .code_index_ignored_dependency_admission()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open primitive runtime requires the mounted ignored-dependency admission authority"
                .to_owned(),
        })?;
    invocation
        .primitive_runtime_registrar()
        .open_and_register(
            project_root.to_path_buf(),
            ProductionPrimitiveOpenRequestV1::new(
                Arc::new(source),
                ProductionPrimitiveCodeAuthoritiesV1 {
                    code_graph,
                    ignored_dependency_admission: Some(ignored_dependency_admission),
                    code_index: Arc::new(invocation.code_index_schedulers.clone()),
                    diagnostic_identity: Arc::new(invocation.code_index_schedulers.clone()),
                    convergence_park: Arc::new(invocation.code_index_schedulers.clone()),
                },
                session_db,
                temporal,
                access,
                admitted_root_uri.clone(),
            ),
        )
        .await
        .map_err(primitive_runtime_registration_error)?;
    Ok(admitted_root_uri)
}

fn primitive_runtime_registration_error(
    error: DaemonPrimitiveRuntimeRegistrationError,
) -> TraceDecayError {
    match error {
        error @ DaemonPrimitiveRuntimeRegistrationError::Open(_) => {
            TraceDecayError::Io(std::io::Error::other(error))
        }
        error => TraceDecayError::Config {
            message: format!("project-open primitive runtime registration failed: {error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use tracedecay_contracts::ApplicationContractError;
    use tracedecay_daemon_service::DaemonPrimitiveRuntimeRegistrationError;

    use super::primitive_runtime_registration_error;

    #[test]
    fn primitive_open_failure_preserves_field_specific_contract_cause() {
        let error = primitive_runtime_registration_error(
            DaemonPrimitiveRuntimeRegistrationError::Open(ApplicationContractError::Inconsistent {
                field: "application primitive session cursor key",
            }),
        );
        let io = error
            .source()
            .and_then(|source| source.downcast_ref::<std::io::Error>())
            .expect("typed root error source");
        let registration = io
            .get_ref()
            .and_then(|source| source.downcast_ref::<DaemonPrimitiveRuntimeRegistrationError>())
            .expect("typed primitive registration cause");
        let contract = registration
            .source()
            .and_then(|source| source.downcast_ref::<ApplicationContractError>())
            .expect("typed application contract cause");

        assert!(matches!(
            contract,
            ApplicationContractError::Inconsistent {
                field: "application primitive session cursor key"
            }
        ));
    }
}
