//! Primitive runtime ownership installed after full project admission.

use std::path::Path;
use std::sync::Arc;

use tracedecay_application::primitives::{
    ProductionPrimitiveCodeAuthoritiesV1, ProductionPrimitiveOpenRequestV1,
};
use tracedecay_application::source_authorization::ProjectSourceAccessSnapshot;

use crate::daemon::DaemonInvocationState;
use crate::mcp::McpServer;
use tracedecay_daemon_service::daemon_operation_event_authority;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_session_runtime::session_retrieval::DaemonSessionLookupPrimitiveV1;

#[hotpath::measure(label = "daemon.project.owners.primitive", future = true)]
pub(super) async fn open_and_register_project_primitive_runtime(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    graph: Arc<crate::tracedecay::TraceDecay>,
    server: &McpServer,
    session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
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
    let source = graph
        .source_read_context()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open primitive runtime requires an exact registered source identity"
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
                },
                session_db,
                temporal,
                access,
                admitted_root_uri.to_owned(),
                daemon_operation_event_authority(),
            ),
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open primitive runtime registration failed: {error}"),
        })
}
