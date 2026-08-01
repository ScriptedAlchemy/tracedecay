//! Per-turn `token_count` usage accounting for Codex rollouts.

use serde_json::Value;

use crate::runtime::SessionMessageRecord;

/// Accumulates per-API-call `token_count` usage across one turn's tool loop.
///
/// Codex emits one `token_count` event per API call: the tool-loop calls
/// report *during* the turn (before the final `agent_message`) and the final
/// call reports right after it. Real rollouts on this machine showed ~64% of
/// input spend in those mid-turn reports, so honest cost accounting must sum
/// every call rather than keep only the one following the assistant reply.
/// Consecutive events whose cumulative `total_token_usage.total_tokens` did
/// not advance are duplicate reports of the same call and are skipped.
///
/// Counters are normalized for the savings dashboard's additive pricing
/// (Anthropic semantics): `OpenAI` `input_tokens` *includes*
/// `cached_input_tokens`, so the cached portion is split out into
/// `cache_read_input_tokens` and `input_tokens` keeps only the uncached
/// remainder.
#[derive(Default)]
pub struct CodexTurnUsage {
    input: i64,
    output: i64,
    cache_read: i64,
    reasoning: i64,
    total: i64,
    seen: bool,
    last_cumulative: Option<i64>,
}

impl CodexTurnUsage {
    /// Consume a rollout line when it is a `token_count` event, adding its
    /// per-call counters to the running turn sums. Returns `true` for every
    /// `token_count` line (even malformed or duplicate ones, which add
    /// nothing) and `false` for any other line kind.
    pub fn observe(&mut self, record: &Value) -> bool {
        if record.get("type").and_then(Value::as_str) != Some("event_msg") {
            return false;
        }
        let Some(payload) = record.get("payload") else {
            return false;
        };
        if payload.get("type").and_then(Value::as_str) != Some("token_count") {
            return false;
        }
        let Some(info) = payload.get("info") else {
            return true;
        };
        let cumulative = info
            .pointer("/total_token_usage/total_tokens")
            .and_then(Value::as_i64);
        if cumulative.is_some() && cumulative == self.last_cumulative {
            return true;
        }
        if cumulative.is_some() {
            self.last_cumulative = cumulative;
        }
        let Some(last) = info
            .get("last_token_usage")
            .or_else(|| info.get("total_token_usage"))
        else {
            return true;
        };
        let input = last
            .get("input_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let output = last
            .get("output_tokens")
            .or_else(|| last.get("completion_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let cached = last
            .get("cached_input_tokens")
            .or_else(|| last.get("cache_read_input_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0);
        let reasoning = last
            .get("reasoning_output_tokens")
            .or_else(|| last.get("reasoning_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0);
        let total = last
            .get("total_tokens")
            .and_then(Value::as_i64)
            .or(cumulative)
            .unwrap_or_else(|| input.saturating_add(output).saturating_add(reasoning));
        if input == 0 && output == 0 && cached == 0 && reasoning == 0 && total == 0 {
            return true;
        }
        self.input = self
            .input
            .saturating_add((input.saturating_sub(cached)).max(0));
        self.cache_read = self.cache_read.saturating_add(cached);
        self.reasoning = self.reasoning.saturating_add(reasoning);
        self.output = self
            .output
            .saturating_add(output.max(0).saturating_add(reasoning));
        self.total = self.total.saturating_add(total.max(0));
        self.seen = true;
        true
    }

    /// The summed counters as a dashboard-shaped usage object, resetting the
    /// turn sums (the cumulative-total dedup guard survives across turns).
    pub fn take(&mut self) -> Option<Value> {
        if !self.seen {
            return None;
        }
        let mut usage = serde_json::Map::new();
        usage.insert("input_tokens".to_string(), Value::from(self.input));
        usage.insert("output_tokens".to_string(), Value::from(self.output));
        if self.cache_read > 0 {
            usage.insert(
                "cache_read_input_tokens".to_string(),
                Value::from(self.cache_read),
            );
        }
        if self.reasoning > 0 {
            usage.insert("reasoning_tokens".to_string(), Value::from(self.reasoning));
        }
        if self.total > 0 {
            usage.insert("total_tokens".to_string(), Value::from(self.total));
        }
        self.input = 0;
        self.output = 0;
        self.cache_read = 0;
        self.reasoning = 0;
        self.total = 0;
        self.seen = false;
        Some(Value::Object(usage))
    }
}

/// Add `add`'s numeric counters field-wise into `existing` (both are usage
/// objects). Used when several flushes land on the same assistant message
/// (e.g. an aborted turn with no reply of its own).
pub fn merge_usage_counters(existing: &mut Value, add: &Value) {
    let (Some(map), Some(add_map)) = (existing.as_object_mut(), add.as_object()) else {
        return;
    };
    for (key, value) in add_map {
        if let Some(count) = value.as_i64() {
            let current = map.get(key).and_then(Value::as_i64).unwrap_or(0);
            map.insert(key.clone(), Value::from(current.saturating_add(count)));
        }
    }
}

/// Attach the finished turn's summed usage to the most recent assistant
/// message of the batch (the reply the turn's `token_count` events report
/// on), merging additively when that message already carries usage.
pub fn flush_turn_usage(messages: &mut [SessionMessageRecord], turn_usage: &mut CodexTurnUsage) {
    let Some(usage) = turn_usage.take() else {
        return;
    };
    let Some(message) = messages
        .iter_mut()
        .rev()
        .find(|message| message.role == "assistant")
    else {
        return;
    };
    let mut metadata = message
        .metadata_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    match metadata.get_mut("usage") {
        Some(existing) => merge_usage_counters(existing, &usage),
        None => {
            metadata.insert("usage".to_string(), usage);
        }
    }
    if let Ok(serialized) = serde_json::to_string(&Value::Object(metadata)) {
        message.metadata_json = Some(serialized);
    }
}
