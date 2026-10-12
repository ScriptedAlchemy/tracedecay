//! Portable handler result and argument helpers used by graph-backed tools.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tracedecay_api::HttpApplicationControls;
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::{
    ApplicationProblemRecord, CancellationSignal, Deadline, RequestId, now_micros,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{
    CursorBindingMismatchV1, CursorBindingV1, UtcMicros, decode_bound_cursor, encode_bound_cursor,
};

use crate::ToolResult;
use crate::application_output::tool_result::problem_tool_result;
use crate::tool_errors::cursor_refusal;
use crate::tools::render;

/// A bound retrieval continuation stays inside the application cursor bound.
const MAX_RETRIEVAL_CURSOR_BYTES: usize = 8_192;

/// Decodes the authenticated retrieval continuation `encoded` names, when it
/// was minted for `binding`.
///
/// The cursor is caller-supplied, so it is bounded before parsing and then
/// validated: a continuation that does not authenticate is rejected rather
/// than treated as "start from the beginning", which would silently restart
/// a page walk instead of reporting the tampered envelope.
pub fn decode_retrieval_cursor(
    binding: &CursorBindingV1,
    encoded: Option<&str>,
) -> Result<Option<tracedecay_domain::RetrievalCursor>> {
    let Some(encoded) = encoded else {
        return Ok(None);
    };
    if encoded.len() > MAX_RETRIEVAL_CURSOR_BYTES {
        return Err(cursor_refusal(&CursorBindingMismatchV1::Foreign));
    }
    let cursor: tracedecay_domain::RetrievalCursor =
        decode_bound_cursor(binding, encoded).map_err(|mismatch| cursor_refusal(&mismatch))?;
    cursor
        .validate()
        .map_err(|_| cursor_refusal(&CursorBindingMismatchV1::Foreign))?;
    Ok(Some(cursor))
}

/// The continuation a caller presents with `binding`'s request to read the
/// page after `cursor`.
pub fn encode_retrieval_cursor(
    binding: &CursorBindingV1,
    cursor: Option<&tracedecay_domain::RetrievalCursor>,
) -> Result<Option<String>> {
    cursor
        .map(|cursor| {
            encode_bound_cursor(binding, cursor).map_err(|error| TraceDecayError::Config {
                message: format!("failed to encode retrieval cursor: {error}"),
            })
        })
        .transpose()
}

/// The single wrapper every MCP tool handler returns through.
///
/// Renders the default-format (markdown) body with `md` and records
/// `touched_files`. The `format:"json"` path is unaffected,
/// [`render::finalize`] serializes `value` compactly there.
pub fn rendered_tool_result<F: FnOnce() -> String>(
    response_handle_root: Option<&Path>,
    args: &Value,
    value: &Value,
    touched_files: Vec<String>,
    md: F,
) -> ToolResult {
    let text = render::finalize(response_handle_root, args, value, md);
    text_tool_result(&text, touched_files).with_structured_result(value.clone())
}

/// An owner's `{kind, value}` HTTP envelope as the tool result: an answer
/// as its JSON body, a problem as the one refusing tool result with the
/// typed record at `structuredContent.problem`.
pub fn owner_envelope_result(payload: &Value) -> Result<ToolResult> {
    if payload.get("kind").and_then(Value::as_str) != Some("problem") {
        return Ok(json_result(payload).with_semantic_error(false));
    }
    let problem: ApplicationProblemRecord =
        serde_json::from_value(payload["value"]["problem"].clone()).map_err(|error| {
            TraceDecayError::Config {
                message: format!("the owner's problem record does not decode: {error}"),
            }
        })?;
    problem_tool_result(&payload.to_string(), &problem)
}

/// The fallback request id an owner adapter (`owner` is `Work` or
/// `Workflow`) mints when the protocol supplied none.
pub(crate) fn mint_owner_request_id(owner: &str) -> Result<RequestId> {
    mint_global_request_id(GlobalRequestSurface::McpFallback).map_err(|_| TraceDecayError::Config {
        message: format!("could not allocate a {owner} request id"),
    })
}

/// Bounds the protocol deadline by the owner binding's maximum and adopts the
/// protocol cancellation, minting either one the protocol did not supply.
pub(crate) fn owner_request_controls(
    owner: &str,
    maximum_millis: u64,
    request_id: &RequestId,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<HttpApplicationControls> {
    let maximum_micros =
        i64::try_from(Duration::from_millis(maximum_millis).as_micros()).map_err(|_| {
            TraceDecayError::project_route(
                format!("{}.deadline_unavailable", owner.to_ascii_lowercase()),
                false,
                format!("The canonical {owner} deadline exceeds the domain clock"),
            )
        })?;
    let maximum_deadline = UtcMicros(now_micros().0.saturating_add(maximum_micros));
    let deadline = protocol_deadline
        .filter(|deadline| deadline.expires_at <= maximum_deadline)
        .map_or_else(|| Deadline::new(maximum_deadline), Ok)
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
    let cancellation = protocol_cancellation
        .map_or_else(
            || CancellationSignal::active(format!("cancellation.{}", request_id.as_str())),
            Ok,
        )
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
    Ok(HttpApplicationControls {
        deadline,
        cancellation,
    })
}

/// A compact JSON payload rendered as the tool's text content.
pub fn json_result(value: &Value) -> ToolResult {
    text_tool_result(&value.to_string(), Vec::new()).with_structured_result(value.clone())
}

pub fn text_tool_result(text: &str, touched_files: Vec<String>) -> ToolResult {
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    )
}

/// [`rendered_tool_result`] for handlers that don't need a custom markdown
/// renderer, the default body is [`render::generic_md`] over the same value.
pub fn generic_tool_result(
    response_handle_root: Option<&Path>,
    args: &Value,
    value: &Value,
    touched_files: Vec<String>,
) -> ToolResult {
    rendered_tool_result(response_handle_root, args, value, touched_files, || {
        render::generic_md(value)
    })
}

/// [`generic_tool_result`] for handlers that touch no files.
pub fn tool_json(response_handle_root: Option<&Path>, args: &Value, value: &Value) -> ToolResult {
    generic_tool_result(response_handle_root, args, value, Vec::new())
}

/// The single rejection every dispatch family returns for a name it does not own.
pub fn unknown_tool_error(tool_name: &str) -> TraceDecayError {
    TraceDecayError::project_route("unknown_tool", false, format!("unknown tool: {tool_name}"))
}

/// Rejects tool arguments that are not a JSON object.
pub fn require_object_args(args: &Value, tool_name: &str) -> Result<()> {
    if args.is_object() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!("invalid arguments: {tool_name} expects a JSON object"),
    })
}

