//! Primitive runtime ownership installed after full project admission.

use std::path::Path;
use std::sync::Arc;

use tracedecay_usecases::primitives::{
    ProductionPrimitiveCodeAuthoritiesV1, ProductionPrimitiveOpenRequestV1,
    open_production_primitive_runtime,
};
use tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot;

use crate::daemon::service::invocation::daemon_operation_event_authority;
use crate::daemon::session_retrieval::DaemonSessionLookupPrimitiveV1;
use crate::daemon::{DaemonInvocationState, DaemonPrimitiveRuntimeRegistrationError};
use crate::errors::{Result, TraceDecayError};
use crate::mcp::McpServer;

pub(super) async fn open_and_register_project_primitive_runtime(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    graph: Arc<crate::tracedecay::TraceDecay>,
    server: &McpServer,
    session_db: crate::global_db::RegisteredGlobalDbLeaseV1,
    access: ProjectSourceAccessSnapshot,
    admitted_root_uri: &str,
) -> Result<()> {
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
    let temporal = Arc::new(DaemonSessionLookupPrimitiveV1::new(
        server.project_session_application_retrieval_service(&access.scope)?,
    ));
    let primitive_runtime =
        open_production_primitive_runtime(ProductionPrimitiveOpenRequestV1::new(
            graph,
            ProductionPrimitiveCodeAuthoritiesV1 {
                code_graph,
                ignored_dependency_admission: Some(ignored_dependency_admission),
                code_index: Arc::new(invocation.code_index_schedulers.clone()),
                diagnostic_identity: Arc::new(invocation.code_index_schedulers.clone()),
            },
            session_db,
            temporal,
            access,
            admitted_root_uri.to_owned(),
            daemon_operation_event_authority(),
        ))
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open primitive runtime open failed: {error}"),
        })?;
    match invocation
        .primitive_runtime_registrar()
        .register(project_root.to_path_buf(), primitive_runtime)
        .await
    {
        Ok(_) | Err(DaemonPrimitiveRuntimeRegistrationError::AlreadyRegistered) => Ok(()),
        Err(DaemonPrimitiveRuntimeRegistrationError::RegistryClosed) => {
            Err(TraceDecayError::Config {
                message: "project-open primitive runtime registration failed: the daemon project runtime registry is closed"
                    .to_owned(),
            })
        }
    }
}
