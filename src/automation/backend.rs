use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;

use crate::errors::{Result, TraceDecayError};
use crate::sessions::codex_app_server::{
    run_prompt_with_codex_app_server, CodexAppServerSummaryConfig,
};

use super::config::{AutomationBackend, AutomationConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskKind {
    MemoryCurator,
    SessionReflector,
    SkillWriter,
}

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
        .map_err(|err| TraceDecayError::Config {
            message: format!("failed to encode automation backend request: {err}"),
        })
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
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskFailureClass {
    Retryable,
    Permanent,
    Timeout,
    Unavailable,
    MalformedOutput,
}

impl AgentTaskFailureClass {
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Retryable | Self::Timeout | Self::Unavailable)
    }
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

pub fn agent_task_failure_disposition(
    recorded_classification: Option<AgentTaskFailureClass>,
    recorded_retryable: Option<bool>,
    error: Option<&str>,
) -> AgentTaskFailureDisposition {
    let classification = error
        .map(classify_agent_task_error_message)
        .or(recorded_classification);
    let retryable = classification
        .map(AgentTaskFailureClass::is_retryable)
        .or(recorded_retryable);

    AgentTaskFailureDisposition {
        classification,
        retryable,
    }
}

pub fn classify_agent_task_error_message(message: &str) -> AgentTaskFailureClass {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("timed out") || normalized.contains("timeout") {
        return AgentTaskFailureClass::Timeout;
    }
    if normalized.contains("not found")
        || normalized.contains("no such file")
        || normalized.contains("failed to spawn")
        || normalized.contains("failed to start")
        || normalized.contains("executable")
        || normalized.contains("connection refused")
        || normalized.contains("connection reset")
        || normalized.contains("broken pipe")
        || normalized.contains("closed stdout")
    {
        return AgentTaskFailureClass::Unavailable;
    }
    if normalized.contains("json error")
        || normalized.contains("expected value")
        || normalized.contains("expected ident")
        || normalized.contains("trailing characters")
        || normalized.contains("backend output")
        || normalized.contains("json fence")
        || normalized.contains("empty summary")
        || normalized.contains("empty output")
        || normalized.contains("output must include")
    {
        return AgentTaskFailureClass::MalformedOutput;
    }
    if normalized.contains("temporarily unavailable")
        || normalized.contains("rate limit")
        || normalized.contains("429")
        || normalized.contains("503")
        || normalized.contains("try again")
    {
        return AgentTaskFailureClass::Retryable;
    }
    AgentTaskFailureClass::Permanent
}

pub fn agent_task_contract(task: AgentTaskKind) -> AgentTaskContract {
    AgentTaskContract {
        task_key: task_key(task).to_string(),
        prompt_version: prompt_version(task).to_string(),
        response_schema: response_schema(task),
        strict_json: true,
    }
}

pub fn task_key(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator",
        AgentTaskKind::SessionReflector => "session_reflector",
        AgentTaskKind::SkillWriter => "skill_writer",
    }
}

pub fn prompt_version(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator:v1",
        AgentTaskKind::SessionReflector => "session_reflector:v1",
        AgentTaskKind::SkillWriter => "skill_writer:v1",
    }
}

fn response_schema(task: AgentTaskKind) -> Value {
    match task {
        AgentTaskKind::MemoryCurator => json_schema_for_array_property("ops"),
        AgentTaskKind::SessionReflector => json_schema_for_array_property("facts"),
        AgentTaskKind::SkillWriter => json_schema_for_array_property("skills"),
    }
}

