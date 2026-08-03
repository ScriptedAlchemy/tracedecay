//! Root-owned automation backend composition over leaf-owned contracts and policy.

use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use crate::errors::{Result, TraceDecayError};
use crate::sessions::codex_app_server::{
    CodexAppServerSummaryConfig, run_prompt_with_codex_app_server,
};

use super::config::{AutomationBackend, AutomationConfig};

pub use tracedecay_automation::backend::{
    AGENT_TASK_MAX_ATTEMPTS, AGENT_TASK_RETRY_BACKOFFS, AgentBackendAvailability,
    AgentTaskContract, AgentTaskFailureClass, AgentTaskFailureDisposition, AgentTaskKind,
    AgentTaskRequest, AgentTaskResponse, BackendRetryPolicy, agent_task_contract,
    agent_task_failure_disposition, classify_agent_task_error_message, prompt_version, task_key,
};

/// Root operation adapter for a concrete automation backend.
pub trait AgentTaskBackend: Send + Sync {
    fn run_task(&self, request: &AgentTaskRequest) -> Result<AgentTaskResponse>;
}

/// Runs a root backend operation using the leaf retry policy.
pub async fn run_agent_task_with_retry(
    backend: &dyn AgentTaskBackend,
    request: &AgentTaskRequest,
    policy: &BackendRetryPolicy,
) -> Result<AgentTaskResponse> {
    let start = Instant::now();
    let mut attempt: u32 = 1;
    loop {
        match backend.run_task(request) {
            Ok(response) => return Ok(response),
            Err(err) => {
                let Some(backoff) =
                    policy.retry_backoff_after_failure(attempt, start.elapsed(), &err.to_string())
                else {
                    return Err(err);
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
    if config.backend != AutomationBackend::CodexAppServer {
        return tracedecay_automation::backend::backend_availability(config, "", false);
    }

    let summary_config = CodexAppServerSummaryConfig::from_env();
    let executable = summary_config.codex_bin;
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
        config.timeout = std::time::Duration::from_secs(timeout_secs.clamp(5, 300));
        Self { config }
    }

    pub fn from_config(config: CodexAppServerSummaryConfig) -> Self {
        Self { config }
    }
}

impl AgentTaskBackend for CodexAppServerBackend {
    fn run_task(&self, request: &AgentTaskRequest) -> Result<AgentTaskResponse> {
        let backend_message = request.backend_message().map_err(config_automation_error)?;
        let summary = run_prompt_with_codex_app_server(
            &backend_message,
            &self.config,
            "tracedecay_automation",
        )?;
        let output_json = request
            .contract
            .strict_json
            .then(|| {
                tracedecay_automation::backend::extract_response_json_object_preserving_json(
                    &summary.text,
                    &request.contract,
                )
            })
            .transpose()
            .map_err(automation_error)?;
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
    tracedecay_automation::backend::extract_json_object_prefix_preserving_json(text)
        .map_err(automation_error)
}

fn automation_error(error: tracedecay_automation::backend::JsonExtractionError) -> TraceDecayError {
    match error {
        tracedecay_automation::backend::JsonExtractionError::Json(error) => {
            TraceDecayError::Json(error)
        }
        tracedecay_automation::backend::JsonExtractionError::Config(error) => {
            TraceDecayError::Config {
                message: error.to_string(),
            }
        }
    }
}

fn config_automation_error(error: tracedecay_automation::AutomationError) -> TraceDecayError {
    TraceDecayError::Config {
        message: error.to_string(),
    }
}
