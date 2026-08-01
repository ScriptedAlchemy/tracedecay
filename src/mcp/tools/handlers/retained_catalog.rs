use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use tracedecay_application::handlers::CanonicalApplicationDispatcher;
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationOperation, RetainedSurfaceOperation,
    retained_surface_application_operation,
};
use tracedecay_tool_catalog::{BindingSurface, ProfileId, SurfaceOperationName};

use crate::catalog_composition::{ApplicationCatalogComposition, compose_application_catalog};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::TraceDecay;

use super::ToolCallRegistryOptions;
use super::ToolResult;
use super::dispatch_groups::execute_project_retained_application_tool;
use super::handle_user_lcm_tool_with_db;
use super::session;

#[derive(Debug)]
pub(super) struct CatalogBoundRetainedMcpRequest {
    pub(super) operation: RetainedSurfaceOperation,
    pub(super) arguments: Value,
}

#[derive(Clone, Copy)]
pub(super) enum RetainedMcpExecutionContext<'call, 'authority> {
    Profile {
        tool_name: &'call str,
        profile_root: &'call Path,
        options: &'call ToolCallRegistryOptions<'authority>,
    },
    Project {
        cg: &'call TraceDecay,
        scope_prefix: Option<&'call str>,
        active_project_session_db: Option<&'call Arc<RegisteredGlobalDb>>,
        active_lcm_context: session::LcmHandlerContext<'call>,
        options: &'call ToolCallRegistryOptions<'authority>,
    },
}

static RETAINED_MCP_COMPOSITION: OnceLock<
    std::result::Result<ApplicationCatalogComposition<()>, String>,
> = OnceLock::new();

struct RetainedMcpCatalogDispatcher<'call, 'authority> {
    context: RetainedMcpExecutionContext<'call, 'authority>,
}

type RetainedMcpInvocationFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send + 'a>>;

impl<'call> CanonicalApplicationDispatcher<CatalogBoundRetainedMcpRequest>
    for RetainedMcpCatalogDispatcher<'call, '_>
{
    type Output = RetainedMcpInvocationFuture<'call>;

    fn invoke(
        &self,
        operation: &ApplicationOperation,
        request: CatalogBoundRetainedMcpRequest,
    ) -> Self::Output {
        let expected = match retained_surface_application_operation(request.operation)
            .map_err(retained_catalog_error)
        {
            Ok(expected) => expected,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        if operation != &expected {
            let error = retained_catalog_error(
                "resolved retained MCP handler does not own the requested operation",
            );
            return Box::pin(async move { Err(error) });
        }
        let context = self.context;
        Box::pin(async move {
            match context {
                RetainedMcpExecutionContext::Profile {
                    tool_name,
                    profile_root,
                    options,
                } => {
                    execute_profile_retained_application_tool(
                        request,
                        tool_name,
                        profile_root,
                        options,
                    )
                    .await
                }
                RetainedMcpExecutionContext::Project {
                    cg,
                    scope_prefix,
                    active_project_session_db,
                    active_lcm_context,
                    options,
                } => {
                    execute_project_retained_application_tool(
                        request,
                        cg,
                        scope_prefix,
                        active_project_session_db,
                        active_lcm_context,
                        options,
                    )
                    .await
                }
            }
        })
    }
}

fn retained_catalog_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("retained application catalog is unavailable: {error}"),
    }
}

pub(super) fn retained_mcp_composition() -> Result<&'static ApplicationCatalogComposition<()>> {
    RETAINED_MCP_COMPOSITION
        .get_or_init(|| compose_application_catalog(()).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(retained_catalog_error)
}

pub(super) async fn invoke_retained_mcp_request(
    context: RetainedMcpExecutionContext<'_, '_>,
    operation: RetainedSurfaceOperation,
    arguments: Value,
) -> Result<ToolResult> {
    let composition = retained_mcp_composition()?;
    let profile_id =
        ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).map_err(retained_catalog_error)?;
    let operation_name =
        SurfaceOperationName::new(operation.as_str()).map_err(retained_catalog_error)?;
    let capability = composition
        .snapshot()
        .resolve_binding(
            &profile_id,
            BindingSurface::Mcp,
            &operation_name,
            1,
            &BTreeSet::new(),
        )
        .ok_or_else(|| retained_catalog_error("retained MCP binding is not callable"))?;
    let expected =
        retained_surface_application_operation(operation).map_err(retained_catalog_error)?;
    if capability.capability_id() != expected.capability_id()
        || capability.use_case_id() != expected.use_case_id()
    {
        return Err(retained_catalog_error(
            "retained MCP binding resolves a different application operation",
        ));
    }
    let dispatcher = RetainedMcpCatalogDispatcher { context };
    let handler = composition
        .bind_handler(capability.use_case_id(), &dispatcher)
        .ok_or_else(|| retained_catalog_error("retained MCP handler is not registered"))?;
    handler
        .invoke(CatalogBoundRetainedMcpRequest {
            operation,
            arguments,
        })
        .await
}

pub(super) async fn dispatch_profile_retained_application_tool(
    operation: RetainedSurfaceOperation,
    tool_name: &str,
    args: Value,
    profile_root: &Path,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    invoke_retained_mcp_request(
        RetainedMcpExecutionContext::Profile {
            tool_name,
            profile_root,
            options: &options,
        },
        operation,
        args,
    )
    .await
}

pub(super) async fn execute_profile_retained_application_tool(
    request: CatalogBoundRetainedMcpRequest,
    tool_name: &str,
    profile_root: &Path,
    options: &ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match request.operation {
        RetainedSurfaceOperation::MessageSearch
        | RetainedSurfaceOperation::LcmStatus
        | RetainedSurfaceOperation::LcmDoctor
        | RetainedSurfaceOperation::LcmLoadSession
        | RetainedSurfaceOperation::LcmGrep
        | RetainedSurfaceOperation::LcmDescribe
        | RetainedSurfaceOperation::LcmExpand
        | RetainedSurfaceOperation::LcmExpandQuery
        | RetainedSurfaceOperation::LcmPreflight
        | RetainedSurfaceOperation::LcmCompress
        | RetainedSurfaceOperation::LcmSessionBoundary => {
            handle_user_lcm_tool_with_db(
                tool_name,
                request.arguments,
                profile_root,
                options.session_authorities.user,
                options.global_db,
                options.session_authorities.profile_retrieval,
            )
            .await
        }
        _ => Err(TraceDecayError::Config {
            message: format!("storage_scope=user is not supported for `{tool_name}`"),
        }),
    }
}
