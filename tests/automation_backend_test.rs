use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;

use tracedecay::automation::backend::{
    backend_availability, classify_agent_task_error_message, extract_json_object_prefix,
    AgentTaskBackend, AgentTaskFailureClass, AgentTaskKind, AgentTaskRequest, AgentTaskResponse,
    CodexAppServerBackend,
};
use tracedecay::automation::config::{AutomationBackend, AutomationConfig};
use tracedecay::sessions::codex_app_server::{
    run_prompt_with_codex_app_server, CodexAppServerSummaryConfig,
};

mod common;

use common::{fake_codex_bin, install_fake_codex_launcher, windows_python_launcher, EnvVarGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn fake_codex_response_timeout() -> Duration {
    if cfg!(windows) {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(5)
    }
}

fn fake_codex_response_timeout_secs() -> u64 {
    fake_codex_response_timeout().as_secs()
}

struct EchoBackend;

impl AgentTaskBackend for EchoBackend {
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> tracedecay::errors::Result<AgentTaskResponse> {
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
        extract_json_object_prefix("```json\n{\"facts\":[]}\n```\n\nSummary: no facts.").unwrap()
            ["facts"],
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
fn fake_codex_app_server_returns_summary_and_logs_protocol() {
    let fake = FakeCodexAppServer::new();
    let config = CodexAppServerSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout: fake_codex_response_timeout(),
        max_tokens: Some(2048),
        temperature: Some(0.2),
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
    assert_eq!(messages[3]["params"]["maxOutputTokens"], 2048);
    assert!(
        (messages[3]["params"]["temperature"].as_f64().unwrap() - 0.2).abs() < 0.0001,
        "temperature should be forwarded: {}",
        messages[3]["params"]["temperature"]
    );
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
    let backend = CodexAppServerBackend::from_config(CodexAppServerSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout: fake_codex_response_timeout(),
        max_tokens: None,
        temperature: None,
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
        "skill_writer:v1"
    );
    assert_eq!(backend_request["evidence_hash"], "sha256:evidence");
    assert_eq!(backend_request["prompt"], r#"{"skills":[]}"#);
    assert_eq!(backend_request["context"], json!({"kind":"test"}));
}

#[test]
fn codex_app_server_backend_uses_first_schema_matching_json_object() {
    let fake = FakeCodexAppServer::new_with_behavior("json_after_echo");
    let backend = CodexAppServerBackend::from_config(CodexAppServerSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout: fake_codex_response_timeout(),
        max_tokens: None,
        temperature: None,
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
    let backend = CodexAppServerBackend::from_config(CodexAppServerSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout: fake_codex_response_timeout(),
        max_tokens: None,
        temperature: None,
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
fn codex_app_server_backend_from_automation_config_forwards_runtime_limits() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let fake = FakeCodexAppServer::new_with_behavior("json");
    let _codex_bin = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &fake.bin);
    let backend = CodexAppServerBackend::from_automation_config(&AutomationConfig {
        backend: AutomationBackend::CodexAppServer,
        model: Some("automation-model".to_string()),
        timeout_secs: fake_codex_response_timeout_secs(),
        max_tokens: Some(1024),
        temperature: Some(0.4),
        ..AutomationConfig::default()
    });
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
    assert_eq!(messages[2]["params"]["model"], "automation-model");
    assert_eq!(messages[3]["params"]["model"], "automation-model");
    assert_eq!(messages[3]["params"]["maxOutputTokens"], 1024);
    assert!(
        (messages[3]["params"]["temperature"].as_f64().unwrap() - 0.4).abs() < 0.0001,
        "temperature should be forwarded: {}",
        messages[3]["params"]["temperature"]
    );
    assert_process_gone(fake.child_pid());
}

#[test]
fn codex_app_server_backend_uses_env_runtime_limits_when_config_omits_them() {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let fake = FakeCodexAppServer::new_with_behavior("json");
    let _codex_bin = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &fake.bin);
    let _max_tokens = EnvVarGuard::set("TRACEDECAY_CODEX_SUMMARY_MAX_TOKENS", "2048");
    let _temperature = EnvVarGuard::set("TRACEDECAY_CODEX_SUMMARY_TEMPERATURE", "0.25");
    let backend = CodexAppServerBackend::from_automation_config(&AutomationConfig {
        backend: AutomationBackend::CodexAppServer,
        model: Some("automation-model".to_string()),
        timeout_secs: fake_codex_response_timeout_secs(),
        max_tokens: None,
        temperature: None,
        ..AutomationConfig::default()
    });
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
    assert_eq!(messages[3]["params"]["maxOutputTokens"], 2048);
    assert!(
        (messages[3]["params"]["temperature"].as_f64().unwrap() - 0.25).abs() < 0.0001,
        "temperature should fall back to env config: {}",
        messages[3]["params"]["temperature"]
    );
    assert_process_gone(fake.child_pid());
}

#[test]
fn codex_app_server_backend_propagates_timeout_errors_and_reaps_child() {
    let (err, pid) = backend_error_for_behavior("timeout", Duration::from_millis(500));

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
    assert!(unavailable
        .reason
        .as_deref()
        .is_some_and(|reason| reason.contains("was not found")));
}

#[test]
fn fake_codex_app_server_uses_thread_model_when_turn_omits_model() {
    let fake = FakeCodexAppServer::new_with_behavior("thread_model_only");
    let config = CodexAppServerSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout: fake_codex_response_timeout(),
        max_tokens: None,
        temperature: None,
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
        max_tokens: None,
        temperature: None,
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
        timeout: Duration::from_millis(500),
        max_tokens: None,
        temperature: None,
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
        max_tokens: None,
        temperature: None,
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
    pid: PathBuf,
}

fn backend_error_for_behavior(behavior: &str, timeout: Duration) -> (String, u32) {
    let fake = FakeCodexAppServer::new_with_behavior(behavior);
    let backend = CodexAppServerBackend::from_config(CodexAppServerSummaryConfig {
        codex_bin: fake.bin.display().to_string(),
        model: Some("configured-model".to_string()),
        timeout,
        max_tokens: None,
        temperature: None,
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
        thread::sleep(Duration::from_millis(10));
        Self {
            _temp: temp,
            bin,
            log,
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
