//! Codex app-server adapter used to generate auxiliary compaction summaries.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write as IoWrite};
#[cfg(windows)]
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::errors::{Result, TraceDecayError};
use crate::sessions::lcm::LcmSummaryRequest;

pub const CODEX_SUMMARY_CHILD_ENV: &str = "TRACEDECAY_CODEX_SUMMARY_CHILD";

#[derive(Debug, Clone)]
pub struct CodexAppServerSummaryConfig {
    pub codex_bin: String,
    pub model: Option<String>,
    pub timeout: Duration,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAppServerSummary {
    pub text: String,
    pub model: Option<String>,
}

impl Default for CodexAppServerSummaryConfig {
    fn default() -> Self {
        Self {
            codex_bin: "codex".to_string(),
            model: None,
            timeout: Duration::from_secs(90),
            max_tokens: None,
            temperature: None,
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
        if let Some(max_tokens) =
            non_empty_env("TRACEDECAY_CODEX_SUMMARY_MAX_TOKENS").and_then(|value| {
                value
                    .parse::<u32>()
                    .ok()
                    .filter(|max_tokens| *max_tokens > 0)
            })
        {
            config.max_tokens = Some(max_tokens);
        }
        if let Some(temperature) =
            non_empty_env("TRACEDECAY_CODEX_SUMMARY_TEMPERATURE").and_then(|value| {
                value
                    .parse::<f32>()
                    .ok()
                    .filter(|temperature| temperature.is_finite() && *temperature >= 0.0)
            })
        {
            config.temperature = Some(temperature);
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
    let model = configured_model(config);
    let mut command = codex_app_server_command(&config.codex_bin);
    let child = command
        .env(CODEX_SUMMARY_CHILD_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| TraceDecayError::Config {
            message: format!("failed to start `{}` app-server: {err}", config.codex_bin),
        })?;
    let mut child = ChildGuard { child };

    let stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| TraceDecayError::Config {
            message: "codex app-server stdout was not available".to_string(),
        })?;
    let (line_tx, line_rx) = mpsc::channel::<std::io::Result<String>>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut stdin = child
        .child
        .stdin
        .take()
        .ok_or_else(|| TraceDecayError::Config {
            message: "codex app-server stdin was not available".to_string(),
        })?;
    let deadline = Instant::now() + config.timeout;
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
    wait_for_response(&line_rx, deadline, 0)?;
    send_json(&mut stdin, &json!({"method": "initialized", "params": {}}))?;

    let thread_params = build_ephemeral_thread_start_params(model, thread_source);
    send_json(
        &mut stdin,
        &json!({"method": "thread/start", "id": 1, "params": thread_params}),
    )?;
    let thread_response = wait_for_response(&line_rx, deadline, 1)?;
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

    let mut turn_params = json!({
        "threadId": thread_id,
        "input": [{"type": "text", "text": prompt}],
        "cwd": std::env::temp_dir().to_string_lossy(),
        "effort": "low",
        "summary": "concise"
    });
    if let Some(model) = model {
        turn_params["model"] = json!(model);
    }
    if let Some(max_tokens) = config.max_tokens {
        turn_params["maxOutputTokens"] = json!(max_tokens);
    }
    if let Some(temperature) = config.temperature {
        turn_params["temperature"] = json!(temperature);
    }
    send_json(
        &mut stdin,
        &json!({"method": "turn/start", "id": 2, "params": turn_params}),
    )?;

    let mut summary = wait_for_turn_summary(&line_rx, deadline)?;
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
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        kill_child_process_tree(&mut self.child);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
fn kill_child_process_tree(child: &mut Child) {
    // `cmd /C` shims wait for their grandchildren, so a child that already
    // exited has no live process tree left; skip the taskkill spawn that
    // would fail anyway.
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let _ = Command::new("taskkill")
        .arg("/PID")
        .arg(child.id().to_string())
        .arg("/T")
        .arg("/F")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn kill_child_process_tree(_child: &mut Child) {}

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
                return Ok(CodexAppServerSummary { text, model });
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
    const MODEL_KEYS: [&str; 13] = [
        "model",
        "model_id",
        "modelId",
        "model_name",
        "modelName",
        "model_slug",
        "modelSlug",
        "model_display_name",
        "modelDisplayName",
        "display_model",
        "displayModel",
        "display_model_name",
        "displayModelName",
    ];
    match value {
        Value::Object(map) => {
            for key in MODEL_KEYS {
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
    use crate::sessions::lcm::{LcmSummaryRequest, LcmSummarySourceMessage, LcmSummarySourceRange};
    use serde_json::json;
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
        assert!(tx
            .send(Ok(json!({
                "method": "item/completed",
                "params": {
                    "model": "gpt-5.5-codex-actual",
                    "item": {"content": [{"text": "summary text"}]}
                }
            })
            .to_string()))
            .is_ok());
        assert!(tx
            .send(Ok(json!({"method": "turn/completed"}).to_string()))
            .is_ok());

        let summary = match wait_for_turn_summary(&rx, Instant::now() + Duration::from_secs(1)) {
            Ok(summary) => summary,
            Err(err) => panic!("turn summary should be returned: {err}"),
        };
        assert_eq!(summary.text, "summary text");
        assert_eq!(summary.model.as_deref(), Some("gpt-5.5-codex-actual"));
    }

    #[test]
    fn summary_thread_start_params_are_ephemeral_and_identified() {
        let params =
            build_ephemeral_thread_start_params(Some("gpt-5.5-codex"), "tracedecay_codex_summary");

        assert_eq!(params["ephemeral"], json!(true));
        assert_eq!(params["threadSource"], json!("tracedecay_codex_summary"));
        assert_eq!(params["model"], json!("gpt-5.5-codex"));
    }
}
