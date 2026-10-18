use std::path::Path;

use serde_json::Value;

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDb;

use super::support::{registered_project_context, validate_registered_project_selector_aliases};
use tracedecay_mcp::tools::binding::{
    tool_accepts_registered_project_selector, tool_dispatches_registered_project_reader,
};

pub(super) fn boxed_send<'a, T, F>(
    future: F,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>
where
    F: std::future::Future<Output = T> + Send + 'a,
{
    Box::pin(future)
}

pub(super) fn rejected_tool_project_selector_present(_tool_name: &str, args: &Value) -> bool {
    args.get("project_selector").is_some()
}

#[tracing::instrument(name = "mcp.project.route.resolve", level = "trace", skip_all)]
pub(crate) async fn resolve_registered_project_route_for_tool(
    tool_name: String,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
    resolver: Option<crate::mcp::server::RetainedProjectServerResolver>,
) -> Result<Option<crate::mcp::project_route::ResolvedProjectRoute>> {
    if tool_accepts_registered_project_selector(&tool_name) {
        validate_registered_project_selector_aliases(&args)?;
    }
    if !tool_dispatches_registered_project_reader(&tool_name) {
        return Ok(None);
    }
    let context = boxed_send(registered_project_context(&args, global_db));
    let Some(context) = context.await? else {
        return Ok(None);
    };

    let database = global_db.ok_or_else(|| {
        TraceDecayError::project_route(
            "project_route_not_authorized",
            false,
            "registered project route has no authenticated profile authority",
        )
    })?;
    let requested_path = context.project.canonical_root.clone();
    crate::mcp::project_route::resolve_registered_project_route(
        context,
        Path::new(&requested_path),
        database,
        resolver,
    )
    .await
    .map(Some)
}
