use serde_json::Value;

use super::contracts::LcmRawMessage;
use super::replay_transactions;

pub const DEFAULT_INCREMENTAL_MAX_DEPTH: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssemblyCapInput {
    pub max_assembly_tokens: Option<i64>,
    pub context_length: Option<i64>,
    pub reserve_tokens_floor: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub struct OverflowRecoveryCapInput<'a> {
    pub current_tokens: Option<i64>,
    pub max_assembly_tokens: Option<i64>,
    pub messages: &'a [Value],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CondensationPolicy {
    pub fan_in: usize,
    pub incremental_max_depth: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondensationSkipReason {
    BacklogPresent,
    AuxiliarySummarizer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondensationDecision {
    Skip(CondensationSkipReason),
    QueryCandidates(CondensationPolicy),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondensationCandidateDecision {
    SkipNotEnoughCandidates,
    Condense,
}

pub fn condensation_candidate_decision(
    candidate_count: usize,
    fan_in: usize,
) -> CondensationCandidateDecision {
    if candidate_count < fan_in {
        CondensationCandidateDecision::SkipNotEnoughCandidates
    } else {
        CondensationCandidateDecision::Condense
    }
}

pub fn incremental_max_depth_limit(configured: Option<i64>) -> i64 {
    match configured {
        Some(value) if value < 0 => i64::MAX,
        Some(value) => value,
        None => DEFAULT_INCREMENTAL_MAX_DEPTH,
    }
}

pub fn effective_assembly_token_cap(input: AssemblyCapInput) -> Option<i64> {
    let explicit_cap = input.max_assembly_tokens.filter(|cap| *cap > 0);
    let reserve_cap = match (
        input.context_length.filter(|length| *length > 0),
        input.reserve_tokens_floor.filter(|floor| *floor > 0),
    ) {
        (Some(context_length), Some(reserve_tokens_floor)) => {
            Some(context_length - reserve_tokens_floor).filter(|cap| *cap > 0)
        }
        _ => None,
    };
    [explicit_cap, reserve_cap]
        .into_iter()
        .flatten()
        .min()
        .map(|cap| cap.max(1))
}

pub fn overflow_recovery_assembly_cap(input: OverflowRecoveryCapInput<'_>) -> Option<i64> {
    let assembly_cap = input.max_assembly_tokens?;
    let Some(current_tokens) = input.current_tokens.filter(|tokens| *tokens > 0) else {
        return Some(assembly_cap);
    };
    if input.messages.is_empty() {
        return Some(assembly_cap);
    }
    let message_tokens = input
        .messages
        .iter()
        .map(|message| crate::lcm::estimate_tokens(&message_content(message)))
        .sum::<i64>();
    let overhead_tokens = (current_tokens - message_tokens).max(0);
    Some((assembly_cap - overhead_tokens).max(1))
}

pub fn has_eligible_backlog(backlog: &[LcmRawMessage], leaf_chunk_tokens: Option<i64>) -> bool {
    if backlog.is_empty() {
        return false;
    }

    match leaf_chunk_tokens.filter(|limit| *limit > 0) {
        Some(token_limit) => source_token_count(backlog) >= token_limit,
        None => true,
    }
}

pub fn effective_leaf_chunk_tokens(
    leaf_chunk_tokens: Option<i64>,
    dynamic_leaf_chunk_enabled: Option<bool>,
    dynamic_leaf_chunk_max: Option<i64>,
    raw_tokens_outside_tail: i64,
) -> Option<i64> {
    if !dynamic_leaf_chunk_enabled.unwrap_or(false) {
        return leaf_chunk_tokens;
    }
    let base = leaf_chunk_tokens.unwrap_or(1).max(1);
    let ceiling = dynamic_leaf_chunk_max.unwrap_or(base).max(base);
    let mut working = base;
    while working < ceiling && raw_tokens_outside_tail > working.saturating_mul(2) {
        working = ceiling.min(working.saturating_mul(2));
    }
    Some(working)
}

pub fn bounded_leaf_chunk_len(
    backlog: &[LcmRawMessage],
    leaf_chunk_tokens: Option<i64>,
    max_source_messages: Option<usize>,
) -> usize {
    if backlog.is_empty() {
        return 0;
    }
    if leaf_chunk_tokens.is_none() && max_source_messages.is_none() {
        return backlog.len();
    }

    let max_messages = max_source_messages
        .filter(|limit| *limit > 0)
        .unwrap_or(backlog.len())
        .min(backlog.len());
    let token_limit = leaf_chunk_tokens.filter(|limit| *limit > 0);
    let mut selected_len = 0;
    let mut selected_tokens = 0;
    for message in backlog.iter().take(max_messages) {
        let message_tokens = crate::lcm::estimate_tokens(&message.content);
        if let Some(token_limit) = token_limit
            && selected_tokens + message_tokens > token_limit
        {
            break;
        }
        selected_tokens += message_tokens;
        selected_len += 1;
    }
    replay_transactions::bounded_atomic_prefix_len(backlog, selected_len)
}

pub fn progress_leaf_chunk_len(
    backlog: &[LcmRawMessage],
    leaf_chunk_tokens: Option<i64>,
    max_source_messages: Option<usize>,
) -> usize {
    let selected_len = bounded_leaf_chunk_len(backlog, leaf_chunk_tokens, max_source_messages);
    if selected_len == 0 && !backlog.is_empty() {
        replay_transactions::first_atomic_unit_len(backlog)
    } else {
        selected_len
    }
}

pub fn threshold_pressure(current_tokens: Option<i64>, threshold_tokens: Option<i64>) -> bool {
    match (current_tokens, threshold_tokens) {
        (Some(current_tokens), Some(threshold_tokens)) if threshold_tokens > 0 => {
            current_tokens >= threshold_tokens
        }
        _ => false,
    }
}

pub fn forced_overflow_pressure(
    current_tokens: Option<i64>,
    max_assembly_tokens: Option<i64>,
) -> bool {
    match (current_tokens, max_assembly_tokens) {
        (Some(current_tokens), Some(max_assembly_tokens)) if max_assembly_tokens > 0 => {
            current_tokens >= max_assembly_tokens
        }
        _ => false,
    }
}

pub fn source_token_count(backlog: &[LcmRawMessage]) -> i64 {
    backlog
        .iter()
        .map(|message| crate::lcm::estimate_tokens(&message.content))
        .sum::<i64>()
}

fn message_content(message: &Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };
    match content {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}
