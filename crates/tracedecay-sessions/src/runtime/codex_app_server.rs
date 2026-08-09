//! Codex app-server adapter used to generate auxiliary compaction summaries.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{BufReader, ErrorKind, Write as IoWrite};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use serde_json::{Value, json};
use tracedecay_store::cursor_dispatch::CURSOR_MODEL_KEYS;

use crate::admission::{MAX_WIRE_MESSAGE_BYTES, wire_oversized_io_error};
use crate::runtime::lcm::LcmSummaryRequest;
use crate::runtime::source::{RawJsonlFrame, RawJsonlFrameReader};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

pub const CODEX_SUMMARY_CHILD_ENV: &str = "TRACEDECAY_CODEX_SUMMARY_CHILD";
const CODEX_APP_SERVER_SPAWN_RETRY_WINDOW: Duration = Duration::from_millis(250);
const CODEX_APP_SERVER_SPAWN_RETRY_SLEEP: Duration = Duration::from_millis(10);

#[derive(Default)]
struct ActiveCodexChildren {
    process_groups: HashSet<u32>,
    shutdown_guards: usize,
}

static ACTIVE_CODEX_CHILDREN: OnceLock<Mutex<ActiveCodexChildren>> = OnceLock::new();

fn active_codex_children() -> &'static Mutex<ActiveCodexChildren> {
    ACTIVE_CODEX_CHILDREN.get_or_init(|| Mutex::new(ActiveCodexChildren::default()))
}

#[derive(Clone, Default)]
pub struct CodexAppServerCancellation {
    cancelled: Arc<AtomicBool>,
    process_group: Arc<Mutex<Option<u32>>>,
}

impl CodexAppServerCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(process_group) = *self
            .process_group
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            terminate_process_tree(process_group);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn register(&self, process_group: u32) {
        *self
            .process_group
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(process_group);
        if self.is_cancelled() {
            terminate_process_tree(process_group);
        }
    }

    fn unregister(&self, process_group: u32) {
        let mut registered = self
            .process_group
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *registered == Some(process_group) {
            *registered = None;
        }
    }
}

#[cfg_attr(
    windows,
    allow(
        dead_code,
        reason = "Windows daemon shutdown does not use this guard yet"
    )
)]
pub struct CodexAppServerShutdownGuard;

#[cfg_attr(
    windows,
    allow(
        dead_code,
        reason = "Windows daemon shutdown does not use this guard yet"
    )
)]
pub fn begin_codex_app_server_shutdown() -> CodexAppServerShutdownGuard {
    let process_groups = {
        let mut active = active_codex_children()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active.shutdown_guards += 1;
        active.process_groups.iter().copied().collect::<Vec<_>>()
    };
    for process_group in process_groups {
        terminate_process_tree(process_group);
    }
    CodexAppServerShutdownGuard
}

impl Drop for CodexAppServerShutdownGuard {
    fn drop(&mut self) {
        let mut active = active_codex_children()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active.shutdown_guards = active.shutdown_guards.saturating_sub(1);
    }
}

