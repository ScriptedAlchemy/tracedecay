use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(target_os = "linux")]
use std::thread;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;

use tracedecay::automation::backend::{
    AgentTaskBackend, AgentTaskFailureClass, AgentTaskKind, AgentTaskRequest, AgentTaskResponse,
    BackendRetryPolicy, CodexAppServerBackend, agent_task_failure_disposition,
    backend_availability, classify_agent_task_error_message, extract_json_object_prefix,
    run_agent_task_with_retry,
};
use tracedecay::automation::config::{AutomationBackend, AutomationConfig};
use tracedecay::sessions::codex_app_server::{
    CodexAppServerSummaryConfig, run_prompt_with_codex_app_server,
};
use tracedecay_agent_hosts::ports::codex_app_server::SummaryConfig as AutomationSummaryConfig;

use crate::common::{
    EnvVarGuard, fake_codex_bin, install_fake_codex_launcher, windows_python_launcher,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Success-path budget for the fake codex app-server child to spawn (a real
/// python interpreter) and complete its scripted turn. This is the upper bound
/// the backend waits before declaring a timeout; it must be generous enough
/// that a slow python spawn/schedule under nextest's process-per-test
/// parallelism can never false-fire it, while still failing fast on a genuine
/// hang. Tests that deliberately exercise the timeout path pass their own tight
/// `Duration` (e.g. 300ms) and are unaffected by this value.
fn fake_codex_response_timeout() -> Duration {
    Duration::from_secs(30)
}

fn fake_codex_response_timeout_secs() -> u64 {
    fake_codex_response_timeout().as_secs()
}

struct EchoBackend;

impl AgentTaskBackend for EchoBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: request.prompt.clone(),
            output_json: extract_json_object_prefix(&request.prompt).ok(),
            model: Some("test-model".to_string()),
            input_tokens: Some(12),
            output_tokens: Some(34),
        })
    }
}

#[test]
fn backend_contract_round_trips_structured_task_output() {
    let request = AgentTaskRequest::new(
        "run_001".to_string(),
        AgentTaskKind::MemoryCurator,
        r#"{"ops":[{"kind":"keep","id":"fact-1"}]}"#.to_string(),
        Some("sha256:evidence".to_string()),
        json!({"bank":"core"}),
    );

    let response = EchoBackend.run_task(&request).unwrap();

    assert_eq!(response.run_id, "run_001");
    assert_eq!(response.task, AgentTaskKind::MemoryCurator);
    assert_eq!(response.model.as_deref(), Some("test-model"));
    assert_eq!(request.evidence_hash.as_deref(), Some("sha256:evidence"));
    assert_eq!(request.contract.task_key, "memory_curator");
    assert_eq!(request.contract.prompt_version, "memory_curator:v1");
    assert_eq!(request.contract.response_schema["required"][0], "ops");
    assert!(request.contract.strict_json);
    assert!(request.input_hash.starts_with("sha256:"));
    assert_ne!(request.input_hash, "sha256:evidence");
    assert_eq!(response.output_json.unwrap()["ops"][0]["id"], "fact-1");
    assert_eq!(response.input_tokens, Some(12));
    assert_eq!(response.output_tokens, Some(34));
}

