use serde_json::Value;
use tracedecay_application::RetainedSurfaceOperation;

use super::session;
use crate::errors::{Result, TraceDecayError};

/// Projectless daemon path over the daemon-mounted LCM command/query and
/// canonical temporal retrieval authorities.
pub(crate) async fn handle_user_lcm_tool_with_authorities(
    tool_name: &str,
    args: Value,
    lcm_authority: Option<&dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>,
    retrieval_service: Option<&dyn session::message_search::SessionRetrievalServicePort>,
) -> Result<crate::mcp::tools::ToolResult> {
    if args.get("storage_scope").and_then(Value::as_str) != Some("user") {
        return Err(TraceDecayError::Config {
            message: "projectless LCM dispatch requires storage_scope=user".to_string(),
        });
    }
    if [
        "project_id",
        "project_path",
        "project_root",
        "project_scope",
        "project_selector",
    ]
    .iter()
    .any(|key| args.get(*key).is_some())
    {
        return Err(TraceDecayError::Config {
            message:
                "storage_scope=user cannot be combined with a project selector or project_scope"
                    .to_string(),
        });
    }
    if tool_name == "tracedecay_message_search" {
        // `storage_scope=user` already refused `project_scope` above, so this
        // profile lane never fans out over the registry.
        return session::message_search::handle_message_search_with_service(
            None,
            session::message_search::SessionRetrievalStoreScope::Profile,
            args,
            retrieval_service,
            None,
        )
        .await;
    }
    let context =
        session::LcmHandlerContext::user(retrieval_service).with_lcm_authority(lcm_authority);
    let operation =
        RetainedSurfaceOperation::from_name(tool_name).ok_or_else(|| TraceDecayError::Config {
            message: format!("unknown user-scoped LCM tool: {tool_name}"),
        })?;
    dispatch_lcm_tool(operation, args, context).await
}

pub(super) async fn dispatch_lcm_tool(
    operation: RetainedSurfaceOperation,
    args: Value,
    context: session::LcmHandlerContext<'_>,
) -> Result<crate::mcp::tools::ToolResult> {
    match operation {
        RetainedSurfaceOperation::LcmStatus => session::handle_lcm_status(context, args).await,
        RetainedSurfaceOperation::LcmDoctor => session::handle_lcm_doctor(context, args).await,
        RetainedSurfaceOperation::LcmLoadSession => {
            session::handle_lcm_load_session(context, args).await
        }
        RetainedSurfaceOperation::LcmGrep => session::handle_lcm_grep(context, args).await,
        RetainedSurfaceOperation::LcmDescribe => session::handle_lcm_describe(context, args).await,
        RetainedSurfaceOperation::LcmExpand => session::handle_lcm_expand(context, args).await,
        RetainedSurfaceOperation::LcmExpandQuery => {
            session::handle_lcm_expand_query(context, args).await
        }
        RetainedSurfaceOperation::FactStore
        | RetainedSurfaceOperation::FactFeedback
        | RetainedSurfaceOperation::MemoryStatus
        | RetainedSurfaceOperation::SessionRefresh
        | RetainedSurfaceOperation::MessageSearch
        | RetainedSurfaceOperation::SessionsFor
        | RetainedSurfaceOperation::Workflows
        | RetainedSurfaceOperation::SessionStart
        | RetainedSurfaceOperation::SessionEnd => Err(TraceDecayError::Config {
            message: format!(
                "internal: retained operation `{}` is not an LCM handler",
                operation.as_str()
            ),
        }),
    }
}
