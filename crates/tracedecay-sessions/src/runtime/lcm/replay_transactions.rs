use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::LcmRawMessage;

pub const ACTIVE_REPLAY_METADATA_KEY: &str = "lcm_active_replay";
pub const ACTIVE_REPLAY_MESSAGE_KEY: &str = "active_replay";

#[derive(Debug)]
pub struct ReplayUnit<'a> {
    pub messages: Vec<&'a LcmRawMessage>,
}

impl ReplayUnit<'_> {
    pub fn token_count(&self) -> i64 {
        self.messages
            .iter()
            .map(|message| replay_message_tokens(message))
            .sum()
    }
}

/// Back a bounded prefix off when its boundary bisects a tool transaction.
pub fn bounded_atomic_prefix_len(messages: &[LcmRawMessage], requested_len: usize) -> usize {
    let mut selected_len = requested_len.min(messages.len());
    for (start, end) in transaction_ranges(messages.iter()) {
        if start < selected_len && selected_len < end {
            selected_len = start;
        }
    }
    selected_len
}

/// Select one complete leading transaction when progress would otherwise be
/// zero. This is the only path allowed to exceed a configured prefix cap.
pub fn first_atomic_unit_len(messages: &[LcmRawMessage]) -> usize {
    transaction_ranges(messages.iter())
        .into_iter()
        .find_map(|(start, end)| (start == 0).then_some(end))
        .unwrap_or(1)
        .min(messages.len())
}

/// Move a suffix boundary backward when it falls inside a valid tool transaction.
pub fn atomic_tail_start(messages: &[LcmRawMessage], requested_start: usize) -> usize {
    let mut start = requested_start.min(messages.len());
    for (transaction_start, transaction_end) in transaction_ranges(messages.iter()) {
        if transaction_start < start && start < transaction_end {
            start = transaction_start;
        }
    }
    start
}

/// Build replay-selection units. Valid assistant call + consecutive result
/// transactions are atomic. Legacy orphan results are omitted; an unmatched
/// assistant call remains only when its visible content is useful, and the
/// final value normalizer strips its invalid `tool_calls` field.
pub fn replay_units<'a>(messages: &[&'a LcmRawMessage]) -> Vec<ReplayUnit<'a>> {
    let transaction_ranges = transaction_ranges(messages.iter().copied());
    let mut transaction_by_start = transaction_ranges.into_iter().peekable();
    let mut units = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        if let Some((start, end)) = transaction_by_start.peek().copied()
            && start == index
        {
            let _ = transaction_by_start.next();
            units.push(ReplayUnit {
                messages: messages[index..end].to_vec(),
            });
            index = end;
            continue;
        }
        let message = messages[index];
        if message.role == "tool" {
            index += 1;
            continue;
        }
        let trailing_open_call = index + 1 == messages.len() && tool_call_ids(message).is_some();
        if tool_call_ids(message).is_some()
            && message.content.trim().is_empty()
            && !trailing_open_call
        {
            index += 1;
            continue;
        }
        units.push(ReplayUnit {
            messages: vec![message],
        });
        index += 1;
    }
    units
}

fn transaction_ranges<'a>(
    messages: impl IntoIterator<Item = &'a LcmRawMessage>,
) -> Vec<(usize, usize)> {
    let messages = messages.into_iter().collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let Some(expected_ids) = tool_call_ids(messages[index]) else {
            index += 1;
            continue;
        };
        if messages[index].role != "assistant" || expected_ids.is_empty() {
            index += 1;
            continue;
        }
        let mut found_ids = BTreeSet::new();
        let mut end = index + 1;
        while end < messages.len() && messages[end].role == "tool" {
            if let Some(tool_call_id) = tool_result_id(messages[end]) {
                found_ids.insert(tool_call_id);
            }
            end += 1;
        }
        if !found_ids.is_empty()
            && found_ids.is_subset(&expected_ids)
            && end - index - 1 == found_ids.len()
        {
            ranges.push((index, end));
            index = end;
        } else {
            index += 1;
        }
    }
    ranges
}