#[test]
fn combined_review_contract_requires_both_arrays_with_deterministic_input_hash() {
    let request = AgentTaskRequest::new(
        "run_combined".to_string(),
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

    // Same inputs hash identically; run_id is not part of the input hash.
    let same_inputs = AgentTaskRequest::new(
        "run_combined_other".to_string(),
        AgentTaskKind::CombinedReview,
        "combined prompt".to_string(),
        Some("sha256:evidence".to_string()),
        json!({"apply": false}),
    );
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
        extract_json_object_prefix("```json\n{\"facts\":[]}\n```\n\nSummary: no facts.").unwrap()["facts"],
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
fn failure_disposition_heals_stale_recorded_retryability() {
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
}

#[test]
fn malformed_output_is_retryable_on_a_later_scheduled_run() {
    let disposition = agent_task_failure_disposition(
        Some(AgentTaskFailureClass::MalformedOutput),
        Some(false),
        Some("config error: automation backend output must include a ops array"),
    );

    assert_eq!(
        disposition.classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(disposition.retryable, Some(true));
    assert!(!disposition.is_non_retryable());
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
fn fake_codex_app_server_returns_summary_and_logs_protocol() {
    let fake = FakeCodexAppServer::new();
    let config = CodexAppServerSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout: fake_codex_response_timeout(),
    };

    let summary =
        run_prompt_with_codex_app_server("summarize this", &config, "test_source").unwrap();

    assert_eq!(summary.text, "summary text");
    assert_eq!(summary.model.as_deref(), Some("actual-model"));

    let messages = fake.logged_messages();
    assert_eq!(messages[0]["method"], "initialize");
    assert_eq!(messages[1]["method"], "initialized");
    assert_eq!(messages[2]["method"], "thread/start");
    assert_eq!(messages[2]["params"]["ephemeral"], true);
    assert_eq!(messages[2]["params"]["threadSource"], "test_source");
    assert_eq!(messages[2]["params"]["model"], "configured-model");
    assert_eq!(messages[3]["method"], "turn/start");
    assert_eq!(messages[3]["params"]["threadId"], "thread-1");
    assert_eq!(messages[3]["params"]["model"], "configured-model");
    assert!(messages[3]["params"].get("maxOutputTokens").is_none());
    assert!(messages[3]["params"].get("temperature").is_none());
    assert_eq!(messages[3]["params"]["effort"], "low");
    assert_eq!(messages[3]["params"]["summary"], "concise");
    assert_eq!(
        messages[3]["params"]["input"][0]["text"],
        json!("summarize this")
    );
    assert_process_gone(fake.child_pid());
}

#[test]
fn codex_app_server_backend_run_task_uses_injected_config() {
    let fake = FakeCodexAppServer::new_with_behavior("json");
    let backend = CodexAppServerBackend::from_config(AutomationSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout: fake_codex_response_timeout(),
    });
    let request = AgentTaskRequest::new(
        "run_app_server".to_string(),
        AgentTaskKind::SkillWriter,
        r#"{"skills":[]}"#.to_string(),
        Some("sha256:evidence".to_string()),
        json!({"kind":"test"}),
    );

    let response = backend.run_task(&request).unwrap();

    assert_eq!(response.run_id, "run_app_server");
    assert_eq!(response.task, AgentTaskKind::SkillWriter);
    assert_eq!(response.output_text, r#"{"skills": []}"#);
    assert_eq!(response.output_json.unwrap()["skills"], json!([]));
    assert_eq!(response.model.as_deref(), Some("actual-model"));
    assert_eq!(response.input_tokens, None);
    assert_eq!(response.output_tokens, None);
    let messages = fake.logged_messages();
    assert_eq!(
        messages[2]["params"]["threadSource"],
        "tracedecay_automation"
    );
    let backend_request: serde_json::Value =
        serde_json::from_str(messages[3]["params"]["input"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(backend_request["run_id"], "run_app_server");
    assert_eq!(backend_request["task"], "skill_writer");
    assert_eq!(backend_request["contract"]["task_key"], "skill_writer");
    assert_eq!(
        backend_request["contract"]["prompt_version"],
        "skill_writer:v2"
    );
    assert_eq!(backend_request["evidence_hash"], "sha256:evidence");
    assert_eq!(backend_request["prompt"], r#"{"skills":[]}"#);
    assert_eq!(backend_request["context"], json!({"kind":"test"}));
}

#[test]
fn codex_app_server_backend_uses_first_schema_matching_json_object() {
    let fake = FakeCodexAppServer::new_with_behavior("json_after_echo");
    let backend = CodexAppServerBackend::from_config(AutomationSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout: fake_codex_response_timeout(),
    });
    let request = AgentTaskRequest::new(
        "run_app_server_echo".to_string(),
        AgentTaskKind::MemoryCurator,
        r#"{"ops":[]}"#.to_string(),
        None,
        json!({}),
    );

    let response = backend.run_task(&request).unwrap();

    assert_eq!(response.output_json.unwrap()["ops"], json!([]));
    assert_process_gone(fake.child_pid());
}

#[test]
fn codex_app_server_backend_rejects_nested_schema_matching_json_object() {
    let (err, pid) =
        backend_error_for_behavior("json_wrapped_response", fake_codex_response_timeout());

    assert!(
        err.contains("automation backend output must include a ops array"),
        "unexpected error: {err}"
    );
    assert_process_gone(pid);
}

#[test]
fn codex_app_server_backend_falls_back_to_configured_model_when_server_omits_model() {
    let fake = FakeCodexAppServer::new_with_behavior("no_model");
    let backend = CodexAppServerBackend::from_config(AutomationSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout: fake_codex_response_timeout(),
    });
    let request = AgentTaskRequest::new(
        "run_app_server".to_string(),
        AgentTaskKind::SessionReflector,
        r#"{"facts":[]}"#.to_string(),
        None,
        json!({}),
    );

    let response = backend.run_task(&request).unwrap();

    assert_eq!(response.output_text, r#"{"facts": []}"#);
    assert_eq!(response.output_json.unwrap()["facts"], json!([]));
    assert_eq!(response.model.as_deref(), Some("configured-model"));
    assert_process_gone(fake.child_pid());
}

#[test]
fn codex_app_server_backend_from_automation_config_lets_app_server_choose_model() {
    let fake = FakeCodexAppServer::new_with_behavior("json");
    // Env vars are only read while the backend is constructed, so hold the
    // env lock just for that window instead of across the subprocess run.
    let backend = {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let _codex_bin = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &fake.bin);
        CodexAppServerBackend::from_automation_config(&AutomationConfig {
            backend: AutomationBackend::CodexAppServer,
            timeout_secs: fake_codex_response_timeout_secs(),
            ..AutomationConfig::default()
        })
    };
    let request = AgentTaskRequest::new(
        "run_runtime_options".to_string(),
        AgentTaskKind::SessionReflector,
        r#"{"facts":[]}"#.to_string(),
        None,
        json!({}),
    );

    let response = backend.run_task(&request).unwrap();

    assert_eq!(response.run_id, "run_runtime_options");
    assert_eq!(response.output_json.unwrap()["facts"], json!([]));
    let messages = fake.logged_messages();
    assert!(messages[2]["params"].get("model").is_none());
    assert!(messages[3]["params"].get("model").is_none());
    assert!(messages[3]["params"].get("maxOutputTokens").is_none());
    assert!(messages[3]["params"].get("temperature").is_none());
    assert_process_gone(fake.child_pid());
}

#[test]
fn codex_app_server_backend_ignores_env_generation_options() {
    let fake = FakeCodexAppServer::new_with_behavior("json");
    // Env vars are only read while the backend is constructed, so hold the
    // env lock just for that window instead of across the subprocess run.
    let backend = {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let _codex_bin = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &fake.bin);
        let _max_tokens = EnvVarGuard::set("TRACEDECAY_CODEX_SUMMARY_MAX_TOKENS", "2048");
        let _temperature = EnvVarGuard::set("TRACEDECAY_CODEX_SUMMARY_TEMPERATURE", "0.25");
        CodexAppServerBackend::from_automation_config(&AutomationConfig {
            backend: AutomationBackend::CodexAppServer,
            timeout_secs: fake_codex_response_timeout_secs(),
            ..AutomationConfig::default()
        })
    };
    let request = AgentTaskRequest::new(
        "run_env_runtime_options".to_string(),
        AgentTaskKind::SkillWriter,
        r#"{"skills":[]}"#.to_string(),
        None,
        json!({}),
    );

    let response = backend.run_task(&request).unwrap();

    assert_eq!(response.run_id, "run_env_runtime_options");
    assert_eq!(response.output_json.unwrap()["skills"], json!([]));
    let messages = fake.logged_messages();
    assert!(messages[3]["params"].get("maxOutputTokens").is_none());
    assert!(messages[3]["params"].get("temperature").is_none());
    assert_process_gone(fake.child_pid());
}

#[test]
fn codex_app_server_backend_propagates_timeout_errors_and_reaps_child() {
    // Short but not tight: the fake must have time to start and write its pid
    // file on Linux before the client gives up and reaps it.
    let (err, pid) = backend_error_for_behavior("timeout", Duration::from_millis(300));

    assert!(
        err.contains("timed out waiting for codex app-server"),
        "unexpected error: {err}"
    );
    assert_eq!(
        classify_agent_task_error_message(&err),
        AgentTaskFailureClass::Timeout
    );
    assert_process_gone(pid);
}

#[test]
fn codex_app_server_backend_propagates_malformed_json_errors_and_reaps_child() {
    let (err, pid) = backend_error_for_behavior("malformed", fake_codex_response_timeout());

    assert!(
        err.contains("expected ident") || err.contains("expected value"),
        "unexpected error: {err}"
    );
    assert_eq!(
        classify_agent_task_error_message(&err),
        AgentTaskFailureClass::MalformedOutput
    );
    assert_process_gone(pid);
}

#[test]
fn codex_app_server_backend_propagates_empty_output_errors_and_reaps_child() {
    let (err, pid) = backend_error_for_behavior("empty", fake_codex_response_timeout());

    assert!(
        err.contains("codex app-server returned an empty summary"),
        "unexpected error: {err}"
    );
    assert_eq!(
        classify_agent_task_error_message(&err),
        AgentTaskFailureClass::MalformedOutput
    );
    assert_process_gone(pid);
}

#[test]
fn backend_availability_reports_configured_codex_executable_status() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let fake = FakeCodexAppServer::new();
    let _codex_bin = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &fake.bin);
    let available = backend_availability(&AutomationConfig {
        backend: AutomationBackend::CodexAppServer,
        ..AutomationConfig::default()
    });

    assert!(available.available);
    assert_eq!(
        available.executable.as_deref(),
        Some(fake.bin.to_string_lossy().as_ref())
    );

    let missing = fake.bin.with_file_name("missing-codex");
    let _codex_bin = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &missing);
    let unavailable = backend_availability(&AutomationConfig {
        backend: AutomationBackend::CodexAppServer,
        ..AutomationConfig::default()
    });

    assert!(!unavailable.available);
    assert!(
        unavailable
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("was not found"))
    );
}

