//! Cursor hook admission payloads.
//!
//! The host sends one native event to the daemon, but the durable queue keeps
//! only fields the replay worker needs. Prompt text, commands, tool output,
//! and applied edit bodies never enter the daemon admission spool.

use crate::application::host_admission::{HostAdmissionOutcome, SharedHostAdmissionBroker};
use crate::automation::config_error;
use crate::errors::Result;
use crate::mcp::hook_events::{HookEventPlan, encode_durable_hook_event_plan};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Component, Path};

const MAX_CURSOR_EVENT_TEXT_BYTES: usize = 4 * 1024;
const MAX_CURSOR_SESSION_ID_BYTES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CursorQueuedEventV1 {
    pub(crate) event_name: String,
    pub(crate) event: Value,
    #[serde(default)]
    pub(crate) rel_paths: Vec<String>,
}

fn allowed_event_name(event_name: &str) -> bool {
    matches!(
        event_name,
        "subagentStart"
            | "postToolUse"
            | "beforeSubmitPrompt"
            | "preCompact"
            | "afterFileEdit"
            | "sessionStart"
            | "sessionEnd"
            | "afterShellExecution"
            | "workspaceOpen"
            | "stop"
    )
}

fn bounded_text(value: &str, limit: usize) -> Option<String> {
    (!value.is_empty()
        && value.len() <= limit
        && !value.as_bytes().contains(&0)
        && !value.chars().any(char::is_control))
    .then(|| value.to_owned())
}

fn copy_text_field(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    key: &str,
    limit: usize,
) {
    if let Some(value) = source
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| bounded_text(value, limit))
    {
        target.insert(key.to_owned(), Value::String(value));
    }
}

fn copy_integer_field(source: &Map<String, Value>, target: &mut Map<String, Value>, key: &str) {
    let value = source.get(key).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
    });
    if let Some(value) = value {
        target.insert(key.to_owned(), Value::from(value));
    }
}

fn bounded_relative_path(value: &str) -> Option<String> {
    let normalized = value.replace('\\', "/");
    let path = Path::new(&normalized);
    (!normalized.is_empty()
        && normalized.len() <= 512
        && !normalized.as_bytes().contains(&0)
        && !normalized.chars().any(char::is_control)
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)))
    .then_some(normalized)
}

/// Select the bounded, content-free portion of one Cursor event for durable
/// replay. Invalid event shapes are rejected before the spool can retain them.
pub(crate) fn sanitize_cursor_event(
    event_name: &str,
    event: &Value,
) -> Result<CursorQueuedEventV1> {
    if !allowed_event_name(event_name) {
        return Err(config_error("unsupported Cursor hook event"));
    }
    let source = event
        .as_object()
        .ok_or_else(|| config_error("Cursor hook event must be a JSON object"))?;
    let mut retained = Map::new();
    for key in ["session_id", "conversation_id", "chat_id"] {
        copy_text_field(source, &mut retained, key, MAX_CURSOR_SESSION_ID_BYTES);
    }
    copy_text_field(
        source,
        &mut retained,
        "transcript_path",
        MAX_CURSOR_EVENT_TEXT_BYTES,
    );
    for key in [
        "messages_to_compact",
        "compact_count",
        "message_count",
        "messages_count",
        "context_tokens",
        "current_tokens",
        "tokens",
        "context_window_size",
        "context_length",
    ] {
        copy_integer_field(source, &mut retained, key);
    }
    Ok(CursorQueuedEventV1 {
        event_name: event_name.to_owned(),
        event: Value::Object(retained),
        rel_paths: Vec::new(),
    })
}

fn event_file_paths(event: &Value) -> impl Iterator<Item = &str> {
    event
        .get("file_path")
        .and_then(Value::as_str)
        .into_iter()
        .chain(
            event
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|edit| edit.get("file_path").and_then(Value::as_str)),
        )
        .chain(
            event
                .get("file_paths")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        )
}

