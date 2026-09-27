//! `tracedecay_admin_sync`: the operator's code-index reconcile, which needs
//! the daemon's code-index reconcile sink.

use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexDemandAdmissionV1, CodeIndexDemandV1,
};
use tracedecay_contracts::retrieval::{
    AdminSyncAdmissionV1, AdminSyncReconcileScopeV1, AdminSyncResultV1,
};

use super::{Result, TraceDecay, TraceDecayError};

/// Queues the reconcile the first-party CLI asks for (`tracedecay init` /
/// `tracedecay sync`). It is never advertised: external agents rely on the
/// daemon watcher.
#[hotpath::measure(label = "mcp.info.admin_sync.total")]
pub(crate) async fn admin_sync(
    cg: &TraceDecay,
    reconcile_sink: Option<&crate::mcp::server::CodeIndexReconcileSink>,
) -> Result<AdminSyncResultV1> {
    let project_root = cg.project_root().to_path_buf();
    let reconcile_sink = reconcile_sink.ok_or_else(|| {
        TraceDecayError::project_route(
            crate::mcp::server::CODE_INDEX_SCHEDULER_UNAVAILABLE,
            true,
            "admin sync requires the daemon code-index scheduler",
        )
    })?;
    // The operator named this route: the one demand that may index a route
    // the watcher policy keeps quiet.
    let admission = hotpath::future!(
        reconcile_sink(project_root.clone(), CodeIndexDemandV1::OperatorReconcile),
        label = "mcp.info.admin_sync.reconcile"
    )
    .await;
    let status = match admission {
        CodeIndexDemandAdmissionV1::Queued => AdminSyncAdmissionV1::Queued,
        CodeIndexDemandAdmissionV1::NotApplicable => AdminSyncAdmissionV1::NotApplicable,
        CodeIndexDemandAdmissionV1::Terminal(parked) => {
            return Err(parked.publication_authority_corrupt_error());
        }
        CodeIndexDemandAdmissionV1::RefusedByPolicy => {
            return Err(crate::mcp::server::code_index_linked_worktree_disabled());
        }
        CodeIndexDemandAdmissionV1::Unavailable(cause) => {
            return Err(crate::mcp::server::code_index_unavailable_error(cause));
        }
    };
    Ok(AdminSyncResultV1 {
        reconcile_scope: AdminSyncReconcileScopeV1::AuthoritativeProject,
        status,
        project_root: project_root.to_string_lossy().into_owned(),
    })
}