#[test]
fn fake_codex_app_server_uses_thread_model_when_turn_omits_model() {
    let fake = FakeCodexAppServer::new_with_behavior("thread_model_only");
    let config = CodexAppServerSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout: fake_codex_response_timeout(),
    };

    let summary =
        run_prompt_with_codex_app_server("summarize this", &config, "test_source").unwrap();

    assert_eq!(summary.text, "summary from completed item");
    assert_eq!(summary.model.as_deref(), Some("thread-model"));
    assert_process_gone(fake.child_pid());
}

#[test]
fn fake_codex_app_server_rejects_empty_turn_output() {
    let fake = FakeCodexAppServer::new_with_behavior("empty");
    let config = CodexAppServerSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: None,
        timeout: fake_codex_response_timeout(),
    };

    let err = run_prompt_with_codex_app_server("summarize this", &config, "test_source")
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("codex app-server returned an empty summary"),
        "unexpected error: {err}"
    );
    assert_process_gone(fake.child_pid());
}

#[test]
fn fake_codex_app_server_times_out_and_reaps_child() {
    let fake = FakeCodexAppServer::new_with_behavior("timeout");
    let config = CodexAppServerSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: None,
        timeout: Duration::from_millis(300),
    };

    let err = run_prompt_with_codex_app_server("summarize this", &config, "test_source")
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("timed out waiting for codex app-server"),
        "unexpected error: {err}"
    );
    assert_process_gone(fake.child_pid());
}

