use std::sync::Arc;

use serde_json::{Value, json};

use super::context_scout_v2::{
    ContextScoutModelAssistantV1, ContextScoutModelBackendV1, ContextScoutModelCandidateV1,
    ContextScoutModelErrorV1, ContextScoutModelExecutionV1, ContextScoutModelFuture,
    ContextScoutModelProposalV1, ContextScoutModelReceiptV1, ContextScoutModelRequestV1,
    serialized_token_count,
};
use crate::automation::backend::{
    AgentTaskBackend, AgentTaskContract, AgentTaskError, AgentTaskKind, AgentTaskRequest,
    AgentTaskResponse, CodexAppServerBackend, backend_availability,
};
use crate::automation::config::{AutomationBackend, AutomationConfig};
use crate::ports::pricing::cost_of_turn;

const CONTEXT_SCOUT_PROMPT_V1: &str = "\
Select one supplied candidate and return only the JSON object required by the response schema. \
You may refine suggestion_text, but selected_dedupe_key and cited_anchor_ids must come from that \
same candidate. Do not call tools, inspect files, run commands, access credentials, mutate state, \
or add facts not present in the supplied candidates.";

pub fn context_scout_backend_from_automation_config(
    config: &AutomationConfig,
) -> ContextScoutModelBackendV1 {
    if !config.enabled {
        return ContextScoutModelBackendV1::Disabled;
    }
    match config.backend {
        AutomationBackend::Disabled => ContextScoutModelBackendV1::Disabled,
        AutomationBackend::CodexAppServer => ContextScoutModelBackendV1::CodexAppServer,
        AutomationBackend::ExternalCommand => ContextScoutModelBackendV1::Unsupported,
    }
}

pub fn context_scout_model_assistant_from_automation_config(
    config: &AutomationConfig,
) -> Arc<dyn ContextScoutModelAssistantV1> {
    let route = context_scout_backend_from_automation_config(config);
    if route != ContextScoutModelBackendV1::CodexAppServer
        || !backend_availability(config).available
    {
        return Arc::new(UnavailableContextScoutModelAssistantV1 { route });
    }
    Arc::new(ProductionContextScoutModelAssistantV1::new(
        Arc::new(CodexAppServerBackend::from_automation_config(config)),
        route,
    ))
}

pub fn context_scout_model_assistant_from_project_config(
    config: Option<&AutomationConfig>,
) -> Arc<dyn ContextScoutModelAssistantV1> {
    config.map_or_else(
        || {
            Arc::new(UnavailableContextScoutModelAssistantV1 {
                route: ContextScoutModelBackendV1::Disabled,
            }) as Arc<dyn ContextScoutModelAssistantV1>
        },
        context_scout_model_assistant_from_automation_config,
    )
}

pub struct ProductionContextScoutModelAssistantV1 {
    backend: Arc<dyn AgentTaskBackend>,
    requested_backend: ContextScoutModelBackendV1,
}

impl ProductionContextScoutModelAssistantV1 {
    pub fn new(
        backend: Arc<dyn AgentTaskBackend>,
        requested_backend: ContextScoutModelBackendV1,
    ) -> Self {
        Self {
            backend,
            requested_backend,
        }
    }
}

impl ContextScoutModelAssistantV1 for ProductionContextScoutModelAssistantV1 {
    fn backend(&self) -> ContextScoutModelBackendV1 {
        self.requested_backend
    }

    fn propose(
        &self,
        request: ContextScoutModelRequestV1,
        execution: ContextScoutModelExecutionV1,
    ) -> ContextScoutModelFuture<'_> {
        let backend = Arc::clone(&self.backend);
        let requested_backend = self.requested_backend;
        Box::pin(async move {
            execution.checkpoint()?;
            execution.validate_input(&request)?;
            let measured_input_tokens =
                serialized_token_count(&request).and_then(|tokens| u64::try_from(tokens).ok());
            let backend_request = backend_request(request, execution.max_output_tokens)?;
            let cancellation = execution.cancellation.clone();
            let deadline = tokio::time::Instant::from_std(execution.deadline.instant());
            let mut task = tokio::task::spawn_blocking(move || backend.run_task(&backend_request));
            let response = tokio::select! {
                // A join failure means the backend worker terminated without
                // producing a result: the run was lost mid-flight, which is a
                // disconnect, not an unreachable backend.
                result = &mut task => result
                    .map_err(|_| ContextScoutModelErrorV1::Disconnected)?
                    .map_err(scout_model_error_from_agent_task)?,
                () = cancellation.cancelled() => {
                    task.abort();
                    return Err(ContextScoutModelErrorV1::Cancelled);
                }
                () = tokio::time::sleep_until(deadline) => {
                    task.abort();
                    return Err(ContextScoutModelErrorV1::DeadlineExceeded);
                }
            };
            execution.checkpoint()?;
            response_to_proposal(
                response,
                requested_backend,
                measured_input_tokens,
                &execution,
            )
        })
    }
}

