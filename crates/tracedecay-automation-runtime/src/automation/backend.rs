//! Runtime adapters for leaf-owned automation backend contracts and policies.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tracedecay_automation::backend as leaf_backend;
pub use tracedecay_automation::backend::{
    AGENT_TASK_MAX_ATTEMPTS, AGENT_TASK_RETRY_BACKOFFS, AgentBackendAvailability, AgentTaskBackend,
    AgentTaskContract, AgentTaskError, AgentTaskFailureClass, AgentTaskFailureDisposition,
    AgentTaskKind, AgentTaskRequest, AgentTaskResponse, AgentTaskRetryAttempt,
    AgentTaskRetryReport, BackendRetryPolicy, agent_task_contract, agent_task_failure_disposition,
    classify_agent_task_error_message, prompt_version, task_key,
};

use crate::errors::Result;
use crate::ports::codex_app_server::{
    SummaryConfig as CodexAppServerSummaryConfig, run_prompt as run_prompt_with_codex_app_server,
};

use super::config::{AutomationBackend, AutomationConfig};

pub fn backend_availability(config: &AutomationConfig) -> AgentBackendAvailability {
    match config.backend {
        AutomationBackend::Disabled => AgentBackendAvailability {
            backend: AutomationBackend::Disabled,
            available: false,
            executable: None,
            reason: Some("automation backend is disabled".to_string()),
        },
        AutomationBackend::CodexAppServer => {
            let summary_config = CodexAppServerSummaryConfig::from_env();
            let executable = summary_config.codex_bin.clone();
            match executable_resolution(&executable) {
                Ok(true) => AgentBackendAvailability {
                    backend: AutomationBackend::CodexAppServer,
                    available: true,
                    executable: Some(executable),
                    reason: None,
                },
                Ok(false) => AgentBackendAvailability {
                    backend: AutomationBackend::CodexAppServer,
                    available: false,
                    executable: Some(executable.clone()),
                    reason: Some(format!(
                        "codex app-server backend executable '{executable}' was not found"
                    )),
                },
                Err(error) => AgentBackendAvailability {
                    backend: AutomationBackend::CodexAppServer,
                    available: false,
                    executable: Some(executable),
                    reason: Some(error.to_string()),
                },
            }
        }
    }
}

fn executable_resolution(bin: &str) -> Result<bool> {
    let path = Path::new(bin);
    if path.components().count() > 1 {
        return Ok(path.is_file());
    }
    Ok(
        super::executable_lookup::resolve_on_path(bin, std::env::var_os("PATH").as_deref())?
            .is_some(),
    )
}

pub async fn run_agent_task_with_retry(
    backend: &dyn AgentTaskBackend,
    request: &AgentTaskRequest,
    policy: &BackendRetryPolicy,
) -> Result<AgentTaskResponse> {
    Ok(leaf_backend::run_agent_task_with_retry(backend, request, policy).await?)
}

pub async fn run_agent_task_with_retry_report(
    backend: &dyn AgentTaskBackend,
    request: &AgentTaskRequest,
    policy: &BackendRetryPolicy,
    report: &mut AgentTaskRetryReport,
) -> Result<AgentTaskResponse> {
    Ok(leaf_backend::run_agent_task_with_retry_report(backend, request, policy, report).await?)
}

pub fn extract_json_object_prefix(text: &str) -> Result<Value> {
    Ok(leaf_backend::extract_json_object_prefix(text)?)
}

#[derive(Debug, Clone)]
pub struct CodexAppServerBackend {
    config: CodexAppServerSummaryConfig,
}

impl CodexAppServerBackend {
    pub fn from_automation_config(config: &AutomationConfig) -> Self {
        Self::new(config.model_id.clone(), config.timeout_secs)
    }

    pub fn new(model: Option<String>, timeout_secs: u64) -> Self {
        let mut config = CodexAppServerSummaryConfig::from_env();
        config.model = model.filter(|model| !model.trim().is_empty());
        config.timeout = Duration::from_secs(timeout_secs.clamp(5, 300));
        Self { config }
    }

    pub fn from_config(config: CodexAppServerSummaryConfig) -> Self {
        Self { config }
    }
}

impl AgentTaskBackend for CodexAppServerBackend {
    // One backend attempt end to end, distinct from the retry-ladder block
    // (`automation.backend.startup`) that also includes backoff sleeps.
    #[hotpath::measure(
        label = "automation.backend.invoke.codex_app_server",
        impl_type = "CodexAppServerBackend"
    )]
    fn run_task(
        &self,
        request: &AgentTaskRequest,
    ) -> std::result::Result<AgentTaskResponse, AgentTaskError> {
        let backend_message =
            request
                .backend_message()
                .map_err(|error| AgentTaskError::Failed {
                    reason: error.to_string(),
                })?;
        // The app-server port renders its failure as one message; the typed
        // taxonomy admits that string exactly once, at this boundary.
        let summary = run_prompt_with_codex_app_server(
            &backend_message,
            &self.config,
            "tracedecay_automation",
        )
        .map_err(AgentTaskError::from_backend_message)?;
        let output_json = request
            .contract
            .strict_json
            .then(|| leaf_backend::extract_response_json_object(&summary.text, &request.contract))
            .transpose()
            .map_err(|error| AgentTaskError::MalformedOutput {
                reason: error.to_string(),
            })?;
        Ok(AgentTaskResponse {
            run_id: request.run_id.clone(),
            task: request.task,
            output_json,
            output_text: summary.text,
            model: summary.model.or_else(|| self.config.model.clone()),
            provider: Some("codex".to_owned()),
            input_tokens: None,
            output_tokens: None,
        })
    }
}