#[test]
fn fake_codex_app_server_rejects_malformed_json_and_reaps_child() {
    let fake = FakeCodexAppServer::new_with_behavior("malformed");
    let config = CodexAppServerSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: None,
        timeout: fake_codex_response_timeout(),
    };

    let err = run_prompt_with_codex_app_server("summarize this", &config, "test_source")
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("expected ident") || err.contains("expected value"),
        "unexpected error: {err}"
    );
    assert_process_gone(fake.child_pid());
}

struct FakeCodexAppServer {
    _temp: TempDir,
    bin: PathBuf,
    log: PathBuf,
    #[cfg(target_os = "linux")]
    pid: PathBuf,
}

fn backend_error_for_behavior(behavior: &str, timeout: Duration) -> (String, u32) {
    let fake = FakeCodexAppServer::new_with_behavior(behavior);
    let backend = CodexAppServerBackend::from_config(AutomationSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout,
    });
    let request = AgentTaskRequest::new(
        format!("run_{behavior}"),
        AgentTaskKind::MemoryCurator,
        "backend prompt".to_string(),
        None,
        json!({}),
    );
    let err = backend.run_task(&request).unwrap_err().to_string();
    let pid = fake.child_pid();
    (err, pid)
}

impl FakeCodexAppServer {
    fn new() -> Self {
        Self::new_with_behavior("success")
    }

    fn new_with_behavior(behavior: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let bin = fake_codex_bin(temp.path());
        let script_path = temp.path().join("codex.py");
        let log = temp.path().join("stdin.jsonl");
        let pid = temp.path().join("child.pid");
        let script = fake_codex_script(&log, &pid, behavior);
        fs::write(&script_path, script).unwrap();
        install_fake_codex_launcher(&script_path, &bin);
        Self {
            _temp: temp,
            bin,
            log,
            #[cfg(target_os = "linux")]
            pid,
        }
    }

