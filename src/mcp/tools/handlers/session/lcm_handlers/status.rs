use super::super::lcm_args::*;
use super::super::lcm_storage::LcmHandlerContext;
use super::super::*;
use tracedecay_usecases::session::lcm::{
    LcmAuthorityOutcome, LcmAuthorityPayload, LcmAuthorityRequest, LcmDoctorQuery, LcmStatusQuery,
};

use super::shared::lcm_status_payload;

pub(in crate::mcp::tools::handlers) async fn handle_lcm_status(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_or_all_arg(&args)?;
    let session_id = string_arg(&args, "session_id");
    let deep = bool_arg(&args, "deep")?.unwrap_or(false);
    let Some(authority) = context.lcm_authority else {
        return Ok(super::super::lcm_storage::lcm_unavailable(&args));
    };
    let Some(response) = authority
        .execute(LcmAuthorityRequest::Status(LcmStatusQuery {
            provider: provider.to_owned(),
            session_id: session_id.map(str::to_owned),
            deep,
        }))
        .await
    else {
        return Ok(super::super::lcm_storage::lcm_unavailable(&args));
    };
    let Some(LcmAuthorityPayload::Status(status)) = response.payload else {
        return Ok(tool_json(
            context.project_root,
            &args,
            &json!({
                "status": "unavailable",
                "authority_outcome": response.outcome,
            }),
        )
        .with_semantic_error(true));
    };
    if response.outcome != LcmAuthorityOutcome::Ready {
        return Ok(tool_json(
            context.project_root,
            &args,
            &json!({
                "status": "unavailable",
                "authority_outcome": response.outcome,
            }),
        )
        .with_semantic_error(true));
    }
    Ok(tool_json(
        context.project_root,
        &args,
        &lcm_status_payload(provider, session_id, deep, status),
    ))
}

pub(in crate::mcp::tools::handlers) async fn handle_lcm_doctor(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let Some(authority) = context.lcm_authority else {
        return Ok(super::super::lcm_storage::lcm_unavailable(&args));
    };
    let Some(response) = authority
        .execute(LcmAuthorityRequest::Doctor(LcmDoctorQuery))
        .await
    else {
        return Ok(super::super::lcm_storage::lcm_unavailable(&args));
    };
    let Some(LcmAuthorityPayload::Doctor(report)) = response.payload else {
        return Ok(tool_json(
            context.project_root,
            &args,
            &json!({
                "status": "unavailable",
                "authority_outcome": response.outcome,
            }),
        )
        .with_semantic_error(true));
    };
    let Some(status) = report.get("status").and_then(Value::as_str) else {
        return Ok(tool_json(
            context.project_root,
            &args,
            &json!({
                "status": "unavailable",
                "reason": "invalid_lcm_doctor_payload",
                "authority_outcome": response.outcome,
            }),
        )
        .with_semantic_error(true));
    };
    let semantic_error = matches!(status, "unavailable" | "locked");
    Ok(tool_json(
        context.project_root,
        &args,
        &json!({
            "status": status,
            "authority_outcome": response.outcome,
            "health": report,
        }),
    )
    .with_semantic_error(semantic_error))
}

#[cfg(test)]
mod tests;