struct UnavailableContextScoutModelAssistantV1 {
    route: ContextScoutModelBackendV1,
}

impl ContextScoutModelAssistantV1 for UnavailableContextScoutModelAssistantV1 {
    fn backend(&self) -> ContextScoutModelBackendV1 {
        self.route
    }

    fn propose(
        &self,
        _request: ContextScoutModelRequestV1,
        _execution: ContextScoutModelExecutionV1,
    ) -> ContextScoutModelFuture<'_> {
        let error = match self.route {
            ContextScoutModelBackendV1::Disabled => ContextScoutModelErrorV1::Disabled,
            ContextScoutModelBackendV1::CodexAppServer
            | ContextScoutModelBackendV1::Unsupported => ContextScoutModelErrorV1::Unavailable,
        };
        Box::pin(async move { Err(error) })
    }
}

/// Maps each typed backend failure onto its truthful Scout model state.
/// Denial, disconnect, and unavailability stay distinct outcomes end to end;
/// only the residual untyped `Failed` state collapses to `Unavailable`
/// because the Scout taxonomy has no generic-failure outcome.
fn scout_model_error_from_agent_task(error: AgentTaskError) -> ContextScoutModelErrorV1 {
    match error {
        AgentTaskError::Denied { .. } => ContextScoutModelErrorV1::Denied,
        AgentTaskError::Disconnected { .. } => ContextScoutModelErrorV1::Disconnected,
        AgentTaskError::Unavailable { .. } => ContextScoutModelErrorV1::Unavailable,
        AgentTaskError::Timeout { .. } => ContextScoutModelErrorV1::DeadlineExceeded,
        AgentTaskError::MalformedOutput { .. } => ContextScoutModelErrorV1::InvalidOutput,
        AgentTaskError::Failed { .. } => ContextScoutModelErrorV1::Unavailable,
    }
}

fn backend_request(
    request: ContextScoutModelRequestV1,
    max_output_tokens: usize,
) -> Result<AgentTaskRequest, ContextScoutModelErrorV1> {
    let context =
        serde_json::to_value(request).map_err(|_| ContextScoutModelErrorV1::InvalidOutput)?;
    let contract = AgentTaskContract {
        task_key: "context_scout_v1".to_string(),
        prompt_version: "context-scout-model-v1".to_string(),
        response_schema: response_schema(),
        strict_json: true,
    };
    let prompt = format!(
        "{CONTEXT_SCOUT_PROMPT_V1} The maximum serialized output budget is \
         {max_output_tokens} tokens."
    );
    Ok(AgentTaskRequest::new(
        "context-scout-v1".to_string(),
        AgentTaskKind::UserJob,
        prompt,
        None,
        context,
    )
    .with_contract(contract))
}

fn response_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["selected_dedupe_key", "suggestion_text", "cited_anchor_ids"],
        "properties": {
            "selected_dedupe_key": {
                "type": "array",
                "minItems": 32,
                "maxItems": 32,
                "items": {"type": "integer", "minimum": 0, "maximum": 255}
            },
            "suggestion_text": {"type": "string"},
            "cited_anchor_ids": {
                "type": "array",
                "items": {"type": "string"}
            }
        }
    })
}

fn response_to_proposal(
    response: AgentTaskResponse,
    requested_backend: ContextScoutModelBackendV1,
    measured_input_tokens: Option<u64>,
    execution: &ContextScoutModelExecutionV1,
) -> Result<ContextScoutModelProposalV1, ContextScoutModelErrorV1> {
    let input_tokens = response.input_tokens.or(measured_input_tokens);
    let value = response
        .output_json
        .ok_or(ContextScoutModelErrorV1::InvalidOutput)?;
    let candidate: ContextScoutModelCandidateV1 =
        serde_json::from_value(value).map_err(|_| ContextScoutModelErrorV1::InvalidOutput)?;
    let measured_output_tokens =
        serialized_token_count(&candidate).and_then(|tokens| u64::try_from(tokens).ok());
    let output_tokens = response.output_tokens.or(measured_output_tokens);
    if input_tokens.is_some_and(|tokens| tokens > execution.max_input_tokens as u64)
        || output_tokens.is_some_and(|tokens| tokens > execution.max_output_tokens as u64)
    {
        return Err(ContextScoutModelErrorV1::TokenBudgetExceeded);
    }
    execution.validate_output(&candidate)?;
    let estimated_cost_microusd = estimated_cost_microusd(
        response.provider.as_deref(),
        response.model.as_deref(),
        input_tokens,
        output_tokens,
    );
    Ok(ContextScoutModelProposalV1 {
        candidate,
        receipt: ContextScoutModelReceiptV1 {
            requested_backend,
            actual_provider: response.provider,
            actual_model: response.model,
            input_tokens,
            output_tokens,
            estimated_cost_microusd,
        },
    })
}