fn add_relative_edit_paths(queued: &mut CursorQueuedEventV1, event: &Value, project_root: &Path) {
    if queued.event_name != "afterFileEdit" {
        return;
    }
    for path in event_file_paths(event) {
        let Ok(relative) = Path::new(path).strip_prefix(project_root) else {
            continue;
        };
        let Some(relative) = relative.to_str().and_then(bounded_relative_path) else {
            continue;
        };
        if !queued.rel_paths.contains(&relative) && queued.rel_paths.len() < 64 {
            queued.rel_paths.push(relative);
        }
    }
}

pub(crate) fn validate_queued_cursor_event(event: &CursorQueuedEventV1) -> bool {
    let Ok(normalized) = sanitize_cursor_event(&event.event_name, &event.event) else {
        return false;
    };
    normalized.event == event.event
        && event.rel_paths.len() <= 64
        && event
            .rel_paths
            .iter()
            .all(|path| bounded_relative_path(path).as_deref() == Some(path.as_str()))
}

fn cursor_admission_source(queued: &CursorQueuedEventV1) -> Result<String> {
    let encoded = serde_json::to_vec(queued)
        .map_err(|error| config_error(format!("serialize Cursor admission identity: {error}")))?;
    Ok(format!(
        "cursor:{}",
        crate::context::read_cache::digest_bytes(&encoded)
    ))
}

fn unavailable_receipt(outcome: HostAdmissionOutcome) -> Value {
    serde_json::json!({
        "action": "cursor_event",
        "status": "unavailable",
        "retryable": outcome.retryable,
        "reason_code": outcome.reason_code,
    })
}

/// Durably queue one privacy-filtered Cursor event for the daemon's admission
/// worker. This action intentionally does not index, ingest, compact, or call
/// a model before replying to the host hook.
pub(super) async fn admit_cursor_event(
    cg: &crate::tracedecay::TraceDecay,
    args: &Value,
    broker: Option<&SharedHostAdmissionBroker>,
) -> Result<Value> {
    let event_name = args
        .get("event_name")
        .and_then(Value::as_str)
        .ok_or_else(|| config_error("cursor_event requires event_name"))?;
    let event = args
        .get("event")
        .ok_or_else(|| config_error("cursor_event requires event"))?;
    let mut queued = sanitize_cursor_event(event_name, event)?;
    add_relative_edit_paths(&mut queued, event, cg.project_root());
    let payload = encode_durable_hook_event_plan(&HookEventPlan::CursorEvent(queued.clone()))
        .map_err(|()| config_error("Cursor hook event exceeded durable admission bounds"))?;
    let source = cursor_admission_source(&queued)?;
    let Some(broker) = broker else {
        return Ok(unavailable_receipt(
            HostAdmissionOutcome::retained_unavailable("spool_unavailable"),
        ));
    };
    match broker.admit(&source, &payload).await {
        Ok(admission) => Ok(serde_json::json!({
            "action": "cursor_event",
            "status": "deferred",
            "disposition": admission.outcome.status,
            "sequence": admission.seq,
        })),
        Err(outcome) => Ok(unavailable_receipt(outcome)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_cursor_events_never_accept_content_or_traversal_paths() {
        let source = serde_json::json!({
            "session_id": "cursor-session",
            "prompt": "private host prompt",
            "command": "private host command",
            "edits": [{ "new_string": "private edit body" }],
        });
        let queued = sanitize_cursor_event("afterFileEdit", &source).unwrap();
        assert_eq!(
            queued.event,
            serde_json::json!({ "session_id": "cursor-session" })
        );

        let invalid = CursorQueuedEventV1 {
            event_name: "afterFileEdit".to_owned(),
            event: queued.event,
            rel_paths: vec!["..\\private.rs".to_owned()],
        };
        assert!(!validate_queued_cursor_event(&invalid));
    }
}
