//! Runtime adapters for leaf-owned automation backend contracts and policies.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;
use tracedecay_automation::AutomationError;
use tracedecay_automation::backend as leaf_backend;
pub use tracedecay_automation::backend::{
    AgentBackendAvailability, AgentTaskBackend, AgentTaskContract, AgentTaskError,
    AgentTaskFailureClass, AgentTaskFailureDisposition, AgentTaskKind, AgentTaskRequest,
    AgentTaskResponse, agent_task_contract, agent_task_failure_disposition,
    classify_agent_task_error_message, prompt_version, task_key,
};
use tracedecay_domain::errors::Result;

use super::config::{AutomationBackend, AutomationConfig};
use crate::ports::codex_app_server::{
    SummaryConfig as CodexAppServerSummaryConfig, run_prompt as run_prompt_with_codex_app_server,
};

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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentTaskRetryAttempt {
    pub attempt: u32,
    pub succeeded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_classification: Option<AgentTaskFailureClass>,
    pub backoff_millis: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

    pub fn append(&mut self, later: Self) {
        self.attempts.extend(later.attempts);
    }
}

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
    run_agent_task_with_retry_report(
        backend,
        request,
        policy,
        &mut AgentTaskRetryReport::default(),
    )
    .await
}

#[hotpath::measure(label = "automation.backend.run_task", future = true)]
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
        match hotpath::measure_block!("automation.backend.invoke", backend.run_task(request)) {
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
                let classification = error.failure_class();
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
                    return Err(AutomationError::config(error.to_string()).into());
                }
                if !backoff.is_zero() {
                    tokio::time::sleep(backoff).await;
                }
                attempt += 1;
            }
        }
    }
}

pub fn extract_json_object_prefix(text: &str) -> Result<Value> {
    leaf_backend::extract_json_object_prefix(text).map_err(Into::into)
}

#[derive(Debug, Clone)]
pub struct CodexAppServerBackend {
    config: CodexAppServerSummaryConfig,
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
        error: AgentTaskError,
    }

    impl FlakyBackend {
        fn failing_with(failures: usize, error: AgentTaskError) -> Self {
            Self {
                failures,
                calls: AtomicUsize::new(0),
                error,
            }
        }
    }

    impl AgentTaskBackend for FlakyBackend {
        fn run_task(
            &self,
            request: &AgentTaskRequest,
        ) -> std::result::Result<AgentTaskResponse, AgentTaskError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.failures {
                return Err(self.error.clone());
            }
            Ok(AgentTaskResponse {
                run_id: request.run_id.clone(),
                task: request.task,
                output_text: "recovered".to_string(),
                output_json: None,
                model: None,
                provider: None,
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

    #[tokio::test]
    async fn denied_task_is_never_retried_and_surfaces_denial() {
        let backend = FlakyBackend::failing_with(
            usize::MAX,
            AgentTaskError::Denied {
                reason: "workspace write scope was denied".to_string(),
            },
        );
        let policy = BackendRetryPolicy::new(3, vec![Duration::ZERO], Duration::from_secs(120));
        let mut report = AgentTaskRetryReport::default();

        let error = run_agent_task_with_retry_report(&backend, &request(), &policy, &mut report)
            .await
            .unwrap_err();

        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.attempt_count(), 1);
        assert_eq!(
            report.attempts()[0].failure_classification,
            Some(AgentTaskFailureClass::Denied)
        );
        assert!(
            error.to_string().contains("agent task denied"),
            "denial must survive the retry boundary: {error}"
        );
    }

    #[tokio::test]
    async fn disconnected_task_is_retried_and_classified_as_disconnect() {
        let backend = FlakyBackend::failing_with(
            1,
            AgentTaskError::Disconnected {
                reason: "connection reset by peer".to_string(),
            },
        );
        let policy = BackendRetryPolicy::new(
            3,
            vec![Duration::ZERO, Duration::ZERO],
            Duration::from_secs(120),
        );
        let mut report = AgentTaskRetryReport::default();

        run_agent_task_with_retry_report(&backend, &request(), &policy, &mut report)
            .await
            .unwrap();

        assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            report.attempts()[0].failure_classification,
            Some(AgentTaskFailureClass::Disconnected)
        );
        assert!(report.attempts()[1].succeeded);
    }

    #[tokio::test]
    async fn unavailable_task_is_retried_and_classified_as_unavailable() {
        let backend = FlakyBackend::failing_with(
            1,
            AgentTaskError::Unavailable {
                reason: "codex executable was not found".to_string(),
            },
        );
        let policy = BackendRetryPolicy::new(
            3,
            vec![Duration::ZERO, Duration::ZERO],
            Duration::from_secs(120),
        );
        let mut report = AgentTaskRetryReport::default();

        run_agent_task_with_retry_report(&backend, &request(), &policy, &mut report)
            .await
            .unwrap();

        assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            report.attempts()[0].failure_classification,
            Some(AgentTaskFailureClass::Unavailable)
        );
        assert!(report.attempts()[1].succeeded);
    }
}

impl CodexAppServerBackend {
    pub fn from_automation_config(config: &AutomationConfig) -> Self {
        Self::new(config.model_id.clone(), config.timeout_secs)
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
            matches!(
                request.task,
                AgentTaskKind::SkillWriter | AgentTaskKind::CombinedReview
            )
            .then_some(&request.contract.response_schema),
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
