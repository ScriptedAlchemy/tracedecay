//! Cursor CLI adapter used to generate auxiliary compaction summaries.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::errors::{Result, TraceDecayError};
use crate::sessions::codex_app_server::strip_reasoning_tags;
use crate::sessions::lcm::LcmSummaryRequest;

pub const CURSOR_SUMMARY_CHILD_ENV: &str = "TRACEDECAY_CURSOR_SUMMARY_CHILD";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorAgentSummaryConfig {
    pub cursor_agent_bin: String,
    pub model: Option<String>,
    pub timeout: Duration,
    pub workspace: Option<PathBuf>,
}

impl Default for CursorAgentSummaryConfig {
    fn default() -> Self {
        Self {
            cursor_agent_bin: "cursor-agent".to_string(),
            model: None,
            timeout: Duration::from_secs(90),
            workspace: None,
        }
    }
}

impl CursorAgentSummaryConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Some(bin) = non_empty_env("TRACEDECAY_CURSOR_AGENT_BIN") {
            config.cursor_agent_bin = bin;
        }
        if let Some(model) = non_empty_env("TRACEDECAY_CURSOR_SUMMARY_MODEL") {
            config.model = Some(model);
        }
        if let Some(secs) = non_empty_env("TRACEDECAY_CURSOR_SUMMARY_TIMEOUT_SECS")
            .and_then(|secs| secs.parse::<u64>().ok())
        {
            config.timeout = Duration::from_secs(secs.clamp(5, 300));
        }
        if let Some(workspace) = non_empty_env("TRACEDECAY_CURSOR_SUMMARY_WORKSPACE") {
            config.workspace = Some(PathBuf::from(workspace));
        }
        config
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

pub fn summarize_with_cursor_agent(
    request: &LcmSummaryRequest,
    config: &CursorAgentSummaryConfig,
) -> Result<String> {
    let prompt = build_cursor_summary_prompt(request);
    let workspace = config.workspace.clone().unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&workspace)?;
    let prompt_path = workspace.join(cursor_summary_prompt_filename());
    std::fs::write(&prompt_path, prompt)?;
    let _prompt_cleanup = FileCleanupGuard(prompt_path.clone());
    let driver_prompt = format!(
        "Read the TraceDecay summary input file at {} and produce the requested durable summary. Return only the summary text. Do not inspect any other files.",
        prompt_path.display()
    );

    let mut command = Command::new(&config.cursor_agent_bin);
    command
        .arg("-p")
        .arg("--output-format")
        .arg("text")
        .arg("--mode")
        .arg("ask")
        .arg("--trust")
        .arg("--sandbox")
        .arg("enabled")
        .arg("--workspace")
        .arg(&workspace);
    if let Some(model) = config.model.as_deref().filter(|model| !model.is_empty()) {
        command.arg("--model").arg(model);
    }
    command
        .arg(driver_prompt)
        .env(CURSOR_SUMMARY_CHILD_ENV, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|err| TraceDecayError::Config {
        message: format!("failed to start `{}`: {err}", config.cursor_agent_bin),
    })?;
    let deadline = Instant::now() + config.timeout;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(TraceDecayError::Config {
                message: format!("timed out waiting for `{}`", config.cursor_agent_bin),
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(TraceDecayError::Config {
            message: if stderr.is_empty() {
                format!(
                    "`{}` exited with status {}",
                    config.cursor_agent_bin, output.status
                )
            } else {
                format!(
                    "`{}` exited with status {}: {}",
                    config.cursor_agent_bin,
                    output.status,
                    stderr.chars().take(2000).collect::<String>()
                )
            },
        });
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let text = strip_reasoning_tags(&text);
    let text = text.trim();
    if text.is_empty() {
        return Err(TraceDecayError::Config {
            message: "cursor-agent returned an empty summary".to_string(),
        });
    }
    Ok(text.to_string())
}

struct FileCleanupGuard(PathBuf);

impl Drop for FileCleanupGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn cursor_summary_prompt_filename() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        "tracedecay-cursor-summary-{}-{nanos}.txt",
        std::process::id()
    )
}

pub fn build_cursor_summary_prompt(request: &LcmSummaryRequest) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are generating a durable TraceDecay LCM summary from Cursor transcript messages.\n",
    );
    prompt.push_str("Return only the summary text. Do not mention that you are summarizing. Do not inspect project files or run shell commands.\n\n");
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