#[derive(Debug, Clone)]
pub struct CodexAppServerSummaryConfig {
    pub codex_bin: String,
    pub model: Option<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAppServerSummary {
    pub text: String,
    pub model: Option<String>,
    /// Provider-native thread identity returned by the admitted thread/start.
    pub thread_id: String,
}

impl Default for CodexAppServerSummaryConfig {
    fn default() -> Self {
        Self {
            codex_bin: "codex".to_string(),
            model: None,
            timeout: Duration::from_secs(90),
        }
    }
}

impl CodexAppServerSummaryConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Some(bin) = non_empty_env("TRACEDECAY_CODEX_BIN") {
            config.codex_bin = bin;
        }
        if let Some(model) = non_empty_env("TRACEDECAY_CODEX_SUMMARY_MODEL") {
            config.model = Some(model);
        }
        if let Some(secs) = non_empty_env("TRACEDECAY_CODEX_SUMMARY_TIMEOUT_SECS")
            .and_then(|secs| secs.parse::<u64>().ok())
        {
            config.timeout = Duration::from_secs(secs.clamp(5, 300));
        }
        config
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn configured_model(config: &CodexAppServerSummaryConfig) -> Option<&str> {
    config.model.as_deref().filter(|model| !model.is_empty())
}

pub fn summarize_with_codex_app_server(
    request: &LcmSummaryRequest,
    config: &CodexAppServerSummaryConfig,
) -> Result<CodexAppServerSummary> {
    let prompt = build_codex_summary_prompt(request);
    run_prompt_with_codex_app_server(&prompt, config, "tracedecay_codex_summary")
}

pub fn run_prompt_with_codex_app_server(
    prompt: &str,
    config: &CodexAppServerSummaryConfig,
    thread_source: &str,
) -> Result<CodexAppServerSummary> {
    run_prompt_with_optional_cancellation(prompt, config, thread_source, None, None, None, None)
}

/// Runs a Work attempt through Codex app-server with only the environment
/// values captured for this spawn. The durable Work authority is the
/// snapshot's allowlisted key set; callers resolve those keys just in time, so
/// this function never persists plaintext credential values.
pub fn run_work_with_codex_app_server(
    prompt: &str,
    config: &CodexAppServerSummaryConfig,
    thread_source: &str,
    cancellation: &CodexAppServerCancellation,
    cwd: &Path,
    timeout: Duration,
    admitted_environment: &BTreeMap<String, OsString>,
) -> Result<CodexAppServerSummary> {
    run_prompt_with_optional_cancellation(
        prompt,
        config,
        thread_source,
        Some(cancellation),
        Some(cwd),
        Some(timeout),
        Some(admitted_environment),
    )
}

fn run_prompt_with_optional_cancellation(
    prompt: &str,
    config: &CodexAppServerSummaryConfig,
    thread_source: &str,
    cancellation: Option<&CodexAppServerCancellation>,
    cwd: Option<&Path>,
    timeout: Option<Duration>,
    admitted_environment: Option<&BTreeMap<String, OsString>>,
) -> Result<CodexAppServerSummary> {
    let model = configured_model(config);
    let mut command = codex_app_server_command(&config.codex_bin);
    if let Some(admitted_environment) = admitted_environment {
        command.env_clear();
        for (key, value) in admitted_environment {
            command.env(key, value);
        }
    }
    command
        // The recursion guard is an internal invariant. Set it after the
        // admitted map so a caller cannot replace it through an allowlisted
        // key.
        .env(CODEX_SUMMARY_CHILD_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = spawn_codex_app_server(&mut command, &config.codex_bin)?;
    let process_group = child.id();
    let mut child = ChildGuard {
        child,
        cancellation: cancellation.cloned(),
    };
    if let Some(cancellation) = cancellation {
        cancellation.register(process_group);
    }

    let stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| TraceDecayError::Config {
            message: "codex app-server stdout was not available".to_string(),
        })?;
    let (line_tx, line_rx) = mpsc::channel::<std::io::Result<String>>();
    let stdout_reader = std::thread::spawn(move || {
        let mut frames = RawJsonlFrameReader::new(BufReader::new(stdout), MAX_WIRE_MESSAGE_BYTES);
        loop {
            let line = match frames.next_frame() {
                Ok(RawJsonlFrame::Eof) => break,
                Ok(RawJsonlFrame::Complete { .. } | RawJsonlFrame::Partial { .. }) => {
                    String::from_utf8(frames.record().to_vec()).map_err(|error| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                    })
                }
                Ok(RawJsonlFrame::Oversized { .. } | RawJsonlFrame::BudgetExhausted { .. }) => {
                    Err(wire_oversized_io_error())
                }
                Err(error) => Err(error),
            };
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    let outcome = run_codex_protocol(
        &mut child,
        &line_rx,
        prompt,
        config,
        thread_source,
        model,
        cwd,
        timeout.unwrap_or(config.timeout),
    );
    drop(child);
    let _ = stdout_reader.join();
    outcome
}

#[allow(clippy::too_many_arguments)]
fn run_codex_protocol(
    child: &mut ChildGuard,
    line_rx: &mpsc::Receiver<std::io::Result<String>>,
    prompt: &str,
    config: &CodexAppServerSummaryConfig,
    thread_source: &str,
    model: Option<&str>,
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<CodexAppServerSummary> {
    let mut stdin = child
        .child
        .stdin
        .take()
        .ok_or_else(|| TraceDecayError::Config {
            message: "codex app-server stdin was not available".to_string(),
        })?;
    let deadline = Instant::now() + timeout.min(config.timeout);
    send_json(
        &mut stdin,
        &json!({
            "method": "initialize",
            "id": 0,
            "params": {
                "clientInfo": {
                    "name": "tracedecay_codex_summary",
                    "title": "TraceDecay Codex Summary",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }),
    )?;
    wait_for_response(line_rx, deadline, 0)?;
    send_json(&mut stdin, &json!({"method": "initialized", "params": {}}))?;

    let thread_params = build_ephemeral_thread_start_params(model, thread_source);
    send_json(
        &mut stdin,
        &json!({"method": "thread/start", "id": 1, "params": thread_params}),
    )?;
    let thread_response = wait_for_response(line_rx, deadline, 1)?;
    let thread_model = find_model_id(&thread_response);
    let thread_id = thread_response
        .pointer("/result/thread/id")
        .or_else(|| thread_response.pointer("/result/id"))
        .and_then(Value::as_str)
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "codex app-server thread/start response lacked a thread id: {thread_response}"
            ),
        })?
        .to_string();

    let cwd = cwd.unwrap_or_else(|| Path::new("."));
    let mut turn_params = json!({
        "threadId": thread_id,
        "input": [{"type": "text", "text": prompt}],
        "cwd": cwd.to_string_lossy(),
        "effort": "low",
        "summary": "concise"
    });
    if let Some(model) = model {
        turn_params["model"] = json!(model);
    }
    send_json(
        &mut stdin,
        &json!({"method": "turn/start", "id": 2, "params": turn_params}),
    )?;
    drop(stdin);

    let mut summary = wait_for_turn_summary(line_rx, deadline, thread_id)?;
    if summary.model.is_none() {
        summary.model = thread_model;
    }
    let text = strip_reasoning_tags(&summary.text);
    let text = text.trim();
    if text.is_empty() {
        return Err(TraceDecayError::Config {
            message: "codex app-server returned an empty summary".to_string(),
        });
    }
    summary.text = text.to_string();
    Ok(summary)
}

fn spawn_codex_app_server(command: &mut Command, codex_bin: &str) -> Result<Child> {
    #[cfg(unix)]
    command.process_group(0);
    let deadline = Instant::now() + CODEX_APP_SERVER_SPAWN_RETRY_WINDOW;
    loop {
        let spawn_result = {
            let mut active = active_codex_children()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if active.shutdown_guards > 0 {
                return Err(TraceDecayError::Config {
                    message: "codex app-server shutdown is in progress".to_string(),
                });
            }
            let child = command.spawn();
            if let Ok(child) = &child {
                active.process_groups.insert(child.id());
            }
            child
        };
        match spawn_result {
            Ok(child) => return Ok(child),
            Err(err)
                if err.kind() == ErrorKind::ExecutableFileBusy && Instant::now() < deadline =>
            {
                std::thread::sleep(CODEX_APP_SERVER_SPAWN_RETRY_SLEEP);
            }
            Err(err) => {
                return Err(TraceDecayError::Config {
                    message: format!("failed to start `{codex_bin}` app-server: {err}"),
                });
            }
        }
    }
}

fn codex_app_server_command(codex_bin: &str) -> Command {
    let mut command = command_for_codex_bin(codex_bin);
    command.arg("app-server");
    command
}

#[cfg(windows)]
fn command_for_codex_bin(codex_bin: &str) -> Command {
    let extension = Path::new(codex_bin)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    if matches!(extension.as_deref(), Some("bat" | "cmd")) {
        let mut command = Command::new("cmd");
        command.arg("/D").arg("/C").arg(codex_bin);
        return command;
    }
    Command::new(codex_bin)
}

#[cfg(not(windows))]
fn command_for_codex_bin(codex_bin: &str) -> Command {
    Command::new(codex_bin)
}

fn build_ephemeral_thread_start_params(model: Option<&str>, thread_source: &str) -> Value {
    let mut params = json!({
        "ephemeral": true,
        "threadSource": thread_source
    });
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    params
}

struct ChildGuard {
    child: Child,
    cancellation: Option<CodexAppServerCancellation>,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let process_group = self.child.id();
        terminate_process_tree(process_group);
        let _ = self.child.kill();
        let _ = self.child.wait();
        active_codex_children()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .process_groups
            .remove(&process_group);
        if let Some(cancellation) = &self.cancellation {
            cancellation.unregister(process_group);
        }
    }
}

#[cfg(windows)]
fn terminate_process_tree(process_group: u32) {
    let _ = Command::new("taskkill")
        .arg("/PID")
        .arg(process_group.to_string())
        .arg("/T")
        .arg("/F")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(unix)]
fn terminate_process_tree(process_group: u32) {
    const SIGKILL: i32 = 9;
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // The app-server is started as its own process-group leader, so signaling
    // the negative pid also terminates node/codex descendants.
    let _ = unsafe { kill(-(process_group as i32), SIGKILL) };
}

#[cfg(not(any(unix, windows)))]
fn terminate_process_tree(_process_group: u32) {}

fn send_json(stdin: &mut impl IoWrite, value: &Value) -> Result<()> {
    writeln!(stdin, "{value}")?;
    stdin.flush()?;
    Ok(())
}

fn wait_for_response(
    line_rx: &mpsc::Receiver<std::io::Result<String>>,
    deadline: Instant,
    id: i64,
) -> Result<Value> {
    loop {
        let line = recv_line(line_rx, deadline)?;
        let value: Value = serde_json::from_str(&line)?;
        if value.get("id").and_then(Value::as_i64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(TraceDecayError::Config {
                message: format!("codex app-server request {id} failed: {error}"),
            });
        }
        return Ok(value);
    }
}

fn wait_for_turn_summary(
    line_rx: &mpsc::Receiver<std::io::Result<String>>,
    deadline: Instant,
    thread_id: String,
) -> Result<CodexAppServerSummary> {
    let mut text = String::new();
    let mut model = None;
    loop {
        let line = recv_line(line_rx, deadline)?;
        let value: Value = serde_json::from_str(&line)?;
        if model.is_none() {
            model = find_model_id(&value);
        }
        if let Some(error) = value.get("error") {
            return Err(TraceDecayError::Config {
                message: format!("codex app-server turn failed: {error}"),
            });
        }
        match value.get("method").and_then(Value::as_str) {
            Some("item/agentMessage/delta") => {
                if let Some(delta) = value.pointer("/params/delta").and_then(Value::as_str) {
                    text.push_str(delta);
                }
            }
            Some("item/completed") if text.trim().is_empty() => {
                if let Some(item_text) = collect_item_text(value.get("params")) {
                    text.push_str(&item_text);
                }
            }
            Some("turn/completed") => {
                return Ok(CodexAppServerSummary {
                    text,
                    model,
                    thread_id,
                });
            }
            _ => {}
        }
    }
}

fn recv_line(
    line_rx: &mpsc::Receiver<std::io::Result<String>>,
    deadline: Instant,
) -> Result<String> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .unwrap_or_default();
    if remaining.is_zero() {
        return Err(TraceDecayError::Config {
            message: "timed out waiting for codex app-server".to_string(),
        });
    }
    match line_rx.recv_timeout(remaining) {
        Ok(Ok(line)) => Ok(line),
        Ok(Err(err)) => Err(err.into()),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(TraceDecayError::Config {
            message: "timed out waiting for codex app-server".to_string(),
        }),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(TraceDecayError::Config {
            message: "codex app-server closed stdout before completing".to_string(),
        }),
    }
}

fn collect_item_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(|item| collect_item_text(Some(item)))
                .collect::<String>();
            (!text.is_empty()).then_some(text)
        }
        Value::Object(map) => {
            for key in ["text", "message", "item", "content"] {
                if let Some(text) = collect_item_text(map.get(key)) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}

fn find_model_id(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in CURSOR_MODEL_KEYS.iter().copied() {
                if let Some(model) = map
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|model| !model.trim().is_empty())
                {
                    return Some(model.trim().to_string());
                }
            }
            map.iter()
                .filter(|(key, _)| {
                    !matches!(
                        key.as_str(),
                        "provider" | "model_provider" | "modelProvider" | "clientInfo"
                    )
                })
                .find_map(|(_, child)| find_model_id(child))
        }
        Value::Array(items) => items.iter().find_map(find_model_id),
        _ => None,
    }
}