fn estimated_cost_microusd(
    provider: Option<&str>,
    model: Option<&str>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Option<u64> {
    let cost = cost_of_turn(provider?, model?, input_tokens?, output_tokens?, 0, 0)?;
    Some((cost * 1_000_000.0).round().clamp(0.0, u64::MAX as f64) as u64)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::automation::backend::AgentTaskResponse;
    use crate::ports::context::{CancellationToken, MonotonicDeadline};

    #[derive(Clone)]
    struct RecordingBackend {
        calls: Arc<AtomicUsize>,
        request: Arc<Mutex<Option<AgentTaskRequest>>>,
    }

    impl AgentTaskBackend for RecordingBackend {
        fn run_task(
            &self,
            request: &AgentTaskRequest,
        ) -> Result<AgentTaskResponse, AgentTaskError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.request.lock().unwrap() = Some(request.clone());
            Ok(AgentTaskResponse {
                run_id: request.run_id.clone(),
                task: request.task,
                output_text: serde_json::to_string(&valid_candidate()).unwrap(),
                output_json: Some(serde_json::to_value(valid_candidate()).unwrap()),
                model: Some("gpt-5.6-test".to_string()),
                provider: Some("codex".to_string()),
                input_tokens: Some(32),
                output_tokens: Some(16),
            })
        }
    }

    #[cfg(feature = "token-counting")]
    struct MissingUsageBackend;

    #[cfg(feature = "token-counting")]
    impl AgentTaskBackend for MissingUsageBackend {
        fn run_task(
            &self,
            request: &AgentTaskRequest,
        ) -> Result<AgentTaskResponse, AgentTaskError> {
            Ok(AgentTaskResponse {
                run_id: request.run_id.clone(),
                task: request.task,
                output_text: serde_json::to_string(&valid_candidate()).unwrap(),
                output_json: Some(serde_json::to_value(valid_candidate()).unwrap()),
                model: Some("gpt-5.6-test".to_string()),
                provider: Some("codex".to_string()),
                input_tokens: None,
                output_tokens: None,
            })
        }
    }

    fn request() -> ContextScoutModelRequestV1 {
        ContextScoutModelRequestV1 {
            candidates: vec![
                super::super::context_scout_v2::ContextScoutModelCandidateInputV1 {
                    dedupe_key: [1; 32],
                    category: super::super::context_scout_v2::ContextScoutCategoryV1::Verification,
                    suggestion_text: "Run the cited focused test.".to_string(),
                    citation_anchor_ids: vec![
                        tracedecay_domain::RetrievalAnchorId::new("anchor.model.fixture").unwrap(),
                    ],
                },
            ],
        }
    }

    fn valid_candidate() -> ContextScoutModelCandidateV1 {
        ContextScoutModelCandidateV1 {
            selected_dedupe_key: [1; 32],
            suggestion_text: "Run the cited focused test.".to_string(),
            cited_anchor_ids: vec![
                tracedecay_domain::RetrievalAnchorId::new("anchor.model.fixture").unwrap(),
            ],
        }
    }

    fn execution(cancellation: CancellationToken) -> ContextScoutModelExecutionV1 {
        ContextScoutModelExecutionV1 {
            deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            cancellation,
            max_input_tokens: 2_048,
            max_output_tokens: 256,
        }
    }

    #[test]
    fn backend_mapping_fails_closed_for_disabled_and_unsupported_routes() {
        assert_eq!(
            context_scout_model_assistant_from_project_config(None).backend(),
            ContextScoutModelBackendV1::Disabled
        );
        assert_eq!(
            context_scout_backend_from_automation_config(&AutomationConfig::default()),
            ContextScoutModelBackendV1::Disabled
        );
        assert_eq!(
            context_scout_backend_from_automation_config(&AutomationConfig {
                enabled: true,
                backend: AutomationBackend::ExternalCommand,
                ..AutomationConfig::default()
            }),
            ContextScoutModelBackendV1::Unsupported
        );
        assert_eq!(
            context_scout_backend_from_automation_config(&AutomationConfig {
                enabled: true,
                backend: AutomationBackend::CodexAppServer,
                ..AutomationConfig::default()
            }),
            ContextScoutModelBackendV1::CodexAppServer
        );
    }

    #[cfg(feature = "token-counting")]
    struct TypedFailureBackend {
        error: AgentTaskError,
    }

    #[cfg(feature = "token-counting")]
    impl AgentTaskBackend for TypedFailureBackend {
        fn run_task(
            &self,
            _request: &AgentTaskRequest,
        ) -> Result<AgentTaskResponse, AgentTaskError> {
            Err(self.error.clone())
        }
    }

    #[cfg(feature = "token-counting")]
    async fn propose_with_backend_failure(
        error: AgentTaskError,
    ) -> Result<ContextScoutModelProposalV1, ContextScoutModelErrorV1> {
        let assistant = ProductionContextScoutModelAssistantV1::new(
            Arc::new(TypedFailureBackend { error }),
            ContextScoutModelBackendV1::CodexAppServer,
        );
        assistant
            .propose(request(), execution(CancellationToken::new()))
            .await
    }

    #[cfg(feature = "token-counting")]
    #[tokio::test]
    async fn denied_backend_surfaces_denied_not_unavailable() {
        assert_eq!(
            propose_with_backend_failure(AgentTaskError::Denied {
                reason: "workspace scope was denied".to_string(),
            })
            .await,
            Err(ContextScoutModelErrorV1::Denied)
        );
    }

    #[cfg(feature = "token-counting")]
    #[tokio::test]
    async fn disconnected_backend_surfaces_disconnect_not_unavailable() {
        assert_eq!(
            propose_with_backend_failure(AgentTaskError::Disconnected {
                reason: "connection reset by peer".to_string(),
            })
            .await,
            Err(ContextScoutModelErrorV1::Disconnected)
        );
    }

    #[cfg(feature = "token-counting")]
    #[tokio::test]
    async fn unavailable_backend_surfaces_unavailable() {
        assert_eq!(
            propose_with_backend_failure(AgentTaskError::Unavailable {
                reason: "codex executable was not found".to_string(),
            })
            .await,
            Err(ContextScoutModelErrorV1::Unavailable)
        );
    }

    #[tokio::test]
    async fn cancelled_request_never_enters_the_blocking_backend() {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = RecordingBackend {
            calls: Arc::clone(&calls),
            request: Arc::new(Mutex::new(None)),
        };
        let assistant = ProductionContextScoutModelAssistantV1::new(
            Arc::new(backend),
            ContextScoutModelBackendV1::CodexAppServer,
        );
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        assert_eq!(
            assistant.propose(request(), execution(cancellation)).await,
            Err(ContextScoutModelErrorV1::Cancelled)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[cfg(feature = "token-counting")]
    #[tokio::test]
    async fn production_adapter_sends_only_bounded_candidates_and_retains_usage() {
        let calls = Arc::new(AtomicUsize::new(0));
        let recorded = Arc::new(Mutex::new(None));
        let assistant = ProductionContextScoutModelAssistantV1::new(
            Arc::new(RecordingBackend {
                calls: Arc::clone(&calls),
                request: Arc::clone(&recorded),
            }),
            ContextScoutModelBackendV1::CodexAppServer,
        );

        let proposal = assistant
            .propose(request(), execution(CancellationToken::new()))
            .await
            .unwrap();

        assert_eq!(proposal.candidate, valid_candidate());
        assert_eq!(
            proposal.receipt.requested_backend,
            ContextScoutModelBackendV1::CodexAppServer
        );
        assert_eq!(
            proposal.receipt.actual_model.as_deref(),
            Some("gpt-5.6-test")
        );
        assert_eq!(proposal.receipt.input_tokens, Some(32));
        assert_eq!(proposal.receipt.output_tokens, Some(16));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let backend_request = recorded.lock().unwrap().clone().unwrap();
        assert_eq!(
            backend_request
                .context
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec!["candidates"]
        );
        assert!(backend_request.contract.strict_json);
        assert_eq!(backend_request.contract.task_key, "context_scout_v1");
        assert!(backend_request.prompt.contains("Do not call tools"));
    }

    #[cfg(feature = "token-counting")]
    #[tokio::test]
    async fn configured_model_measures_usage_when_backend_omits_token_counts() {
        let assistant = ProductionContextScoutModelAssistantV1::new(
            Arc::new(MissingUsageBackend),
            ContextScoutModelBackendV1::CodexAppServer,
        );

        let proposal = assistant
            .propose(request(), execution(CancellationToken::new()))
            .await
            .unwrap();

        assert!(
            proposal
                .receipt
                .input_tokens
                .is_some_and(|tokens| tokens > 0)
        );
        assert!(
            proposal
                .receipt
                .output_tokens
                .is_some_and(|tokens| tokens > 0)
        );
        assert!(proposal.receipt.estimated_cost_microusd.is_some());
    }
}
