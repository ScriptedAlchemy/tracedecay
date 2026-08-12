use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde_json::Value;
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, RetainedSurfaceOperation,
    retained_surface_application_operation,
};
use tracedecay_tool_catalog::{BindingId, BindingSurface, ProfileId, SurfaceOperationName};

use crate::application_surface::normalize_application_tool_args;
use crate::catalog_composition::{ApplicationCatalogComposition, compose_application_catalog};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

use super::{ToolCallRegistryOptions, ToolResult, application_surface};

static RETAINED_MCP_COMPOSITION: OnceLock<
    std::result::Result<ApplicationCatalogComposition<()>, String>,
> = OnceLock::new();

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

fn retained_mcp_binding(operation: RetainedSurfaceOperation) -> Result<BindingId> {
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
    capability
        .binding_ids()
        .iter()
        .find(|binding_id| {
            composition
                .snapshot()
                .binding(binding_id)
                .is_some_and(|binding| {
                    binding.surface() == BindingSurface::Mcp
                        && binding.operation() == &operation_name
                })
        })
        .cloned()
        .ok_or_else(|| retained_catalog_error("retained MCP binding identity is unavailable"))
}

pub(crate) fn retained_mcp_operation(
    tool_name: &str,
    arguments: &Value,
) -> Option<RetainedSurfaceOperation> {
    match tool_name {
        "tracedecay_session_refresh" => match arguments.get("action").and_then(Value::as_str) {
            Some("status") => Some(RetainedSurfaceOperation::SessionRefreshStatus),
            Some("start" | "join" | "resume" | "begin") => {
                Some(RetainedSurfaceOperation::SessionRefreshBegin)
            }
            Some("cancel") => Some(RetainedSurfaceOperation::SessionRefreshCancel),
            _ => None,
        },
        _ => RetainedSurfaceOperation::from_tool_name(tool_name),
    }
}

pub(super) async fn dispatch_profile_retained_application_tool(
    operation: RetainedSurfaceOperation,
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    let authority = options
        .session_authorities
        .profile_retained_authority
        .ok_or_else(|| {
            TraceDecayError::project_route(
                "profile_retained_authority_unavailable",
                true,
                "profile retained authority is unavailable for this authenticated connection",
            )
        })?;
    execute_profile_retained_mcp_tool(
        operation,
        tool_name,
        args,
        cg.store_runtime_registry().as_ref(),
        authority,
        options.session_authorities.profile_lcm,
        options.application_request_id,
        options.application_deadline,
        options.application_cancellation,
        Some(cg.project_root()),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_profile_retained_mcp_tool(
    operation: RetainedSurfaceOperation,
    tool_name: &str,
    mut args: Value,
    runtime_registry: &crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1,
    authority: &crate::daemon::retained_owner::ProfileRetainedConnectionAuthorityV1,
    lcm_authority: Option<&dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>,
    protocol_request_id: Option<tracedecay_application::RequestId>,
    protocol_deadline: Option<tracedecay_application::Deadline>,
    protocol_cancellation: Option<tracedecay_application::CancellationSignal>,
    project_root: Option<&std::path::Path>,
) -> Result<ToolResult> {
    if let Some(arguments) = args.as_object_mut() {
        if tool_name.starts_with("tracedecay_lcm_") || tool_name == "tracedecay_message_search" {
            arguments.remove("storage_scope");
        }
        if tool_name == "tracedecay_session_refresh" {
            arguments.remove("action");
        }
    }
    let normalized = normalize_application_tool_args(tool_name, args).map_err(|error| {
        TraceDecayError::Config {
            message: error.to_string(),
        }
    })?;
    let requested_format = normalized.requested_format;
    let typed_request =
        crate::application_surface::retained::decode_request(operation, normalized.request)
            .ok_or_else(|| retained_catalog_error("retained MCP request is invalid"))?;
    if typed_request.operation() != operation {
        return Err(retained_catalog_error(
            "retained MCP request does not match its catalog operation",
        ));
    }
    let binding_id = retained_mcp_binding(operation)?;
    let request_id = match protocol_request_id {
        Some(request_id) => request_id,
        None => application_surface::request_id()?,
    };
    let (deadline, cancellation) = application_surface::complete_retained_protocol_controls(
        operation,
        &request_id,
        protocol_deadline,
        protocol_cancellation,
    )?
    .ok_or_else(|| {
        TraceDecayError::project_route(
            "profile_retained_controls_unavailable",
            true,
            "profile retained protocol controls are unavailable",
        )
    })?;
    let result = crate::daemon::retained_owner::execute_profile_retained_application(
        crate::daemon::retained_owner::ProfileRetainedAuthoritiesV1 {
            runtime_registry: Some(runtime_registry),
            session_identity: authority.session_identity().clone(),
            configuration_digest: authority.configuration_digest().clone(),
            lcm_authority,
        },
        authority,
        typed_request,
        request_id,
        deadline,
        cancellation,
    )
    .await?;
    application_surface::render_retained_result(
        project_root,
        operation,
        binding_id,
        result,
        requested_format,
    )
}
