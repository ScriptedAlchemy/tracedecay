//! Daemon-only `tracedecay_admin_sync` — still needs the code-index reconcile sink.

use super::*;

/// Daemon-only sync entry point used by the first-party CLI. It is deliberately
/// not advertised in the MCP catalog: external agents should rely on the
/// daemon watcher while the CLI can request an explicit serialized refresh.
#[hotpath::measure(label = "mcp.info.admin_sync.total")]
pub(crate) async fn handle_admin_sync(
    cg: &TraceDecay,
    args: Value,
    reconcile_sink: Option<&crate::mcp::server::CodeIndexReconcileSink>,
) -> Result<ToolResult> {
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    let project_root = cg.project_root().to_path_buf();
    let reconcile_sink = reconcile_sink.ok_or_else(|| {
        TraceDecayError::project_route(
            "code_index_scheduler_unavailable",
            true,
            "admin sync requires the daemon code-index scheduler",
        )
    })?;
    // The operator named this route (`tracedecay init` / `tracedecay sync`):
    // the one demand that may index a route the watcher policy keeps quiet.
    let admission = hotpath::future!(
        reconcile_sink(
            project_root.clone(),
            crate::mcp::server::CodeIndexReconcileDemandV1::Explicit,
        ),
        label = "mcp.info.admin_sync.reconcile"
    )
    .await;
    match admission {
        crate::mcp::server::CodeIndexAdmission::Accepted => {}
        crate::mcp::server::CodeIndexAdmission::PublicationAuthorityCorrupt(parked) => {
            return Err(crate::mcp::server::code_index_publication_corrupt(parked));
        }
        crate::mcp::server::CodeIndexAdmission::LinkedWorktreeDisabled => {
            return Err(crate::mcp::server::code_index_linked_worktree_disabled());
        }
        crate::mcp::server::CodeIndexAdmission::Unavailable => {
            return Err(TraceDecayError::project_route(
                crate::mcp::server::CODE_INDEX_SCHEDULER_UNAVAILABLE,
                true,
                "admin sync was not accepted by the code-index scheduler",
            ));
        }
    }
    let output = json!({
        "requested_mode": if force { "force" } else { "refresh" },
        "reconcile_scope": "authoritative_project",
        "status": "queued",
        "project_root": cg.project_root(),
    });
    let text = serde_json::to_string(&output)?;
    Ok(ToolResult::new(
        json!({
            "content": [{
                "type": "text",
                "text": text,
            }]
        }),
        Vec::new(),
    ))
}
