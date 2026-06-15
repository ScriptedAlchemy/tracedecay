//! Shared session-ingest abstractions and provider-neutral transcript helpers.
//!
//! These types and helpers sit below any particular session source adapter:
//! file-backed [`crate::sessions::source`] drivers and the Hermes `SQLite` sweep
//! both depend on them so they do not need to import from each other.

use std::path::Path;

use serde_json::Value;

use crate::sessions::SessionMessageRecord;

/// Counters returned by an ingestion pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptIngestStats {
    pub sessions_upserted: u64,
    pub messages_upserted: u64,
}

impl TranscriptIngestStats {
    /// Accumulate another pass's counters into this one.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            sessions_upserted: self
                .sessions_upserted
                .saturating_add(other.sessions_upserted),
            messages_upserted: self
                .messages_upserted
                .saturating_add(other.messages_upserted),
        }
    }
}

/// The incremental position persisted between ingestion runs.
///
/// `position` is interpreted per cursor kind: a byte offset (`ByteOffset`), a
/// stable 64-bit content hash prefix (`ContentHash`), or a last-seen `rowid`
/// (`RowCursor`). `mtime` is the file modification time in epoch seconds, used
/// to detect rewrites and to skip unchanged files cheaply.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StoredCursor {
    pub position: u64,
    pub mtime: u64,
    pub file_id: u64,
}

/// Mapped rows read past the stored cursor, plus the advanced cursor.
pub struct NewRows<T> {
    pub items: Vec<T>,
    pub new_cursor: StoredCursor,
}

/// **`RowCursor`** reader for SQLite-backed transcript stores (Zed, Copilot CLI
/// `session-store.db`).
///
/// Selects rows whose rowid is greater than `prev.position` (the last-seen
/// rowid), ordered ascending, mapping each through `map_row` *during* iteration
/// (libsql rows must not outlive the cursor) and advancing the stored cursor to
/// the maximum rowid seen. `select_sql` must select the rowid as its first
/// column and accept a single `?` bound to the previous rowid, e.g.
/// `"SELECT rowid, role, text FROM turns WHERE rowid > ? ORDER BY rowid"`.
/// Fail-open: any query error yields `None`; `map_row` returning `None` skips
/// that row while still advancing the cursor.
pub async fn read_new_rows<T>(
    conn: &libsql::Connection,
    select_sql: &str,
    prev: StoredCursor,
    mut map_row: impl FnMut(i64, &libsql::Row) -> Option<T>,
) -> Option<NewRows<T>> {
    let mut result_rows = match conn
        .query(select_sql, libsql::params![prev.position as i64])
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::debug!(
                select_sql,
                previous_rowid = prev.position,
                error = %error,
                "skipping transcript row source query"
            );
            return None;
        }
    };

    let mut items = Vec::new();
    let mut max_rowid = prev.position;
    while let Ok(Some(row)) = result_rows.next().await {
        let Ok(rowid) = row.get::<i64>(0) else {
            tracing::debug!(
                select_sql,
                "skipping transcript row without rowid in column 0"
            );
            continue;
        };
        if rowid as u64 > max_rowid {
            max_rowid = rowid as u64;
        }
        if let Some(item) = map_row(rowid, &row) {
            items.push(item);
        }
    }

    Some(NewRows {
        items,
        new_cursor: StoredCursor {
            position: max_rowid,
            // Row stores have no single file mtime; the rowid alone is the
            // monotonic cursor, so mtime is left as a sentinel.
            mtime: 0,
            file_id: 0,
        },
    })
}

/// Compare two paths for equality, canonicalizing when possible so that
/// symlinks/`..`/trailing differences do not cause false mismatches. Falls back
/// to a literal comparison when canonicalization fails (e.g. a path that no
/// longer exists).
pub(crate) fn paths_equal(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Collapse whitespace and clip to a short preview suitable for a session title.
pub(crate) fn preview_title(text: &str) -> String {
    const MAX_TITLE_CHARS: usize = 80;
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_TITLE_CHARS {
        collapsed
    } else {
        collapsed.chars().take(MAX_TITLE_CHARS).collect()
    }
}

/// Return the storage representation used by LCM raw ingest for provider
/// transcript content. This intentionally matches the active-message path:
/// strings stay strings, structured content is compact JSON.
pub(crate) fn message_storage_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    serde_json::to_string(content).unwrap_or_else(|_| content.to_string())
}

