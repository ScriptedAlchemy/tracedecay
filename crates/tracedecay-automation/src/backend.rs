use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;

use crate::config::{AutomationBackend, AutomationConfig};
use crate::{AutomationError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskKind {
    MemoryCurator,
    SessionReflector,
    SkillWriter,
    /// One backend call covering both the session reflector and the skill
    /// writer when the scheduler finds both due in the same tick. The
    /// response must carry both a `facts` and a `skills` array; each array is
    /// validated and applied by the existing per-task pipelines.
    CombinedReview,
    /// User-defined scheduled job (Hermes cron parity). The backend response
    /// is plain content to deliver, not a structured proposal set.
    UserJob,
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
        .map_err(|err| {
            AutomationError::config(format!(
                "failed to encode automation backend request: {err}"
            ))
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
        .map(|message| {
            if is_oversized_backend_input(message) {
                AgentTaskFailureClass::Retryable
            } else {
                classify_agent_task_error_message(message)
            }
        })
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

fn is_oversized_backend_input(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("input_too_large")
        || normalized.contains("input exceeds the maximum length")
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
        AgentTaskKind::MemoryCurator => "memory_curator:v1",
        AgentTaskKind::SessionReflector => "session_reflector:v2",
        AgentTaskKind::SkillWriter => "skill_writer:v2",
        AgentTaskKind::CombinedReview => "combined_review:v1",
        AgentTaskKind::UserJob => "user_job:v1",
    }
}

fn response_schema(task: AgentTaskKind) -> Value {
    match task {
        AgentTaskKind::MemoryCurator => json_schema_for_array_properties(&["ops"]),
        AgentTaskKind::SessionReflector => json_schema_for_array_properties(&["facts"]),
        AgentTaskKind::SkillWriter => json_schema_for_array_properties(&["skills"]),
        AgentTaskKind::CombinedReview => json_schema_for_array_properties(&["facts", "skills"]),
        AgentTaskKind::UserJob => serde_json::json!({
            "type": "object",
            "additionalProperties": true
        }),
    }
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
    format!("sha256:{}", hex::encode(Sha256::digest(&bytes)))
}

pub const AGENT_TASK_MAX_ATTEMPTS: u32 = 3;
pub const AGENT_TASK_RETRY_BACKOFFS: [Duration; 2] =
    [Duration::from_secs(2), Duration::from_secs(5)];

#[derive(Debug, Clone)]
pub struct BackendRetryPolicy {
    max_attempts: u32,
    backoffs: Vec<Duration>,
    budget: Duration,
}

impl BackendRetryPolicy {
    #[must_use]
    pub fn from_timeout_secs(timeout_secs: u64) -> Self {
        Self {
            max_attempts: AGENT_TASK_MAX_ATTEMPTS,
            backoffs: AGENT_TASK_RETRY_BACKOFFS.to_vec(),
            budget: Duration::from_secs(timeout_secs.max(1)),
        }
    }

    #[must_use]
    pub fn new(max_attempts: u32, backoffs: Vec<Duration>, budget: Duration) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            backoffs,
            budget,
        }
    }

    pub fn retry_backoff_after_failure(
        &self,
        attempt: u32,
        elapsed: Duration,
        error: &str,
    ) -> Option<Duration> {
        if attempt >= self.max_attempts.max(1)
            || !classify_agent_task_error_message(error).is_retryable()
        {
            return None;
        }
        let backoff = self.backoff_before_attempt(attempt + 1);
        (elapsed.saturating_add(backoff) < self.budget).then_some(backoff)
    }

    fn backoff_before_attempt(&self, next_attempt: u32) -> Duration {
        let idx = (next_attempt.saturating_sub(2)) as usize;
        self.backoffs
            .get(idx)
            .or_else(|| self.backoffs.last())
            .copied()
            .unwrap_or_default()
    }
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

pub fn backend_availability(
    config: &AutomationConfig,
    codex_executable: &str,
    codex_executable_is_resolvable: bool,
) -> AgentBackendAvailability {
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
        AutomationBackend::CodexAppServer if codex_executable_is_resolvable => {
            AgentBackendAvailability {
                backend: AutomationBackend::CodexAppServer,
                available: true,
                executable: Some(codex_executable.to_string()),
                reason: None,
            }
        }
        AutomationBackend::CodexAppServer => AgentBackendAvailability {
            backend: AutomationBackend::CodexAppServer,
            available: false,
            executable: Some(codex_executable.to_string()),
            reason: Some(format!(
                "codex app-server backend executable '{codex_executable}' was not found"
            )),
        },
    }
}

pub fn extract_json_object_prefix(text: &str) -> Result<Value> {
    let candidate = strip_optional_json_fence(text)?;
    parse_json_object_prefix(candidate)
}

pub fn extract_response_json_object(text: &str, contract: &AgentTaskContract) -> Result<Value> {
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
        Some(value) => value.map_err(|err| AutomationError::config(err.to_string()))?,
        None => {
            return Err(AutomationError::config(
                "automation backend output must be a JSON object",
            ));
        }
    };
    if !value.is_object() {
        return Err(AutomationError::config(
            "automation backend output must be a JSON object",
        ));
    }
    Ok(value)
}