/// Decode one catalog-owned primitive request after removing keys owned by
/// the MCP transport rather than the application operation. The session and
/// thread keys are host route identity: the server routes the call by them
/// before dispatch, so they never belong to the request body.
pub fn decode_primitive_request<T: DeserializeOwned>(args: &Value, tool_name: &str) -> Result<T> {
    decode_request_without(args, tool_name, &["project_selector"])
}

/// [`decode_primitive_request`] for a request whose own contract names a
/// registered project through `project_selector`, so routing never consumed
/// it.
pub fn decode_selector_request<T: DeserializeOwned>(args: &Value, tool_name: &str) -> Result<T> {
    decode_request_without(args, tool_name, &[])
}

fn decode_request_without<T: DeserializeOwned>(
    args: &Value,
    tool_name: &str,
    routed_keys: &[&str],
) -> Result<T> {
    require_object_args(args, tool_name)?;
    let mut request = args.clone();
    if let Some(object) = request.as_object_mut() {
        for key in [
            "format",
            "__mcp_request_id",
            "_meta",
            "session_id",
            "sessionId",
            "thread_id",
            "threadId",
        ]
        .iter()
        .chain(routed_keys)
        {
            object.remove(*key);
        }
    }
    serde_json::from_value(request).map_err(|error| TraceDecayError::Config {
        message: format!("invalid arguments for {tool_name}: {error}"),
    })
}

/// Rejects a zero result limit with a typed error.
pub fn require_positive_limit(limit: usize, tool_name: &str) -> Result<()> {
    if limit == 0 {
        return Err(TraceDecayError::Config {
            message: format!("invalid parameter: {tool_name} requires limit to be at least 1"),
        });
    }
    Ok(())
}

pub fn unique_file_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for path in paths {
        if seen.insert(path) {
            result.push(path.to_string());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_contracts::retrieval::NodeSurfaceRequestV1;

    use super::{decode_primitive_request, require_positive_limit, unique_file_paths};

    #[test]
    fn primitive_decode_strips_transport_keys_and_rejects_legacy_aliases() {
        let decoded = decode_primitive_request::<NodeSurfaceRequestV1>(
            &json!({
                "node_id": "function:canonical",
                "format": "json",
                "project_selector": {"project_id": "project.fixture"},
                "__mcp_request_id": "request.fixture",
                "session_id": "session.route",
                "_meta": {"threadId": "thread.route"},
            }),
            "tracedecay_node",
        )
        .expect("transport keys must not enter the canonical request body");
        assert_eq!(decoded.node_id, "function:canonical");

        for invalid in [
            json!({"node_id": "function:canonical", "project_id": "project.legacy"}),
            json!({"id": "function:legacy"}),
        ] {
            let error =
                decode_primitive_request::<NodeSurfaceRequestV1>(&invalid, "tracedecay_node")
                    .expect_err("legacy request must fail");
            let message = error.to_string();
            assert!(
                message.contains("unknown field `project_id`")
                    || message.contains("unknown field `id`")
                    || message.contains("missing field `node_id`"),
                "legacy request must name the rejected field, got {message} for {invalid}"
            );
        }
    }

    #[test]
    fn positive_limit_and_unique_paths_keep_validation_parity() {
        assert!(matches!(
            require_positive_limit(1, "tracedecay_fixture"),
            Ok(())
        ));
        let error =
            require_positive_limit(0, "tracedecay_fixture").expect_err("zero limit must fail");
        assert!(error.to_string().contains("limit to be at least 1"));

        assert_eq!(
            unique_file_paths(["src/a.rs", "src/b.rs", "src/a.rs"].into_iter()),
            vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()]
        );
    }
}
