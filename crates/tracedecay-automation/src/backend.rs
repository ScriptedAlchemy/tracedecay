use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::AutomationBackend;
use crate::{AutomationError, Result, config_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskKind {
    MemoryCurator,
    SessionReflector,
    SkillWriter,
    CombinedReview,
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

    fn is_retryable_on_later_run(self) -> bool {
        self.is_retryable() || self == Self::MalformedOutput
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
        .map(AgentTaskFailureClass::is_retryable_on_later_run)
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

pub trait AgentTaskBackend: Send + Sync {
    fn run_task(&self, request: &AgentTaskRequest) -> Result<AgentTaskResponse>;
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
pub struct AgentTaskRetryAttempt {
    pub attempt: u32,
    pub succeeded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_classification: Option<AgentTaskFailureClass>,
    pub backoff_millis: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTaskRetryReport {
    attempts: Vec<AgentTaskRetryAttempt>,
}

impl AgentTaskRetryReport {
    pub fn attempt_count(&self) -> usize {
        self.attempts.len()
    }

    pub fn attempts(&self) -> &[AgentTaskRetryAttempt] {
        &self.attempts
    }
}

pub async fn run_agent_task_with_retry(
    backend: &dyn AgentTaskBackend,
    request: &AgentTaskRequest,
    policy: &BackendRetryPolicy,
) -> Result<AgentTaskResponse> {
    run_agent_task_with_retry_report(
        backend,
        request,
        policy,
        &mut AgentTaskRetryReport::default(),
    )
    .await
}

pub async fn run_agent_task_with_retry_report(
    backend: &dyn AgentTaskBackend,
    request: &AgentTaskRequest,
    policy: &BackendRetryPolicy,
    report: &mut AgentTaskRetryReport,
) -> Result<AgentTaskResponse> {
    report.attempts.clear();
    let start = Instant::now();
    let max_attempts = policy.max_attempts.max(1);
    let mut attempt: u32 = 1;
    loop {
        match backend.run_task(request) {
            Ok(response) => {
                report.attempts.push(AgentTaskRetryAttempt {
                    attempt,
                    succeeded: true,
                    failure_classification: None,
                    backoff_millis: 0,
                });
                return Ok(response);
            }
            Err(error) => {
                let classification = classify_agent_task_error_message(&error.to_string());
                let backoff = policy.backoff_before_attempt(attempt + 1);
                let should_retry = attempt < max_attempts
                    && classification.is_retryable()
                    && start.elapsed().saturating_add(backoff) < policy.budget;
                report.attempts.push(AgentTaskRetryAttempt {
                    attempt,
                    succeeded: false,
                    failure_classification: Some(classification),
                    backoff_millis: if should_retry {
                        u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX)
                    } else {
                        0
                    },
                });
                if !should_retry {
                    return Err(error);
                }
                if !backoff.is_zero() {
                    tokio::time::sleep(backoff).await;
                }
                attempt += 1;
            }
        }
    }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    struct FlakyBackend {
        failures: usize,
        calls: AtomicUsize,
    }

    impl AgentTaskBackend for FlakyBackend {
        fn run_task(&self, request: &AgentTaskRequest) -> Result<AgentTaskResponse> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.failures {
                return Err(AutomationError::config(
                    "timed out waiting for codex app-server response",
                ));
            }
            Ok(AgentTaskResponse {
                run_id: request.run_id.clone(),
                task: request.task,
                output_text: "recovered".to_string(),
                output_json: None,
                model: None,
                input_tokens: None,
                output_tokens: None,
            })
        }
    }

    fn request() -> AgentTaskRequest {
        AgentTaskRequest::new(
            "run_retry".to_string(),
            AgentTaskKind::MemoryCurator,
            r#"{"ops":[]}"#.to_string(),
            None,
            json!({}),
        )
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
                "json error: expected value at line 1 column 1",
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

    #[tokio::test]
    async fn retry_recovers_transient_backend_failure_on_second_attempt() {
        let backend = FlakyBackend {
            failures: 1,
            calls: AtomicUsize::new(0),
        };
        let policy = BackendRetryPolicy::new(
            3,
            vec![Duration::ZERO, Duration::ZERO],
            Duration::from_secs(120),
        );

        let response = run_agent_task_with_retry(&backend, &request(), &policy)
            .await
            .unwrap();

        assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
        assert_eq!(response.run_id, "run_retry");
    }

    #[tokio::test]
    async fn retry_report_records_transient_transient_success_attempts() {
        let backend = FlakyBackend {
            failures: 2,
            calls: AtomicUsize::new(0),
        };
        let policy = BackendRetryPolicy::new(
            3,
            vec![Duration::ZERO, Duration::ZERO],
            Duration::from_secs(120),
        );
        let mut report = AgentTaskRetryReport::default();

        run_agent_task_with_retry_report(&backend, &request(), &policy, &mut report)
            .await
            .unwrap();

        assert_eq!(report.attempt_count(), 3);
        assert_eq!(
            report
                .attempts()
                .iter()
                .map(|attempt| attempt.failure_classification)
                .collect::<Vec<_>>(),
            vec![
                Some(AgentTaskFailureClass::Timeout),
                Some(AgentTaskFailureClass::Timeout),
                None,
            ]
        );
        assert!(report.attempts()[2].succeeded);
    }

    #[tokio::test]
    async fn retry_stops_at_attempt_limit() {
        let backend = FlakyBackend {
            failures: usize::MAX,
            calls: AtomicUsize::new(0),
        };
        let policy = BackendRetryPolicy::new(3, vec![Duration::ZERO], Duration::from_secs(120));

        run_agent_task_with_retry(&backend, &request(), &policy)
            .await
            .unwrap_err();

        assert_eq!(backend.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_does_not_repeat_permanent_failure() {
        struct PermanentBackend {
            calls: AtomicUsize,
        }

        impl AgentTaskBackend for PermanentBackend {
            fn run_task(&self, _request: &AgentTaskRequest) -> Result<AgentTaskResponse> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Err(AutomationError::config("policy rejected the prompt"))
            }
        }

        let backend = PermanentBackend {
            calls: AtomicUsize::new(0),
        };
        let policy = BackendRetryPolicy::new(3, vec![Duration::ZERO], Duration::from_secs(120));

        run_agent_task_with_retry(&backend, &request(), &policy)
            .await
            .unwrap_err();

        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_respects_exhausted_budget() {
        let backend = FlakyBackend {
            failures: usize::MAX,
            calls: AtomicUsize::new(0),
        };
        let policy =
            BackendRetryPolicy::new(3, vec![Duration::from_secs(1)], Duration::from_millis(1));

        run_agent_task_with_retry(&backend, &request(), &policy)
            .await
            .unwrap_err();

        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failure_disposition_prefers_current_error_evidence() {
        let disposition = agent_task_failure_disposition(
            Some(AgentTaskFailureClass::Permanent),
            Some(false),
            Some("timed out waiting for backend"),
        );

        assert_eq!(
            disposition.classification,
            Some(AgentTaskFailureClass::Timeout)
        );
        assert_eq!(disposition.retryable, Some(true));
    }
}