fn tool_call_ids(message: &LcmRawMessage) -> Option<BTreeSet<String>> {
    let replay = active_replay_value(message)?;
    let tool_calls = replay.get("tool_calls")?.as_array()?;
    Some(
        tool_calls
            .iter()
            .filter_map(|tool_call| tool_call.get("id").and_then(Value::as_str))
            .filter(|tool_call_id| !tool_call_id.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn tool_result_id(message: &LcmRawMessage) -> Option<String> {
    active_replay_value(message)?
        .get("tool_call_id")?
        .as_str()
        .filter(|tool_call_id| !tool_call_id.is_empty())
        .map(str::to_string)
}

fn active_replay_value(message: &LcmRawMessage) -> Option<Value> {
    active_replay_message_from_metadata(message)
}

fn estimate_tokens(text: &str) -> i64 {
    text.split_whitespace().count().max(1) as i64
}

fn replay_message_tokens(message: &LcmRawMessage) -> i64 {
    let mut tokens = estimate_tokens(&message.content);
    if let Some(mut replay) = active_replay_value(message) {
        if let Some(object) = replay.as_object_mut() {
            object.remove("content");
            object.remove("role");
            object.remove("store_id");
        }
        let serialized = replay.to_string();
        tokens += serialized.len().div_ceil(4) as i64;
    }
    tokens
}

pub fn normalize_replay_tool_pairs(messages: &[Value]) -> Vec<Value> {
    let mut normalized = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        let mut message = messages[index].clone();
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "tool" {
            index += 1;
            continue;
        }
        if role != "assistant" || message.get("tool_calls").is_none() {
            normalized.push(message);
            index += 1;
            continue;
        }
        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut result_end = index + 1;
        while result_end < messages.len()
            && messages[result_end].get("role").and_then(Value::as_str) == Some("tool")
        {
            result_end += 1;
        }
        let open_call_ids = tool_calls
            .iter()
            .filter_map(|tool_call| tool_call.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>();
        if result_end == messages.len()
            && result_end == index + 1
            && !open_call_ids.is_empty()
            && open_call_ids
                .iter()
                .all(|tool_call_id| !tool_call_id.is_empty())
            && open_call_ids.iter().collect::<BTreeSet<_>>().len() == open_call_ids.len()
        {
            normalized.push(message);
            break;
        }
        let call_id_counts = tool_calls
            .iter()
            .filter_map(|tool_call| tool_call.get("id").and_then(Value::as_str))
            .fold(
                std::collections::BTreeMap::new(),
                |mut counts, tool_call_id| {
                    *counts.entry(tool_call_id.to_string()).or_insert(0usize) += 1;
                    counts
                },
            );
        let result_id_counts = messages[index + 1..result_end]
            .iter()
            .filter_map(|result| result.get("tool_call_id").and_then(Value::as_str))
            .fold(
                std::collections::BTreeMap::new(),
                |mut counts, tool_call_id| {
                    *counts.entry(tool_call_id.to_string()).or_insert(0usize) += 1;
                    counts
                },
            );
        let kept_calls = tool_calls
            .into_iter()
            .filter(|tool_call| {
                tool_call
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|tool_call_id| {
                        call_id_counts.get(tool_call_id) == Some(&1)
                            && result_id_counts.get(tool_call_id) == Some(&1)
                    })
            })
            .collect::<Vec<_>>();
        if kept_calls.is_empty() {
            message
                .as_object_mut()
                .map(|object| object.remove("tool_calls"));
            if !replay_content_text(&message).trim().is_empty() {
                normalized.push(message);
            }
        } else {
            message["tool_calls"] = Value::Array(kept_calls);
            normalized.push(message);
            normalized.extend(
                messages[index + 1..result_end]
                    .iter()
                    .filter(|result| {
                        result
                            .get("tool_call_id")
                            .and_then(Value::as_str)
                            .is_some_and(|tool_call_id| {
                                call_id_counts.get(tool_call_id) == Some(&1)
                                    && result_id_counts.get(tool_call_id) == Some(&1)
                            })
                    })
                    .cloned(),
            );
        }
        index = result_end;
    }
    normalized
}

fn replay_content_text(message: &Value) -> String {
    match message.get("content") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(content) => serde_json::to_string(content).unwrap_or_default(),
    }
}

pub fn raw_replay_message(message: &LcmRawMessage) -> Value {
    if let Some(mut replay) = active_replay_message_from_metadata(message) {
        replay["role"] = Value::String(message.role.clone());
        replay["store_id"] = Value::from(message.store_id);
        return replay;
    }
    json!({
        "role": message.role,
        "content": message.content,
        "store_id": message.store_id,
    })
}

fn active_replay_message_from_metadata(message: &LcmRawMessage) -> Option<Value> {
    let metadata: Value = serde_json::from_str(message.metadata_json.as_deref()?).ok()?;
    if metadata
        .get(ACTIVE_REPLAY_METADATA_KEY)
        .and_then(Value::as_bool)
        != Some(true)
    {
        return None;
    }
    let mut replay = metadata
        .get(ACTIVE_REPLAY_MESSAGE_KEY)
        .and_then(Value::as_object)
        .cloned()
        .or_else(|| legacy_active_replay_message_from_metadata(&metadata))?;
    if !replay.contains_key("content") {
        replay.insert(
            "content".to_string(),
            Value::String(message.content.clone()),
        );
    }
    strip_disposable_assistant_replay_sidecars(&mut replay, &message.role);
    Some(Value::Object(replay))
}

pub fn strip_disposable_assistant_replay_sidecars(
    replay: &mut Map<String, Value>,
    fallback_role: &str,
) {
    if !replay
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or(fallback_role)
        .eq_ignore_ascii_case("assistant")
    {
        return;
    }
    for key in [
        "codex_message_items",
        "codex_reasoning_items",
        "reasoning",
        "reasoning_content",
        "reasoning_details",
    ] {
        replay.remove(key);
    }
}

fn legacy_active_replay_message_from_metadata(metadata: &Value) -> Option<Map<String, Value>> {
    let mut replay = metadata.as_object()?.clone();
    replay.remove(ACTIVE_REPLAY_METADATA_KEY);
    replay.remove(ACTIVE_REPLAY_MESSAGE_KEY);
    replay.remove("ingest_protection");
    replay.remove("external_payload");
    replay.remove("payload_ref");
    replay.remove("byte_count");
    replay.remove("char_count");
    replay.remove("sha256");
    Some(replay)
}