fn json_schema_for_array_property(property: &str) -> Value {
    serde_json::json!({
        "type": "object",
        "required": [property],
        "properties": {
            property: { "type": "array" }
        },
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
    format!("sha256:{}", hex::encode(Sha256::digest(&bytes)))
}

pub trait AgentTaskBackend: Send + Sync {
    fn run_task(&self, request: &AgentTaskRequest) -> Result<AgentTaskResponse>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBackendAvailability {
    pub backend: AutomationBackend,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub fn backend_availability(config: &AutomationConfig) -> AgentBackendAvailability {
    match config.backend {
        AutomationBackend::Disabled => AgentBackendAvailability {
            backend: AutomationBackend::Disabled,
            available: false,
            executable: None,
            reason: Some("automation backend is disabled".to_string()),
        },
        AutomationBackend::ExternalCommand => AgentBackendAvailability {
            backend: AutomationBackend::ExternalCommand,
            available: false,
            executable: None,
            reason: Some("external_command backend is not implemented".to_string()),
        },
        AutomationBackend::CodexAppServer => {
            let summary_config = CodexAppServerSummaryConfig::from_env();
            let executable = summary_config.codex_bin.clone();
            if executable_is_resolvable(&executable) {
                AgentBackendAvailability {
                    backend: AutomationBackend::CodexAppServer,
                    available: true,
                    executable: Some(executable),
                    reason: None,
                }
            } else {
                AgentBackendAvailability {
                    backend: AutomationBackend::CodexAppServer,
                    available: false,
                    executable: Some(executable.clone()),
                    reason: Some(format!(
                        "codex app-server backend executable '{executable}' was not found"
                    )),
                }
            }
        }
    }
}

fn executable_is_resolvable(bin: &str) -> bool {
    let path = Path::new(bin);
    if path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
}

#[derive(Debug, Clone)]
pub struct CodexAppServerBackend {
    config: CodexAppServerSummaryConfig,
}

impl CodexAppServerBackend {
    pub fn new(model: Option<String>, timeout_secs: u64) -> Self {
        Self::new_with_runtime_options(model, timeout_secs, None, None)
    }

    pub fn from_automation_config(config: &AutomationConfig) -> Self {
        Self::new_with_runtime_options(
            config.model.clone(),
            config.timeout_secs,
            config.max_tokens,
            config.temperature,
        )
    }

    pub fn new_with_runtime_options(
        model: Option<String>,
        timeout_secs: u64,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Self {
        let mut config = CodexAppServerSummaryConfig::from_env();
        if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
            config.model = Some(model);
        }
        config.timeout = Duration::from_secs(timeout_secs.clamp(5, 300));
        if let Some(max_tokens) = max_tokens {
            config.max_tokens = Some(max_tokens);
        }
        if let Some(temperature) = temperature {
            config.temperature = Some(temperature);
        }
        Self { config }
    }

    pub fn from_config(config: CodexAppServerSummaryConfig) -> Self {
        Self { config }
    }
}

impl AgentTaskBackend for CodexAppServerBackend {
    fn run_task(&self, request: &AgentTaskRequest) -> Result<AgentTaskResponse> {
        let backend_message = request.backend_message()?;
        let summary = run_prompt_with_codex_app_server(
            &backend_message,
            &self.config,
            "tracedecay_automation",
        )?;
        let output_json = request
            .contract
            .strict_json
            .then(|| extract_response_json_object(&summary.text, &request.contract))
            .transpose()?;
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_json,
            output_text: summary.text,
            model: summary.model.or_else(|| self.config.model.clone()),
            input_tokens: None,
            output_tokens: None,
        })
    }
}

pub fn extract_json_object_prefix(text: &str) -> Result<Value> {
    let candidate = strip_optional_json_fence(text)?;
    parse_json_object_prefix(candidate)
}

fn extract_response_json_object(text: &str, contract: &AgentTaskContract) -> Result<Value> {
    let mut schema_error = None;
    for (start, _) in text.char_indices().filter(|(_, ch)| *ch == '{') {
        if !is_json_object_candidate_boundary(&text[..start]) {
            continue;
        }
        let Ok(value) = parse_json_object_prefix(&text[start..]) else {
            continue;
        };
        if let Err(err) = validate_response_schema(&value, contract) {
            if schema_error.is_none() {
                schema_error = Some(err);
            }
            continue;
        }

        return Ok(value);
    }

    if let Some(err) = schema_error {
        return Err(err);
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
        None => return config_error("automation backend output must be a JSON object"),
    };
    if !value.is_object() {
        return config_error("automation backend output must be a JSON object");
    }
    Ok(value)
}

fn validate_response_schema(value: &Value, contract: &AgentTaskContract) -> Result<()> {
    let Some(required) = contract
        .response_schema
        .get("required")
        .and_then(Value::as_array)
        .and_then(|required| required.first())
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    if value.get(required).and_then(Value::as_array).is_none() {
        return config_error(format!(
            "automation backend output must include a {required} array"
        ));
    }
    Ok(())
}

fn strip_optional_json_fence(text: &str) -> Result<&str> {
    let trimmed = text.trim();
    let Some(after_opening) = trimmed.strip_prefix("```") else {
        return Ok(trimmed);
    };
    let Some(closing_start) = after_opening.rfind("```") else {
        return config_error("automation backend JSON fence is missing closing fence");
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

fn config_error<T>(message: impl Into<String>) -> Result<T> {
    Err(TraceDecayError::Config {
        message: message.into(),
    })
}
