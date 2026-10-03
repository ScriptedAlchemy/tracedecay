//! `tracedecay_retrieve`: one bounded page of a truncated tool response
//! cached in the project's response-handle store.
//!
//! The owner sizes the page by rendering it: the page limit shrinks until the
//! page's JSON-RPC response frame fits the response budget. Every surface
//! renders the returned page through [`render_retrieved_page`], so the frame a
//! client sends is the frame the owner measured.

use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    RetrieveHandleExpiredV1, RetrieveHandleMissingV1, RetrieveResultV1, RetrieveSurfaceRequestV1,
    RetrievedPageV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::tracedecay::current_timestamp;

use crate::handlers::graph::graph_tool_completion;
use crate::handlers::support::{decode_primitive_request, text_tool_result};
use crate::response_handles::{
    ResponseHandleLookup, ResponseHandleRecord, public_retrieve_error, retrieve_response_handle,
};
use crate::tools::render;
use crate::transport::JsonRpcResponse;
use crate::{MAX_RESPONSE_CHARS, ToolResult, serialize_response_line};

const RETRIEVE_PAGE_HEADER_ALLOWANCE: usize = 2_048;
/// Room the server keeps in the frame for what it appends after rendering
/// (the accounting footer and `_meta`).
const RETRIEVE_FRAME_RESERVED_BYTES: usize = 256;
const RETRY_INSTRUCTION: &str = "Re-run the original MCP tool in this project to regenerate the full response and a fresh handle.";

#[tracing::instrument(name = "mcp.retrieve.handle.total", level = "trace", skip_all)]
pub async fn compute_retrieve(
    response_handle_root: &Path,
    args: &Value,
) -> Result<GraphToolCompletionV1> {
    let request: RetrieveSurfaceRequestV1 = decode_primitive_request(args, "tracedecay_retrieve")?;
    let offset = platform_size(request.offset.unwrap_or(0), "offset")?;
    let page_budget = MAX_RESPONSE_CHARS - RETRIEVE_PAGE_HEADER_ALLOWANCE;
    let max_chars = match request.max_chars {
        Some(0) => {
            return Err(TraceDecayError::project_route(
                "response_handle_invalid_page_size",
                false,
                "tracedecay_retrieve max_chars must be at least 1",
            ));
        }
        Some(requested) => platform_size(requested, "max_chars")?.min(page_budget),
        None => page_budget,
    };
    // The stored payload is by definition larger than the response cap, so
    // loading it back is real disk I/O that must not run inline on the async
    // dispatch worker.
    let lookup = {
        let root = response_handle_root.to_path_buf();
        let handle = request.handle.clone();
        tracing::Instrument::instrument(
            tokio::task::spawn_blocking(move || {
                retrieve_response_handle(&root, &handle, current_timestamp())
            }),
            tracing::trace_span!("mcp.retrieve.handle.load"),
        )
        .await
        .map_err(|join_error| TraceDecayError::Config {
            message: format!("response handle retrieval task failed: {join_error}"),
        })?
        .map_err(public_retrieve_error)?
    };
    let result = match lookup {
        ResponseHandleLookup::Found(record) => {
            RetrieveResultV1::Page(fitted_page(&record, offset, max_chars, args)?)
        }
        ResponseHandleLookup::Missing => RetrieveResultV1::Missing(RetrieveHandleMissingV1 {
            handle: request.handle,
            expired: None,
            content: None,
            reason_code: "handle_not_found".to_owned(),
            message: "Response handle was not found in this project's local cache.".to_owned(),
            retryable: true,
            retry_instruction: RETRY_INSTRUCTION.to_owned(),
        }),
        ResponseHandleLookup::Expired {
            created_at,
            expires_at,
        } => RetrieveResultV1::Expired(RetrieveHandleExpiredV1 {
            handle: request.handle,
            expired: true,
            content: None,
            reason_code: "handle_expired".to_owned(),
            message: format!(
                "Response handle expired at {expires_at} and was removed from this project's local cache."
            ),
            retryable: true,
            retry_instruction: RETRY_INSTRUCTION.to_owned(),
            created_at,
            expires_at,
        }),
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::Retrieve(result),
        Vec::new(),
    ))
}

/// The longest page from `offset` of at most `max_chars` whose rendered
/// response frame fits the frame budget.
fn fitted_page(
    record: &ResponseHandleRecord,
    offset: usize,
    max_chars: usize,
    args: &Value,
) -> Result<RetrievedPageV1> {
    let total_chars = record.original_chars();
    if offset > total_chars {
        return Err(TraceDecayError::project_route(
            "response_handle_offset_out_of_range",
            false,
            format!(
                "tracedecay_retrieve offset {offset} exceeds stored response length {total_chars}"
            ),
        ));
    }
    let frame_budget = MAX_RESPONSE_CHARS - RETRIEVE_FRAME_RESERVED_BYTES;
    let mut page_limit = max_chars;
    loop {
        let content = response_handle_page(&record.content, offset, page_limit);
        let page_chars = content.chars().count();
        let next = offset.saturating_add(page_chars);
        let has_more = next < total_chars;
        let page = RetrievedPageV1 {
            handle: record.handle.clone(),
            expired: false,
            original_chars: total_chars as u64,
            total_chars: total_chars as u64,
            offset: offset as u64,
            next_offset: has_more.then_some(next as u64),
            has_more,
            created_at: record.created_at,
            expires_at: record.expires_at,
            content,
        };
        let frame = serialize_response_line(&JsonRpcResponse::success(
            Value::Null,
            render_retrieved_page(args, &page)?.value,
        ));
        if frame.len() <= frame_budget || page_limit == 1 || page_chars == 0 {
            return Ok(page);
        }
        let scaled = page_limit.saturating_mul(frame_budget) / frame.len();
        page_limit = scaled.clamp(1, page_limit - 1);
    }
}

/// Renders one retrieved page as its tool result. The owner sized the page
/// against exactly this rendering, so it never spills into another handle.
pub(crate) fn render_retrieved_page(args: &Value, page: &RetrievedPageV1) -> Result<ToolResult> {
    let text = if render::wants_json(args) {
        serde_json::to_value(page)?.to_string()
    } else {
        format!(
            "## Retrieved Response\n**handle:** `{}` ({} chars, expires at {})\n**offset:** {}\n**next_offset:** {}\n**has_more:** {}\n\n{}",
            page.handle,
            page.total_chars,
            page.expires_at,
            page.offset,
            page.next_offset
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            page.has_more,
            page.content,
        )
    };
    Ok(text_tool_result(&text, Vec::new()))
}

fn platform_size(value: u64, field: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| TraceDecayError::Config {
        message: format!("{field} exceeds this platform's supported range"),
    })
}

fn response_handle_page(content: &str, offset: usize, max_chars: usize) -> String {
    if content.is_ascii() {
        let end = offset.saturating_add(max_chars).min(content.len());
        return content[offset..end].to_owned();
    }
    let start_byte = char_offset_to_byte(content, offset);
    let end_byte = char_offset_to_byte(&content[start_byte..], max_chars) + start_byte;
    content[start_byte..end_byte].to_owned()
}

fn char_offset_to_byte(content: &str, offset: usize) -> usize {
    content
        .char_indices()
        .nth(offset)
        .map_or(content.len(), |(index, _)| index)
}