    fn logged_messages(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[cfg(target_os = "linux")]
    fn child_pid(&self) -> u32 {
        for _ in 0..100 {
            if let Ok(raw) = fs::read_to_string(&self.pid) {
                return raw.trim().parse().unwrap();
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("fake codex app-server did not write pid file");
    }

    #[cfg(not(target_os = "linux"))]
    fn child_pid(&self) -> u32 {
        0
    }
}

fn fake_codex_script(log: &Path, pid: &Path, behavior: &str) -> String {
    format!(
        r#"#!/usr/bin/env python3
import json
import os
import sys
import time

log_path = r'''{}'''
pid_path = r'''{}'''
behavior = r'''{}'''

if len(sys.argv) != 2 or sys.argv[1] != "app-server":
    sys.exit(42)
if os.environ.get("TRACEDECAY_CODEX_SUMMARY_CHILD") != "1":
    sys.exit(43)

with open(pid_path, "w", encoding="utf-8") as pid_file:
    pid_file.write(str(os.getpid()))
    pid_file.flush()

if behavior == "malformed":
    print("not json", flush=True)
    time.sleep(10)

with open(log_path, "a", encoding="utf-8") as log:
    for line in sys.stdin:
        log.write(line)
        log.flush()
        msg = json.loads(line)
        method = msg.get("method")
        if method == "initialize":
            if behavior == "timeout":
                time.sleep(10)
            print(json.dumps(dict(id=msg.get("id"), result=dict())), flush=True)
        elif method == "thread/start":
            if behavior == "no_model":
                thread = dict(id="thread-1")
            else:
                thread = dict(id="thread-1", model="thread-model")
            print(json.dumps(dict(id=msg.get("id"), result=dict(thread=thread))), flush=True)
        elif method == "turn/start":
            if behavior == "empty":
                print(json.dumps(dict(method="turn/completed")), flush=True)
            elif behavior == "thread_model_only":
                item = dict(content=[dict(text="summary from completed item")])
                print(json.dumps(dict(method="item/completed", params=dict(item=item))), flush=True)
                print(json.dumps(dict(method="turn/completed")), flush=True)
            elif behavior == "no_model":
                print(json.dumps(dict(method="item/agentMessage/delta", params=dict(delta=json.dumps(dict(facts=[]))))), flush=True)
                print(json.dumps(dict(method="turn/completed")), flush=True)
            elif behavior == "json":
                requested = msg.get("params", dict()).get("input", [dict()])[0].get("text", "")
                if "skills" in requested:
                    payload = json.dumps(dict(skills=[]))
                elif "facts" in requested:
                    payload = json.dumps(dict(facts=[]))
                else:
                    payload = json.dumps(dict(ops=[]))
                print(json.dumps(dict(method="item/agentMessage/delta", params=dict(delta=payload, model="actual-model"))), flush=True)
                print(json.dumps(dict(method="turn/completed")), flush=True)
            elif behavior == "json_after_echo":
                payload = json.dumps(dict(run_id="echo", task="memory_curator")) + "\n" + json.dumps(dict(ops=[]))
                print(json.dumps(dict(method="item/agentMessage/delta", params=dict(delta=payload, model="actual-model"))), flush=True)
                print(json.dumps(dict(method="turn/completed")), flush=True)
            elif behavior == "json_wrapped_response":
                payload = json.dumps(dict(result=dict(ops=[])))
                print(json.dumps(dict(method="item/agentMessage/delta", params=dict(delta=payload, model="actual-model"))), flush=True)
                print(json.dumps(dict(method="turn/completed")), flush=True)
            else:
                print(json.dumps(dict(method="item/agentMessage/delta", params=dict(delta="<thinking>hide</thinking>summary ", model="actual-model"))), flush=True)
                print(json.dumps(dict(method="item/agentMessage/delta", params=dict(delta="text"))), flush=True)
                print(json.dumps(dict(method="turn/completed", params=dict(model="actual-model"))), flush=True)
            break
"#,
        log.display(),
        pid.display(),
        behavior,
    )
}

#[cfg(target_os = "linux")]
fn assert_process_gone(pid: u32) {
    let proc_path = PathBuf::from(format!("/proc/{pid}"));
    for _ in 0..50 {
        if !proc_path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("fake codex app-server process {pid} was not reaped");
}

#[cfg(not(target_os = "linux"))]
fn assert_process_gone(_pid: u32) {}

#[test]
fn windows_python_launcher_prefers_setup_python_and_preserves_exit_status() {
    let launcher = windows_python_launcher("codex.py");

    assert!(launcher.contains("%Python_ROOT_DIR%\\python.exe"));
    assert!(launcher.contains("%pythonLocation%\\python.exe"));
    assert!(launcher.contains("exit /b %ERRORLEVEL%"));
    assert!(!launcher.contains("if not errorlevel 1 exit /b 0"));
}

/// Backend fake whose first `fail_until` invocations fail with a fixed error
/// message, then every later invocation succeeds. Counts total invocations so
/// tests can assert exactly how many attempts the retry helper made.
struct FlakyBackend {
    calls: AtomicUsize,
    fail_until: usize,
    fail_message: &'static str,
}

impl FlakyBackend {
    fn new(fail_until: usize, fail_message: &'static str) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fail_until,
            fail_message,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AgentTaskBackend for FlakyBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay_automation::Result<AgentTaskResponse> {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.fail_until {
            return Err(tracedecay_automation::AutomationError::config(
                self.fail_message,
            ));
        }
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_text: "recovered".to_string(),
            output_json: None,
            model: Some("test-model".to_string()),
            input_tokens: None,
            output_tokens: None,
        })
    }
}

fn retry_test_request() -> AgentTaskRequest {
    AgentTaskRequest::new(
        "run_retry".to_string(),
        AgentTaskKind::MemoryCurator,
        r#"{"ops":[]}"#.to_string(),
        None,
        json!({}),
    )
}

/// Zero-backoff policy with a generous budget so retry logic can be exercised
/// deterministically without real sleeps.
fn instant_retry_policy(max_attempts: u32) -> BackendRetryPolicy {
    BackendRetryPolicy::new(
        max_attempts,
        vec![Duration::ZERO, Duration::ZERO],
        Duration::from_secs(120),
    )
}

#[tokio::test]
async fn retry_recovers_transient_backend_failure_on_second_attempt() {
    let backend = FlakyBackend::new(1, "timed out waiting for codex app-server response");
    let request = retry_test_request();

    let response = run_agent_task_with_retry(&backend, &request, &instant_retry_policy(3))
        .await
        .expect("transient failure should be retried into a success");

    assert_eq!(backend.calls(), 2, "should succeed on the second attempt");
    assert_eq!(response.run_id, "run_retry");
    assert_eq!(response.output_text, "recovered");
}

#[tokio::test]
async fn retry_stops_after_exhausting_bounded_attempts() {
    // Always-transient failure with a generous budget: the helper should make
    // the first attempt plus two retries (3 total) then propagate the error.
    let backend = FlakyBackend::new(usize::MAX, "closed stdout before completing");
    let request = retry_test_request();

    let err = run_agent_task_with_retry(&backend, &request, &instant_retry_policy(3))
        .await
        .expect_err("exhausted retries should propagate the final error");

    assert_eq!(backend.calls(), 3, "first attempt plus two bounded retries");
    assert!(
        err.to_string().contains("closed stdout before completing"),
        "final error should propagate unchanged: {err}"
    );
}

#[tokio::test]
async fn retry_does_not_retry_non_transient_backend_failure() {
    let backend = FlakyBackend::new(
        usize::MAX,
        "model refused the request because policy rejected the prompt",
    );
    let request = retry_test_request();

    let err = run_agent_task_with_retry(&backend, &request, &instant_retry_policy(3))
        .await
        .expect_err("permanent failure should not be retried");

    assert_eq!(
        classify_agent_task_error_message(&err.to_string()),
        AgentTaskFailureClass::Permanent
    );
    assert_eq!(backend.calls(), 1, "non-transient failure must fail fast");
}

#[tokio::test]
async fn retry_respects_job_timeout_budget() {
    // Transient failure, but the configured backoff (10s) would exceed the tiny
    // 1s budget, so no retry may be attempted.
    let backend = FlakyBackend::new(
        usize::MAX,
        "timed out waiting for codex app-server response",
    );
    let request = retry_test_request();
    let policy = BackendRetryPolicy::new(3, vec![Duration::from_secs(10)], Duration::from_secs(1));

    let err = run_agent_task_with_retry(&backend, &request, &policy)
        .await
        .expect_err("budget-exhausted retry should propagate the failure");

    assert_eq!(
        backend.calls(),
        1,
        "retry must not exceed the overall job timeout budget"
    );
    assert!(
        err.to_string()
            .contains("timed out waiting for codex app-server")
    );
}