/// Return lossless storage text plus tool names discovered in either structured
/// content blocks or a sibling `tool_calls` field.
pub(crate) fn content_storage_text_and_tools(
    content: &Value,
    tool_calls: Option<&Value>,
) -> (String, Vec<String>) {
    let mut tools = Vec::new();
    collect_tool_names(content, &mut tools);
    if let Some(tool_calls) = tool_calls {
        collect_tool_names(tool_calls, &mut tools);
    }
    tools.sort();
    tools.dedup();
    (message_storage_text(content), tools)
}

pub(crate) fn append_tool_calls_metadata(
    map: &mut serde_json::Map<String, Value>,
    message: &Value,
) {
    if let Some(tool_calls) = message.get("tool_calls") {
        map.insert("tool_calls".to_string(), tool_calls.clone());
    }
}

/// Token-usage counter keys recognized by the savings dashboard
/// (`dashboard/savings_api.rs` `MESSAGE_TOKENS_CTE`): both the Anthropic
/// (`input_tokens`/`output_tokens`/`cache_*`) and `OpenAI`
/// (`prompt_tokens`/`completion_tokens`) shapes, plus `total_tokens` for
/// reference.
const USAGE_COUNTER_KEYS: [&str; 7] = [
    "input_tokens",
    "output_tokens",
    "prompt_tokens",
    "completion_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "total_tokens",
];

/// Extracts a `usage` counters object from a transcript record/message,
/// keeping only recognized numeric token counters (so arbitrarily large or
/// provider-private payloads never bloat `metadata_json`). Returns `None`
/// when the value has no `usage` object or it carries no recognized counters.
pub(crate) fn usage_counters_from(value: &Value) -> Option<Value> {
    let usage = value.get("usage")?.as_object()?;
    let mut counters = serde_json::Map::new();
    for key in USAGE_COUNTER_KEYS {
        if let Some(count) = usage.get(key).and_then(Value::as_i64) {
            counters.insert(key.to_string(), Value::from(count));
        }
    }
    (!counters.is_empty()).then_some(Value::Object(counters))
}

/// Inserts transcript-recorded token usage into message metadata under the
/// `usage` key the savings dashboard reads. Probes each candidate value in
/// order and keeps the first recognized counters object.
pub(crate) fn append_usage_metadata(
    map: &mut serde_json::Map<String, Value>,
    candidates: &[&Value],
) {
    if map.contains_key("usage") {
        return;
    }
    if let Some(usage) = candidates
        .iter()
        .find_map(|value| usage_counters_from(value))
    {
        map.insert("usage".to_string(), usage);
    }
}

fn collect_tool_names(value: &Value, tools: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_tool_names(item, tools);
            }
        }
        Value::Object(map) => {
            if matches!(
                map.get("type").and_then(Value::as_str),
                Some("tool_use" | "tool_call" | "function_call")
            ) {
                if let Some(name) = map.get("name").and_then(Value::as_str) {
                    tools.push(name.to_string());
                }
            }
            for key in ["tool_call", "functionCall", "function_call", "function"] {
                if let Some(name) = map
                    .get(key)
                    .and_then(Value::as_object)
                    .and_then(|nested| nested.get("name"))
                    .and_then(Value::as_str)
                {
                    tools.push(name.to_string());
                }
            }
            if let Some(tool_calls) = map.get("tool_calls") {
                collect_tool_names(tool_calls, tools);
            }
        }
        _ => {}
    }
}

fn title_text_from_stored_content(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| visible_text_from_content(&value))
        .unwrap_or_else(|| text.to_string())
}

fn visible_text_from_content(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(visible_text_from_content)
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        }
        Value::Object(map) => {
            for key in ["text", "content", "message"] {
                if let Some(text) = map.get(key).and_then(Value::as_str) {
                    return Some(text.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

/// Build a session title from the first user message, if any.
pub(crate) fn title_from_messages(messages: &[SessionMessageRecord]) -> Option<String> {
    messages
        .iter()
        .find(|message| message.role == "user")
        .map(|message| preview_title(&title_text_from_stored_content(&message.text)))
}
