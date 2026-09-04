//! Shared parsing and projection semantics for Codex's internal goal context.

use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// Native Codex record shape that carried a rendered goal context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexGoalContextSource {
    /// Legacy/user `response_item.message` representation.
    ResponseItem,
    /// Current `event_msg.item_completed/UserMessage` representation.
    ItemCompleted,
}

/// Content-private semantic identity used to pair Codex's two goal envelopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodexGoalContextCorrelation {
    identity: [u8; 32],
    source: CodexGoalContextSource,
}

impl CodexGoalContextCorrelation {
    /// SHA-256 of the canonical typed goal metadata, never its transcript text.
    pub fn identity(&self) -> [u8; 32] {
        self.identity
    }

    /// Native envelope shape that produced the projected message.
    pub fn source(&self) -> CodexGoalContextSource {
        self.source
    }
}

/// Collect visible text from current and legacy Codex content bags.
pub fn codex_message_visible_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(codex_message_visible_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                return text.to_string();
            }
            ["content", "message", "item"]
                .iter()
                .filter_map(|key| map.get(*key))
                .map(codex_message_visible_text)
                .find(|text| !text.is_empty())
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Structured semantics carried by a Codex internal goal-context message.
pub struct CodexGoalContext {
    objective: String,
    tokens_used: Option<i64>,
    token_budget: Option<i64>,
    token_budget_unbounded: bool,
    tokens_remaining: Option<i64>,
    tokens_remaining_unbounded: bool,
}

impl CodexGoalContext {
    /// Compact searchable text used by direct and canonical projections.
    pub fn storage_text(&self) -> String {
        format!("Codex active goal: {}", self.objective)
    }

    /// Typed goal metadata shared by direct and canonical projections.
    pub fn metadata(&self) -> Value {
        let mut goal = serde_json::Map::new();
        goal.insert("source".to_string(), Value::String("goal".to_string()));
        goal.insert(
            "objective".to_string(),
            Value::String(self.objective.clone()),
        );
        if let Some(tokens_used) = self.tokens_used {
            goal.insert("tokens_used".to_string(), Value::from(tokens_used));
        }
        if let Some(token_budget) = self.token_budget {
            goal.insert("token_budget".to_string(), Value::from(token_budget));
        }
        if self.token_budget_unbounded {
            goal.insert("token_budget_unbounded".to_string(), Value::from(true));
        }
        if let Some(tokens_remaining) = self.tokens_remaining {
            goal.insert(
                "tokens_remaining".to_string(),
                Value::from(tokens_remaining),
            );
        }
        if self.tokens_remaining_unbounded {
            goal.insert("tokens_remaining_unbounded".to_string(), Value::from(true));
        }
        Value::Object(goal)
    }

    /// Content-private identity shared by direct and canonical pairing.
    pub fn correlation_identity(&self) -> Option<[u8; 32]> {
        let encoded = serde_json::to_vec(&self.metadata()).ok()?;
        Some(Sha256::digest(encoded).into())
    }
}

/// Read the shared pairing identity from a projected Codex goal message.
pub fn codex_goal_context_correlation(
    kind: Option<&str>,
    metadata_json: Option<&str>,
) -> Option<CodexGoalContextCorrelation> {
    if kind != Some("goal_context") {
        return None;
    }
    let metadata = serde_json::from_str::<Value>(metadata_json?).ok()?;
    let source = match metadata.get("source_event").and_then(Value::as_str)? {
        "response_item" => CodexGoalContextSource::ResponseItem,
        "item_completed" => CodexGoalContextSource::ItemCompleted,
        _ => return None,
    };
    let encoded = serde_json::to_vec(metadata.get("codex_goal")?).ok()?;
    Some(CodexGoalContextCorrelation {
        identity: Sha256::digest(encoded).into(),
        source,
    })
}

/// Parse Codex's exact internal goal-context wrapper.
pub fn codex_goal_context_from_text(text: &str) -> Option<CodexGoalContext> {
    const START: &str = "<codex_internal_context source=\"goal\">";
    const END: &str = "</codex_internal_context>";
    let start = text.find(START)?;
    if !text[..start].trim().is_empty() {
        return None;
    }
    let after_start = &text[start + START.len()..];
    let end = after_start.find(END)?;
    if !after_start[end + END.len()..].trim().is_empty() {
        return None;
    }
    let body = &after_start[..end];
    let objective = tag_body(body, "objective")?.trim();
    if objective.is_empty() {
        return None;
    }
    let token_budget_line = budget_line_value(body, "Token budget:");
    let tokens_remaining_line = budget_line_value(body, "Tokens remaining:");
    Some(CodexGoalContext {
        objective: objective.to_string(),
        tokens_used: budget_line_value(body, "Tokens used:").and_then(parse_budget_count),
        token_budget: token_budget_line.and_then(parse_budget_count),
        token_budget_unbounded: token_budget_line.is_some_and(is_unbounded_budget_value),
        tokens_remaining: tokens_remaining_line.and_then(parse_budget_count),
        tokens_remaining_unbounded: tokens_remaining_line.is_some_and(is_unbounded_budget_value),
    })
}

fn tag_body<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");
    let after_start = text.split_once(&start_tag)?.1;
    Some(after_start.split_once(&end_tag)?.0)
}

fn budget_line_value<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("- ")?.trim().strip_prefix(prefix))
        .or_else(|| {
            text.lines()
                .map(str::trim)
                .find_map(|line| line.strip_prefix(prefix))
        })
        .map(str::trim)
}

fn parse_budget_count(value: &str) -> Option<i64> {
    let digits = value
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<i64>().ok()
    }
}

fn is_unbounded_budget_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "none" | "unbounded"
    )
}