pub fn build_codex_summary_prompt(request: &LcmSummaryRequest) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are generating a durable TraceDecay LCM summary from Codex transcript messages.\n",
    );
    prompt.push_str("Return only the summary text. Do not mention that you are summarizing. Do not inspect files or run tools.\n\n");
    prompt.push_str("Summarization goal:\n");
    prompt.push_str(&request.prompt);
    prompt.push_str("\n\nSource messages:\n");
    for message in &request.source_messages {
        let _ = write!(
            prompt,
            "\n[{} store_id={}]\n{}\n",
            message.role, message.store_id, message.content
        );
    }
    prompt
}

pub fn strip_reasoning_tags(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find("<thinking>") else {
            output.push_str(rest);
            break;
        };
        output.push_str(&rest[..start]);
        let after_start = &rest[start + "<thinking>".len()..];
        let Some(end) = after_start.find("</thinking>") else {
            break;
        };
        rest = &after_start[end + "</thinking>".len()..];
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::lcm::{LcmSummaryRequest, LcmSummarySourceMessage, LcmSummarySourceRange};
    use serde_json::json;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn prompt_contains_source_messages_and_no_tool_instruction() {
        let request = LcmSummaryRequest {
            provider: "codex".to_string(),
            session_id: "s1".to_string(),
            focus_topic: None,
            prompt: "Summarize durable facts.".to_string(),
            source_range: LcmSummarySourceRange {
                from_store_id: 1,
                to_store_id: 2,
            },
            source_messages: vec![
                LcmSummarySourceMessage {
                    store_id: 1,
                    role: "user".to_string(),
                    content: "Need release automation.".to_string(),
                },
                LcmSummarySourceMessage {
                    store_id: 2,
                    role: "assistant".to_string(),
                    content: "Added release-plz.".to_string(),
                },
            ],
            extraction_request: None,
        };

        let prompt = build_codex_summary_prompt(&request);
        assert!(prompt.contains("Do not inspect files or run tools"));
        assert!(prompt.contains("[user store_id=1]"));
        assert!(prompt.contains("Need release automation."));
        assert!(prompt.contains("[assistant store_id=2]"));
        assert!(prompt.contains("Added release-plz."));
    }

    #[test]
    fn strip_reasoning_tags_removes_internal_text() {
        assert_eq!(
            strip_reasoning_tags("before <thinking>hidden</thinking> after").trim(),
            "before  after"
        );
    }

    #[test]
    fn completed_item_text_descends_through_params_item_content() {
        let event = json!({
            "params": {
                "item": {
                    "content": [
                        {"type": "output_text", "text": "first "},
                        {"type": "output_text", "text": "second"}
                    ]
                }
            }
        });

        assert_eq!(
            collect_item_text(event.get("params")).as_deref(),
            Some("first second")
        );
    }

    #[test]
    fn turn_summary_records_actual_model_from_app_server_events() {
        let (tx, rx) = mpsc::channel();
        let thread_id = "summary-thread-actual";
        assert!(
            tx.send(Ok(json!({
                "method": "item/completed",
                "params": {
                    "threadId": thread_id,
                    "model": "gpt-5.5-codex-actual",
                    "item": {"content": [{"text": "summary text"}]}
                }
            })
            .to_string()))
                .is_ok()
        );
        assert!(
            tx.send(Ok(json!({
                "method": "turn/completed",
                "params": {"threadId": thread_id}
            })
            .to_string()))
                .is_ok()
        );

        let summary = match wait_for_turn_summary(
            &rx,
            Instant::now() + Duration::from_secs(1),
            thread_id.to_string(),
        ) {
            Ok(summary) => summary,
            Err(err) => panic!("turn summary should be returned: {err}"),
        };
        assert_eq!(summary.text, "summary text");
        assert_eq!(summary.model.as_deref(), Some("gpt-5.5-codex-actual"));
        assert_eq!(summary.thread_id, thread_id);
    }

    #[test]
    fn summary_thread_start_params_are_ephemeral_and_identified() {
        let params =
            build_ephemeral_thread_start_params(Some("gpt-5.5-codex"), "tracedecay_codex_summary");

        assert_eq!(params["ephemeral"], json!(true));
        assert_eq!(params["threadSource"], json!("tracedecay_codex_summary"));
        assert_eq!(params["model"], json!("gpt-5.5-codex"));
    }

    #[cfg(unix)]
    #[test]
    fn work_app_server_child_receives_only_admitted_environment() {
        let temporary = tempfile::tempdir().expect("temporary app-server directory");
        let marker = temporary.path().join("environment");
        let executable = temporary.path().join("fake-codex");
        let admitted_key = format!("TRACEDECAY_WORK_ADMITTED_{}", std::process::id());
        let ambient_secret = format!("TRACEDECAY_WORK_SECRET_{}", std::process::id());
        let script = format!(
            "#!/bin/sh\nprintf '%s|%s|%s' \"${{{admitted_key}:-missing}}\" \"${{{ambient_secret}:-missing}}\" \"${{{child_marker}:-missing}}\" > {marker}\nwhile IFS= read -r line; do\n  case \"$line\" in\n    *'\"id\":0'*) printf '%s\\n' '{{\"id\":0,\"result\":{{}}}}' ;;\n    *'\"id\":1'*) printf '%s\\n' '{{\"id\":1,\"result\":{{\"thread\":{{\"id\":\"work-thread\"}}}}}}' ;;\n    *'\"id\":2'*) printf '%s\\n' '{{\"method\":\"item/completed\",\"params\":{{\"item\":{{\"content\":[{{\"type\":\"output_text\",\"text\":\"work result\"}}]}}}}}}'; printf '%s\\n' '{{\"method\":\"turn/completed\"}}'; exit 0 ;;\n  esac\ndone\n",
            marker = marker.display(),
            child_marker = CODEX_SUMMARY_CHILD_ENV,
        );
        std::fs::write(&executable, script).expect("write fake app-server");
        let mut permissions = std::fs::metadata(&executable)
            .expect("fake app-server metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions)
            .expect("make fake app-server executable");

        let prior_admitted = std::env::var_os(&admitted_key);
        let prior_secret = std::env::var_os(&ambient_secret);
        // SAFETY: both process-wide test keys are unique to this process and
        // restored before the assertion below.
        unsafe {
            std::env::set_var(&admitted_key, "ambient-replacement");
            std::env::set_var(&ambient_secret, "ambient-secret");
        }
        let admitted_environment = std::collections::BTreeMap::from([
            (
                admitted_key.clone(),
                std::ffi::OsString::from("admitted-value"),
            ),
            (
                CODEX_SUMMARY_CHILD_ENV.to_string(),
                std::ffi::OsString::from("caller-cannot-control-marker"),
            ),
        ]);
        let config = CodexAppServerSummaryConfig {
            codex_bin: executable.to_string_lossy().into_owned(),
            model: None,
            timeout: Duration::from_secs(2),
        };
        let result = run_work_with_codex_app_server(
            "Return a work result.",
            &config,
            "tracedecay_work_attempt",
            &CodexAppServerCancellation::default(),
            temporary.path(),
            Duration::from_secs(2),
            &admitted_environment,
        );
        // SAFETY: return the process environment to the state this test found.
        unsafe {
            match prior_admitted {
                Some(value) => std::env::set_var(&admitted_key, value),
                None => std::env::remove_var(&admitted_key),
            }
            match prior_secret {
                Some(value) => std::env::set_var(&ambient_secret, value),
                None => std::env::remove_var(&ambient_secret),
            }
        }

        let summary = result.expect("work app-server protocol should complete");
        assert_eq!(summary.text, "work result");
        assert_eq!(
            std::fs::read_to_string(&marker).expect("child environment marker"),
            "admitted-value|missing|1"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_guard_terminates_active_child_and_rejects_new_spawns() {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        let temp = tempfile::tempdir().unwrap();
        let descendant_pid_path = temp.path().join("descendant.pid");
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & echo $! > \"$1\"; wait", "sh"])
            .arg(&descendant_pid_path);
        let child = spawn_codex_app_server(&mut command, "sh").expect("spawn child");
        let mut child = ChildGuard {
            child,
            cancellation: None,
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        while !descendant_pid_path.is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let descendant_pid: i32 = std::fs::read_to_string(&descendant_pid_path)
            .expect("descendant pid file")
            .trim()
            .parse()
            .expect("descendant pid");

        let shutdown = begin_codex_app_server_shutdown();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !matches!(child.child.try_wait(), Ok(Some(_))) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            matches!(child.child.try_wait(), Ok(Some(_))),
            "active child should exit during shutdown"
        );
        let descendant_deadline = Instant::now() + Duration::from_secs(1);
        while unsafe { kill(descendant_pid, 0) } == 0 && Instant::now() < descendant_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_ne!(
            unsafe { kill(descendant_pid, 0) },
            0,
            "app-server descendant should exit during shutdown"
        );

        let mut blocked = Command::new("sh");
        blocked.args(["-c", "exit 0"]);
        let err = spawn_codex_app_server(&mut blocked, "sh")
            .expect_err("new app-server spawns must fail during shutdown");
        assert!(err.to_string().contains("shutdown is in progress"));

        drop(shutdown);
    }
}