fn validate_response_schema(value: &Value, contract: &AgentTaskContract) -> Result<()> {
    let Some(required) = contract
        .response_schema
        .get("required")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    for property in required.iter().filter_map(Value::as_str) {
        if value.get(property).and_then(Value::as_array).is_none() {
            return Err(AutomationError::config(format!(
                "automation backend output must include a {property} array"
            )));
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
        return Err(AutomationError::config(
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
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{
        AgentTaskFailureClass, AgentTaskKind, AgentTaskRequest, BackendRetryPolicy,
        agent_task_failure_disposition, classify_agent_task_error_message,
        extract_json_object_prefix, extract_response_json_object,
    };

    #[test]
    fn combined_review_contract_requires_both_arrays_with_deterministic_input_hash() {
        let request = AgentTaskRequest::new(
            "run_combined".to_string(),
            AgentTaskKind::CombinedReview,
            "combined prompt".to_string(),
            Some("sha256:evidence".to_string()),
            json!({"apply": false}),
        );
        let same_inputs = AgentTaskRequest::new(
            "run_combined_other".to_string(),
            AgentTaskKind::CombinedReview,
            "combined prompt".to_string(),
            Some("sha256:evidence".to_string()),
            json!({"apply": false}),
        );

        assert_eq!(request.contract.task_key, "combined_review");
        assert_eq!(request.contract.prompt_version, "combined_review:v1");
        assert!(request.contract.strict_json);
        assert_eq!(
            request.contract.response_schema["required"],
            json!(["facts", "skills"])
        );
        assert_eq!(
            request.contract.response_schema["properties"]["facts"]["type"],
            "array"
        );
        assert_eq!(
            request.contract.response_schema["properties"]["skills"]["type"],
            "array"
        );
        assert!(request.input_hash.starts_with("sha256:"));
        assert_eq!(request.input_hash, same_inputs.input_hash);

        let different_evidence = AgentTaskRequest::new(
            "run_combined".to_string(),
            AgentTaskKind::CombinedReview,
            "combined prompt".to_string(),
            Some("sha256:other-evidence".to_string()),
            json!({"apply": false}),
        );
        assert_ne!(request.input_hash, different_evidence.input_hash);
    }

    #[test]
    fn extracts_one_plain_or_fenced_json_object() {
        assert_eq!(
            extract_json_object_prefix(r#" { "ok": true } "#).unwrap()["ok"],
            true
        );
        assert_eq!(
            extract_json_object_prefix("```json\n{\"task\":\"skill_writer\"}\n```").unwrap()["task"],
            "skill_writer"
        );
    }

    #[test]
    fn extracts_first_json_object_with_trailing_explanation() {
        assert_eq!(
            extract_json_object_prefix("{\"ops\": []}\n\nNo changes were needed.").unwrap()["ops"],
            json!([])
        );
        assert_eq!(
            extract_json_object_prefix("```json\n{\"facts\":[]}\n```\n\nSummary: no facts.")
                .unwrap()["facts"],
            json!([])
        );
        assert_eq!(
            extract_json_object_prefix("{\"skills\": []}\n{\"ignored\": true}").unwrap()["skills"],
            json!([])
        );
    }

    #[test]
    fn extracts_fenced_json_with_nested_markdown_fence_in_string() {
        let body = json!({
            "skills": [{
                "name": "shell-example",
                "body_markdown": "Run:\n```sh\ntracedecay status\n```"
            }]
        });
        let response = format!("```json\n{body}\n```\n\nCreated a skill.");

        let extracted = extract_json_object_prefix(&response).unwrap();

        assert_eq!(
            extracted["skills"][0]["body_markdown"],
            "Run:\n```sh\ntracedecay status\n```"
        );
    }

    #[test]
    fn rejects_non_object_and_prefix_text() {
        for text in [r#"[{"ok":true}]"#, r#"prefix {"ok":true}"#] {
            assert!(
                extract_json_object_prefix(text).is_err(),
                "accepted non-strict JSON output: {text}"
            );
        }
    }

    #[test]
    fn extracts_json_objects_and_validates_the_contract() {
        let request = AgentTaskRequest::new(
            "run".to_string(),
            AgentTaskKind::MemoryCurator,
            "prompt".to_string(),
            None,
            json!({}),
        );
        assert_eq!(
            extract_response_json_object("{\"ops\": []}\nsummary", &request.contract).unwrap()["ops"],
            json!([])
        );
        assert!(
            extract_response_json_object("{\"result\": {\"ops\": []}}", &request.contract).is_err()
        );
    }

    #[test]
    fn failure_disposition_heals_stale_retryability() {
        let disposition = agent_task_failure_disposition(
            Some(AgentTaskFailureClass::Permanent),
            Some(false),
            Some("config error: codex app-server closed stdout before completing"),
        );

        assert_eq!(
            disposition.classification,
            Some(AgentTaskFailureClass::Unavailable)
        );
        assert_eq!(disposition.retryable, Some(true));
        assert!(!disposition.is_non_retryable());
        assert_eq!(
            classify_agent_task_error_message("json error: expected value"),
            AgentTaskFailureClass::MalformedOutput
        );
    }

    #[test]
    fn classifies_backend_failures_for_retry_policy() {
        for (message, expected, retryable) in [
            (
                "timed out waiting for codex app-server response",
                AgentTaskFailureClass::Timeout,
                true,
            ),
            (
                "codex app-server backend executable 'codex' was not found",
                AgentTaskFailureClass::Unavailable,
                true,
            ),
            (
                "config error: codex app-server closed stdout before completing",
                AgentTaskFailureClass::Unavailable,
                true,
            ),
            (
                "json error: expected value at line 1 column 1",
                AgentTaskFailureClass::MalformedOutput,
                false,
            ),
            (
                "codex app-server returned an empty summary",
                AgentTaskFailureClass::MalformedOutput,
                false,
            ),
            (
                "temporarily unavailable, try again later",
                AgentTaskFailureClass::Retryable,
                true,
            ),
            (
                "model refused the request because policy rejected the prompt",
                AgentTaskFailureClass::Permanent,
                false,
            ),
        ] {
            let classification = classify_agent_task_error_message(message);
            assert_eq!(classification, expected, "message: {message}");
            assert_eq!(
                classification.is_retryable(),
                retryable,
                "message: {message}"
            );
        }
    }

    #[test]
    fn oversized_backend_input_is_retryable_after_request_bounding_changes() {
        let error = "codex app-server turn failed: input_too_large: Input exceeds the maximum length of 1048576 characters";
        let disposition = agent_task_failure_disposition(
            Some(AgentTaskFailureClass::Permanent),
            Some(false),
            Some(error),
        );

        assert_eq!(
            classify_agent_task_error_message(error),
            AgentTaskFailureClass::Permanent,
            "the same oversized request must not be retried immediately"
        );
        assert_eq!(
            disposition.classification,
            Some(AgentTaskFailureClass::Retryable)
        );
        assert_eq!(disposition.retryable, Some(true));
    }

    #[test]
    fn retry_policy_only_allows_transient_failures_within_budget() {
        let policy =
            BackendRetryPolicy::new(3, vec![Duration::from_secs(10)], Duration::from_secs(1));

        assert_eq!(
            policy.retry_backoff_after_failure(
                1,
                Duration::ZERO,
                "timed out waiting for codex app-server response",
            ),
            None
        );
        assert_eq!(
            BackendRetryPolicy::new(3, vec![Duration::ZERO], Duration::from_secs(1))
                .retry_backoff_after_failure(1, Duration::ZERO, "temporarily unavailable"),
            Some(Duration::ZERO)
        );
    }
}
