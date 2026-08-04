use std::path::Path;
use std::time::{Duration, Instant};

pub use tracedecay_automation::backend::{
    AGENT_TASK_MAX_ATTEMPTS, AGENT_TASK_RETRY_BACKOFFS, AgentBackendAvailability,
    AgentTaskContract, AgentTaskFailureClass, AgentTaskFailureDisposition, AgentTaskKind,
    AgentTaskRequest, AgentTaskResponse, BackendRetryPolicy, JsonExtractionError,
    agent_task_contract, agent_task_failure_disposition, classify_agent_task_error_message,
    extract_json_object_prefix, extract_json_object_prefix_preserving_json,
    extract_response_json_object, extract_response_json_object_preserving_json, prompt_version,
    task_key,
};

use super::config::AutomationConfig;
use crate::errors::Result;
use crate::sessions::codex_app_server::{
    CodexAppServerSummaryConfig, run_prompt_with_codex_app_server,
};

pub trait AgentTaskBackend: Send + Sync {
    fn run_task(&self, request: &AgentTaskRequest) -> Result<AgentTaskResponse>;
}

pub async fn run_agent_task_with_retry(
    backend: &dyn AgentTaskBackend,
    request: &AgentTaskRequest,
    policy: &BackendRetryPolicy,
) -> Result<AgentTaskResponse> {
    let start = Instant::now();
    let mut attempt = 1;
    loop {
        match backend.run_task(request) {
            Ok(response) => return Ok(response),
            Err(error) => {
                let Some(backoff) = policy.retry_backoff_after_failure(
                    attempt,
                    start.elapsed(),
                    &error.to_string(),
                ) else {
                    return Err(error);
                };
                if !backoff.is_zero() {
                    tokio::time::sleep(backoff).await;
                }
                attempt += 1;
            }
        }
    }
}

pub fn backend_availability(config: &AutomationConfig) -> AgentBackendAvailability {
    let summary_config = CodexAppServerSummaryConfig::from_env();
    let executable = summary_config.codex_bin.clone();
    tracedecay_automation::backend::backend_availability(
        config,
        &executable,
        executable_is_resolvable(&executable),
    )
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
    pub fn from_automation_config(config: &AutomationConfig) -> Self {
        Self::new(None, config.timeout_secs)
    }

    pub fn new(model: Option<String>, timeout_secs: u64) -> Self {
        let mut config = CodexAppServerSummaryConfig::from_env();
        if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
            config.model = Some(model);
        }
        config.timeout = Duration::from_secs(timeout_secs.clamp(5, 300));
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
