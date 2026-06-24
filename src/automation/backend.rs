use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{Result, TraceDecayError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskKind {
    MemoryCurator,
    SessionReflector,
    SkillWriter,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTaskRequest {
    pub run_id: String,
    pub task: AgentTaskKind,
    pub prompt: String,
    #[serde(default)]
    pub context: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTaskResponse {
    pub run_id: String,
    pub task: AgentTaskKind,
    pub output_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_json: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

pub trait AgentTaskBackend {
    fn run_task(&self, request: &AgentTaskRequest) -> Result<AgentTaskResponse>;
}

pub fn extract_single_json_object(text: &str) -> Result<Value> {
    let candidate = strip_optional_json_fence(text)?;
    let mut deserializer = serde_json::Deserializer::from_str(candidate);
    let value = Value::deserialize(&mut deserializer)?;
    deserializer.end()?;
    if !value.is_object() {
        return config_error("automation backend output must be a JSON object");
    }
    Ok(value)
}

fn strip_optional_json_fence(text: &str) -> Result<&str> {
    let trimmed = text.trim();
    let Some(after_opening) = trimmed.strip_prefix("```") else {
        return Ok(trimmed);
    };
    let Some(closing_start) = after_opening.rfind("```") else {
        return config_error("automation backend JSON fence is missing closing fence");
    };
    let trailing = after_opening[closing_start + "```".len()..].trim();
    if !trailing.is_empty() {
        return config_error("automation backend JSON fence has trailing content");
    }
    let mut inner = &after_opening[..closing_start];
    if let Some(rest) = inner.strip_prefix("json") {
        inner = rest;
    }
    let inner = inner
        .strip_prefix('\n')
        .or_else(|| inner.strip_prefix("\r\n"))
        .unwrap_or(inner);
    Ok(inner.trim())
}

fn config_error<T>(message: impl Into<String>) -> Result<T> {
    Err(TraceDecayError::Config {
        message: message.into(),
    })
}
