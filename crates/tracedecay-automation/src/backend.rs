use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
pub use tracedecay_contracts::automation::{AgentTaskFailureClass, AgentTaskKind};
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;
use tracedecay_domain::errors::{ProjectOpenFailureKind, TraceDecayError};

use crate::config::AutomationBackend;
use crate::{AutomationError, Result, config_error};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTaskContract {
    pub task_key: String,
    pub prompt_version: String,
    pub response_schema: Value,
    pub strict_json: bool,
}

impl Default for AgentTaskContract {
    fn default() -> Self {
        agent_task_contract(AgentTaskKind::MemoryCurator)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTaskRequest {
    pub run_id: String,
    pub task: AgentTaskKind,
    #[serde(default)]
    pub contract: AgentTaskContract,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_hash: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub input_hash: String,
    #[serde(default)]
    pub context: Value,
}

impl AgentTaskRequest {
    pub fn new(
        run_id: String,
        task: AgentTaskKind,
        prompt: String,
        evidence_hash: Option<String>,
        context: Value,
    ) -> Self {
        let contract = agent_task_contract(task);
        let input_hash =
            request_input_hash(task, &contract, &prompt, evidence_hash.as_deref(), &context);
        Self {
            run_id,
            task,
            contract,
            prompt,
            evidence_hash,
            input_hash,
            context,
        }
    }

    #[must_use]
    pub fn with_strict_json(mut self, strict_json: bool) -> Self {
        self.contract.strict_json = strict_json;
        self.input_hash = request_input_hash(
            self.task,
            &self.contract,
            &self.prompt,
            self.evidence_hash.as_deref(),
            &self.context,
        );
        self
    }

    #[must_use]
    pub fn with_contract(mut self, contract: AgentTaskContract) -> Self {
        self.contract = contract;
        self.input_hash = request_input_hash(
            self.task,
            &self.contract,
            &self.prompt,
            self.evidence_hash.as_deref(),
            &self.context,
        );
        self
    }

    pub fn backend_message(&self) -> Result<String> {
        serde_json::to_string_pretty(&serde_json::json!({
            "run_id": self.run_id,
            "task": self.task,
            "contract": self.contract,
            "prompt": self.prompt,
            "evidence_hash": self.evidence_hash,
            "input_hash": self.input_hash,
            "context": self.context,
        }))
        .map_err(AutomationError::from)
    }
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
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentTaskFailureDisposition {
    pub classification: Option<AgentTaskFailureClass>,
    pub retryable: Option<bool>,
}

impl AgentTaskFailureDisposition {
    pub fn is_non_retryable(self) -> bool {
        self.retryable == Some(false)
    }
}

/// How the scheduler judges a recorded failure on a later run. The recorded
/// class is the authority: it was typed where the failure was observed.
pub fn agent_task_failure_disposition(
    recorded_classification: Option<AgentTaskFailureClass>,
    recorded_retryable: Option<bool>,
) -> AgentTaskFailureDisposition {
    let retryable = recorded_classification
        .map(AgentTaskFailureClass::is_retryable_on_later_run)
        .or(recorded_retryable);

    AgentTaskFailureDisposition {
        classification: recorded_classification,
        retryable,
    }
}

pub fn agent_task_contract(task: AgentTaskKind) -> AgentTaskContract {
    AgentTaskContract {
        task_key: task_key(task).to_string(),
        prompt_version: prompt_version(task).to_string(),
        response_schema: response_schema(task),
        strict_json: task != AgentTaskKind::UserJob,
    }
}

pub fn task_key(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator",
        AgentTaskKind::SessionReflector => "session_reflector",
        AgentTaskKind::SkillWriter => "skill_writer",
        AgentTaskKind::CombinedReview => "combined_review",
        AgentTaskKind::UserJob => "user_job",
    }
}

pub fn prompt_version(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator:v2",
        AgentTaskKind::SessionReflector => "session_reflector:v2",
        AgentTaskKind::SkillWriter => "skill_writer:v4",
        AgentTaskKind::CombinedReview => "combined_review:v3",
        AgentTaskKind::UserJob => "user_job:v2",
    }
}

fn response_schema(task: AgentTaskKind) -> Value {
    match task {
        AgentTaskKind::MemoryCurator => json_schema_for_array_properties(&["ops"]),
        AgentTaskKind::SessionReflector => json_schema_for_array_properties(&["facts"]),
        AgentTaskKind::SkillWriter => skill_writer_response_schema(),
        AgentTaskKind::CombinedReview => {
            let mut schema = response_schema(AgentTaskKind::SkillWriter);
            schema["required"] = serde_json::json!(["facts", "skills", "outcome", "decision"]);
            schema["properties"]["facts"] = serde_json::json!({
                "type": "array",
                "items": session_fact_schema()
            });
            schema
        }
        AgentTaskKind::UserJob => serde_json::json!({
            "type": "object",
            "additionalProperties": true
        }),
    }
}

fn session_fact_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["content", "category", "tags", "entities", "trust", "source_span", "reason"],
        "properties": {
            "content": { "type": "string" },
            "category": {
                "type": "string",
                "enum": ["general", "user_pref", "project", "tool", "decision", "code_area"]
            },
            "tags": {
                "type": "array",
                "items": { "type": "string" }
            },
            "entities": {
                "type": "array",
                "items": { "type": "string" }
            },
            "trust": { "type": "number" },
            "source_span": {
                "anyOf": [
                    {
                        "type": "object",
                        "required": ["session_id", "message_id"],
                        "properties": {
                            "session_id": { "type": "string" },
                            "message_id": { "type": "string" }
                        },
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "required": ["store_id"],
                        "properties": {
                            "store_id": { "type": "string" }
                        },
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "required": ["node_id"],
                        "properties": {
                            "node_id": { "type": "string" }
                        },
                        "additionalProperties": false
                    }
                ]
            },
            "reason": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn skill_writer_response_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["skills", "outcome", "decision"],
        "properties": {
            "skills": {
                "type": "array",
                "items": {
                    "anyOf": [
                        skill_create_schema(),
                        skill_update_schema(),
                        skill_merge_schema(),
                        skill_archive_schema()
                    ]
                }
            },
            "outcome": {
                "type": "string",
                "enum": ["skills_proposed", "no_skill_needed"]
            },
            "decision": {
                "type": ["object", "null"],
                "required": ["reason", "remedy"],
                "properties": {
                    "reason": { "type": "string" },
                    "remedy": {
                        "type": "string",
                        "enum": [
                            "improve_tool_description",
                            "improve_hint_routing",
                            "insufficient_repeated_evidence",
                            "generic_reasoning",
                            "one_off_task",
                            "no_action"
                        ]
                    }
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}

fn skill_create_schema() -> Value {
    let routing_validation = routing_validation_schema();
    serde_json::json!({
        "type": "object",
        "required": [
            "action", "id", "title", "summary", "routing_description", "category",
            "targets", "body_markdown", "support_files", "routing_validation", "reason"
        ],
        "properties": {
            "action": { "type": "string", "enum": ["create"] },
            "id": { "type": "string" },
            "title": { "type": "string" },
            "summary": { "type": "string" },
            "routing_description": { "type": "string" },
            "category": { "type": "string" },
            "targets": skill_targets_schema(false),
            "body_markdown": { "type": "string" },
            "support_files": support_files_schema(false),
            "routing_validation": routing_validation,
            "reason": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn skill_update_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": [
            "action", "id", "base_checksum", "title", "summary", "routing_description",
            "category", "targets", "body_markdown", "support_files", "pinned",
            "routing_validation", "reason"
        ],
        "properties": {
            "action": { "type": "string", "enum": ["update"] },
            "id": { "type": "string" },
            "base_checksum": { "type": "string" },
            "title": nullable_string_schema(),
            "summary": nullable_string_schema(),
            "routing_description": nullable_string_schema(),
            "category": nullable_string_schema(),
            "targets": skill_targets_schema(true),
            "body_markdown": nullable_string_schema(),
            "support_files": support_files_schema(true),
            "pinned": { "type": ["boolean", "null"] },
            "routing_validation": nullable_routing_validation_schema(),
            "reason": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn skill_merge_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": [
            "action", "id", "base_checksum", "source_skill_id", "source_base_checksum",
            "title", "summary", "routing_description", "category", "targets",
            "body_markdown", "support_files", "routing_validation", "reason"
        ],
        "properties": {
            "action": { "type": "string", "enum": ["merge"] },
            "id": { "type": "string" },
            "base_checksum": { "type": "string" },
            "source_skill_id": { "type": "string" },
            "source_base_checksum": { "type": "string" },
            "title": nullable_string_schema(),
            "summary": nullable_string_schema(),
            "routing_description": nullable_string_schema(),
            "category": nullable_string_schema(),
            "targets": skill_targets_schema(true),
            "body_markdown": nullable_string_schema(),
            "support_files": support_files_schema(true),
            "routing_validation": nullable_routing_validation_schema(),
            "reason": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn skill_archive_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["action", "id", "base_checksum", "reason"],
        "properties": {
            "action": { "type": "string", "enum": ["archive"] },
            "id": { "type": "string" },
            "base_checksum": { "type": "string" },
            "reason": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn nullable_string_schema() -> Value {
    serde_json::json!({ "type": ["string", "null"] })
}

fn skill_targets_schema(nullable: bool) -> Value {
    serde_json::json!({
        "type": if nullable { serde_json::json!(["array", "null"]) } else { serde_json::json!("array") },
        "items": {
            "type": "string",
            "enum": ["cursor", "codex", "claude", "agents", "opencode", "kimi", "kiro", "hermes"]
        }
    })
}

fn support_files_schema(nullable: bool) -> Value {
    serde_json::json!({
        "type": if nullable { serde_json::json!(["array", "null"]) } else { serde_json::json!("array") },
        "items": {
            "type": "object",
            "required": ["path", "text"],
            "properties": {
                "path": { "type": "string" },
                "text": { "type": "string" }
            },
            "additionalProperties": false
        }
    })
}

fn routing_validation_schema() -> Value {
    serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "required": [
                "id", "category", "hosts", "fixture", "status", "prompt",
                "ground_truth", "max_tool_calls", "expected_skill", "allowed_skills"
            ],
            "properties": {
                "id": { "type": "string" },
                "category": { "type": "string" },
                "hosts": {
                    "type": "array",
                    "items": { "type": "string", "enum": ["claude", "codex"] }
                },
                "fixture": { "type": "string" },
                "status": { "type": "string" },
                "prompt": { "type": "string" },
                "ground_truth": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "max_tool_calls": { "type": "integer" },
                "expected_skill": { "type": ["string", "null"] },
                "allowed_skills": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "additionalProperties": false
        }
    })
}

fn nullable_routing_validation_schema() -> Value {
    let mut schema = routing_validation_schema();
    schema["type"] = serde_json::json!(["array", "null"]);
    schema
}

fn json_schema_for_array_properties(properties: &[&str]) -> Value {
    let schema_properties: serde_json::Map<String, Value> = properties
        .iter()
        .map(|property| {
            (
                (*property).to_string(),
                serde_json::json!({ "type": "array" }),
            )
        })
        .collect();
    serde_json::json!({
        "type": "object",
        "required": properties,
        "properties": schema_properties,
        "additionalProperties": true
    })
}

fn request_input_hash(
    task: AgentTaskKind,
    contract: &AgentTaskContract,
    prompt: &str,
    evidence_hash: Option<&str>,
    context: &Value,
) -> String {
    let payload = serde_json::json!({
        "task": task,
        "task_key": contract.task_key,
        "prompt_version": contract.prompt_version,
        "strict_json": contract.strict_json,
        "response_schema": contract.response_schema,
        "evidence_hash": evidence_hash,
        "prompt": prompt,
        "context": context,
    });
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    encode_tagged_lowercase_hex("sha256:", &Sha256::digest(&bytes))
}

/// Typed failure surface of [`AgentTaskBackend::run_task`].
///
/// Denial, disconnect, and unavailability are distinct truthful states: a
/// denied task must not be retried as if the backend were merely absent, and
/// a mid-task disconnect is not a failure to reach the backend at all.
/// Each backend types the failure where it observes it (process spawn,
/// deadline, stdio, JSON-RPC error data); everything above the backend
/// consumes the typed variant.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AgentTaskError {
    /// The backend or its policy refused to run the task.
    #[error("agent task denied: {reason}")]
    Denied { reason: String },
    /// The backend was reached but the transport or session ended mid-task.
    #[error("agent task backend disconnected: {reason}")]
    Disconnected { reason: String },
    /// The backend could not be reached or started at all.
    #[error("agent task backend unavailable: {reason}")]
    Unavailable { reason: String },
    /// The backend did not finish inside its wall-clock budget.
    #[error("agent task timed out: {reason}")]
    Timeout { reason: String },
    /// The backend completed but its output violated the response contract.
    #[error("agent task returned malformed output: {reason}")]
    MalformedOutput { reason: String },
    /// The backend refused the request as larger than its input limit.
    #[error("agent task input is too large: {reason}")]
    InputTooLarge { reason: String },
    /// The backend reported a transient condition (rate limit, overload).
    #[error("agent task backend asked to retry: {reason}")]
    Retryable { reason: String },
    /// The task failed in a way that has no dedicated typed state.
    #[error("agent task failed: {reason}")]
    Failed { reason: String },
}

/// The typed failure class of local automation work (store writes, project
/// routing, host CLIs) that failed after the backend answered.
pub fn runtime_failure_class(error: &TraceDecayError) -> AgentTaskFailureClass {
    match error {
        TraceDecayError::LockDeadline { .. } | TraceDecayError::SyncLock { .. } => {
            AgentTaskFailureClass::Retryable
        }
        TraceDecayError::ProjectRoute { retryable, .. } => {
            if *retryable {
                AgentTaskFailureClass::Retryable
            } else {
                AgentTaskFailureClass::Permanent
            }
        }
        TraceDecayError::ProjectOpen { kind, .. } => match kind {
            ProjectOpenFailureKind::IndexMissing
            | ProjectOpenFailureKind::CodeRuntimeBudgetExhausted { .. }
            | ProjectOpenFailureKind::BackedOff { .. }
            | ProjectOpenFailureKind::AuthorityVerdict {
                migration_pending: true,
            } => AgentTaskFailureClass::Retryable,
            ProjectOpenFailureKind::IdentityUnregistered
            | ProjectOpenFailureKind::AuthorityVerdict {
                migration_pending: false,
            } => AgentTaskFailureClass::Permanent,
        },
        TraceDecayError::HostCliUnavailable { .. } => AgentTaskFailureClass::Unavailable,
        TraceDecayError::Io(error) => match error.kind() {
            std::io::ErrorKind::TimedOut => AgentTaskFailureClass::Timeout,
            std::io::ErrorKind::PermissionDenied => AgentTaskFailureClass::Denied,
            std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe => AgentTaskFailureClass::Disconnected,
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotConnected => {
                AgentTaskFailureClass::Unavailable
            }
            std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::ResourceBusy
            | std::io::ErrorKind::StorageFull => AgentTaskFailureClass::Retryable,
            _ => AgentTaskFailureClass::Permanent,
        },
        TraceDecayError::File { .. }
        | TraceDecayError::Database { .. }
        | TraceDecayError::Search { .. }
        | TraceDecayError::Config { .. }
        | TraceDecayError::InvalidRequest { .. }
        | TraceDecayError::ProfileResetRequired { .. }
        | TraceDecayError::ResetRequired { .. }
        | TraceDecayError::ToolRefused(_)
        | TraceDecayError::ServiceUnitNotOwned { .. }
        | TraceDecayError::Sqlite(_)
        | TraceDecayError::Json(_)
        | TraceDecayError::Automation(_) => AgentTaskFailureClass::Permanent,
    }
}

impl AgentTaskError {
    /// The typed state for a failure a backend observed as `class`.
    pub fn new(class: AgentTaskFailureClass, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        match class {
            AgentTaskFailureClass::Denied => Self::Denied { reason },
            AgentTaskFailureClass::Disconnected => Self::Disconnected { reason },
            AgentTaskFailureClass::Unavailable => Self::Unavailable { reason },
            AgentTaskFailureClass::Timeout => Self::Timeout { reason },
            AgentTaskFailureClass::MalformedOutput => Self::MalformedOutput { reason },
            AgentTaskFailureClass::InputTooLarge => Self::InputTooLarge { reason },
            AgentTaskFailureClass::Retryable => Self::Retryable { reason },
            AgentTaskFailureClass::Permanent => Self::Failed { reason },
        }
    }

    /// The retry/report classification of this typed state.
    pub fn failure_class(&self) -> AgentTaskFailureClass {
        match self {
            Self::Denied { .. } => AgentTaskFailureClass::Denied,
            Self::Disconnected { .. } => AgentTaskFailureClass::Disconnected,
            Self::Unavailable { .. } => AgentTaskFailureClass::Unavailable,
            Self::Timeout { .. } => AgentTaskFailureClass::Timeout,
            Self::MalformedOutput { .. } => AgentTaskFailureClass::MalformedOutput,
            Self::InputTooLarge { .. } => AgentTaskFailureClass::InputTooLarge,
            Self::Retryable { .. } => AgentTaskFailureClass::Retryable,
            Self::Failed { .. } => AgentTaskFailureClass::Permanent,
        }
    }
}

impl From<AgentTaskError> for AutomationError {
    fn from(error: AgentTaskError) -> Self {
        Self::config(error.to_string())
    }
}

impl From<AgentTaskError> for TraceDecayError {
    fn from(error: AgentTaskError) -> Self {
        AutomationError::from(error).into()
    }
}

pub trait AgentTaskBackend: Send + Sync {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> std::result::Result<AgentTaskResponse, AgentTaskError>;

    /// The host executable this backend spawns for a task, or `None` when it
    /// runs in-process or its executable is not configured. The durable
    /// backend identity stamps the opened file behind this path so replacing
    /// the binary in place re-admits a settled deterministic failure.
    fn executable(&self) -> Option<&std::path::Path>;
}

/// Availability state returned by runtime adapters. This crate does not probe
/// the ambient process environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBackendAvailability {
    pub backend: AutomationBackend,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub fn extract_json_object_prefix(text: &str) -> Result<Value> {
    let candidate = strip_optional_json_fence(text)?;
    parse_json_object_prefix(candidate)
}

#[tracing::instrument(name = "automation.backend.extract_json", level = "trace", skip_all)]
pub fn extract_response_json_object(text: &str, contract: &AgentTaskContract) -> Result<Value> {
    let mut schema_error = None;
    for (start, _) in text.char_indices().filter(|(_, ch)| *ch == '{') {
        if !is_json_object_candidate_boundary(&text[..start]) {
            continue;
        }
        let Ok(value) = parse_json_object_prefix(&text[start..]) else {
            continue;
        };
        if let Err(error) = validate_response_schema(&value, contract) {
            if schema_error.is_none() {
                schema_error = Some(error);
            }
            continue;
        }
        return Ok(value);
    }

    if let Some(error) = schema_error {
        return Err(error);
    }

    let value = extract_json_object_prefix(text)?;
    validate_response_schema(&value, contract)?;
    Ok(value)
}

fn is_json_object_candidate_boundary(prefix: &str) -> bool {
    prefix
        .chars()
        .rev()
        .find(|ch| !ch.is_whitespace())
        .is_none_or(|ch| matches!(ch, '}' | ']'))
}

fn parse_json_object_prefix(candidate: &str) -> Result<Value> {
    let mut stream = serde_json::Deserializer::from_str(candidate).into_iter::<Value>();
    let value = match stream.next() {
        Some(value) => value?,
        None => {
            return Err(config_error(
                "automation backend output must be a JSON object",
            ));
        }
    };
    if !value.is_object() {
        return Err(config_error(
            "automation backend output must be a JSON object",
        ));
    }
    Ok(value)
}

pub fn validate_response_schema(value: &Value, contract: &AgentTaskContract) -> Result<()> {
    let Some(required) = contract
        .response_schema
        .get("required")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    for property in required.iter().filter_map(Value::as_str) {
        let expected_type = contract
            .response_schema
            .pointer(&format!("/properties/{property}/type"))
            .and_then(Value::as_str);
        let property_value = value.get(property);
        let valid_type = match expected_type {
            Some("array") => property_value.is_some_and(Value::is_array),
            Some("string") => property_value.is_some_and(Value::is_string),
            Some("number") => property_value.is_some_and(Value::is_number),
            Some("integer") => property_value.is_some_and(Value::is_i64),
            Some("boolean") => property_value.is_some_and(Value::is_boolean),
            Some("object") => property_value.is_some_and(Value::is_object),
            _ => property_value.is_some(),
        };
        if !valid_type {
            let suffix = expected_type
                .map(|kind| format!(" {kind}"))
                .unwrap_or_default();
            return Err(config_error(format!(
                "automation backend output must include a {property}{suffix}"
            )));
        }
    }
    if contract
        .response_schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        == Some(false)
    {
        let allowed = contract
            .response_schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                AutomationError::config("strict automation response schema must define properties")
            })?;
        if value
            .as_object()
            .is_some_and(|object| object.keys().any(|key| !allowed.contains_key(key)))
        {
            return Err(config_error(
                "automation backend output contains an unknown property",
            ));
        }
    }
    Ok(())
}

fn strip_optional_json_fence(text: &str) -> Result<&str> {
    let trimmed = text.trim();
    let Some(after_opening) = trimmed.strip_prefix("```") else {
        return Ok(trimmed);
    };
    let Some(closing_start) = after_opening.rfind("```") else {
        return Err(config_error(
            "automation backend JSON fence is missing closing fence",
        ));
    };
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_failure_class_round_trips_through_its_typed_state() {
        let classes = [
            AgentTaskFailureClass::Retryable,
            AgentTaskFailureClass::Permanent,
            AgentTaskFailureClass::Timeout,
            AgentTaskFailureClass::Unavailable,
            AgentTaskFailureClass::Denied,
            AgentTaskFailureClass::Disconnected,
            AgentTaskFailureClass::MalformedOutput,
            AgentTaskFailureClass::InputTooLarge,
        ];
        let rendered = classes.map(|class| {
            let error = AgentTaskError::new(class, "observed failure");
            (error.failure_class(), error.to_string())
        });

        assert_eq!(
            rendered,
            [
                (
                    AgentTaskFailureClass::Retryable,
                    "agent task backend asked to retry: observed failure".to_owned(),
                ),
                (
                    AgentTaskFailureClass::Permanent,
                    "agent task failed: observed failure".to_owned(),
                ),
                (
                    AgentTaskFailureClass::Timeout,
                    "agent task timed out: observed failure".to_owned(),
                ),
                (
                    AgentTaskFailureClass::Unavailable,
                    "agent task backend unavailable: observed failure".to_owned(),
                ),
                (
                    AgentTaskFailureClass::Denied,
                    "agent task denied: observed failure".to_owned(),
                ),
                (
                    AgentTaskFailureClass::Disconnected,
                    "agent task backend disconnected: observed failure".to_owned(),
                ),
                (
                    AgentTaskFailureClass::MalformedOutput,
                    "agent task returned malformed output: observed failure".to_owned(),
                ),
                (
                    AgentTaskFailureClass::InputTooLarge,
                    "agent task input is too large: observed failure".to_owned(),
                ),
            ]
        );
    }

    #[test]
    fn runtime_failures_are_classified_by_their_typed_cause() {
        let failures = [
            (
                TraceDecayError::LockDeadline {
                    resource: "test lock",
                    deadline_ms: 1,
                },
                AgentTaskFailureClass::Retryable,
            ),
            (
                TraceDecayError::SyncLock {
                    message: "busy".to_owned(),
                },
                AgentTaskFailureClass::Retryable,
            ),
            (
                TraceDecayError::ProjectRoute {
                    reason_code: "temporarily_unavailable".to_owned(),
                    retryable: true,
                    detail: "route unavailable".to_owned(),
                    typed_detail: None,
                },
                AgentTaskFailureClass::Retryable,
            ),
            (
                TraceDecayError::ProjectRoute {
                    reason_code: "invalid_route".to_owned(),
                    retryable: false,
                    detail: "route invalid".to_owned(),
                    typed_detail: None,
                },
                AgentTaskFailureClass::Permanent,
            ),
            (
                TraceDecayError::HostCliUnavailable {
                    program: "codex".to_owned(),
                    lifecycle: "deploy".to_owned(),
                },
                AgentTaskFailureClass::Unavailable,
            ),
            (
                TraceDecayError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out",
                )),
                AgentTaskFailureClass::Timeout,
            ),
            (
                TraceDecayError::Io(std::io::Error::new(
                    std::io::ErrorKind::ResourceBusy,
                    "busy",
                )),
                AgentTaskFailureClass::Retryable,
            ),
            (
                TraceDecayError::Io(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "pipe closed",
                )),
                AgentTaskFailureClass::Disconnected,
            ),
            (
                TraceDecayError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "missing")),
                AgentTaskFailureClass::Permanent,
            ),
            (
                TraceDecayError::Config {
                    message: "invalid configuration".to_owned(),
                },
                AgentTaskFailureClass::Permanent,
            ),
            (
                TraceDecayError::project_open(
                    ProjectOpenFailureKind::CodeRuntimeBudgetExhausted { limit: 4 },
                    "every code-runtime seat is taken",
                ),
                AgentTaskFailureClass::Retryable,
            ),
            (
                TraceDecayError::project_open(
                    ProjectOpenFailureKind::AuthorityVerdict {
                        migration_pending: false,
                    },
                    "persisted rows rejected",
                ),
                AgentTaskFailureClass::Permanent,
            ),
            (
                TraceDecayError::not_found("symbol 'missing' not found"),
                AgentTaskFailureClass::Permanent,
            ),
        ];

        for (error, expected) in failures {
            assert_eq!(runtime_failure_class(&error), expected);
        }
    }

    #[test]
    fn failure_disposition_reads_the_recorded_class_not_its_message() {
        let oversized =
            agent_task_failure_disposition(Some(AgentTaskFailureClass::InputTooLarge), Some(false));
        let permanent =
            agent_task_failure_disposition(Some(AgentTaskFailureClass::Permanent), Some(false));
        let unclassified = agent_task_failure_disposition(None, Some(false));

        assert_eq!(oversized.retryable, Some(true));
        assert!(!oversized.is_non_retryable());
        assert_eq!(permanent.retryable, Some(false));
        assert!(permanent.is_non_retryable());
        assert_eq!(unclassified.classification, None);
        assert_eq!(unclassified.retryable, Some(false));
    }
}
