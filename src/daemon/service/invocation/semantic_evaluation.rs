use super::*;

#[derive(Clone)]
pub(in crate::daemon) struct SemanticInvocationControlV1 {
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
}

impl SemanticInvocationControlV1 {
    pub(in crate::daemon) fn new(
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            observed_at,
            deadline,
            cancellation,
        }
    }

    pub(in crate::daemon) fn from_request(
        request: &crate::daemon_contract::DaemonInvocationRequest,
    ) -> Option<Self> {
        let DaemonInvocationPayload::SemanticEvaluateAndPublish {
            observed_at,
            deadline,
            cancellation,
            ..
        } = &request.payload
        else {
            return None;
        };
        Some(Self::new(
            *observed_at,
            deadline.clone(),
            cancellation.clone(),
        ))
    }

    pub(in crate::daemon) fn interruption(&self, now: UtcMicros) -> Option<ApplicationProblem> {
        if self.cancellation.is_cancelled() {
            return Some(ApplicationProblem::Cancelled {
                stage: tracedecay_application::CancellationStage::BeforeAdmission,
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            });
        }
        (self.deadline.is_elapsed_at(self.observed_at) || self.deadline.is_elapsed_at(now))
            .then(ApplicationProblem::timed_out_before_admission)
    }

    pub(in crate::daemon) fn remaining(
        &self,
        now: UtcMicros,
    ) -> Result<Duration, ApplicationProblem> {
        self.deadline
            .expires_at
            .0
            .checked_sub(now.0)
            .filter(|remaining| *remaining > 0)
            .map(|remaining| Duration::from_micros(remaining as u64))
            .ok_or_else(ApplicationProblem::timed_out_before_admission)
    }
}

impl DaemonInvocationService {
    pub(super) async fn configuration_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<RegisteredConfigurationRuntime> {
        self.project_runtimes.get(project_root?).await
    }

    pub(super) async fn execute_semantic_evaluation(
        &self,
        project_root: Option<&Path>,
        request_id: String,
        candidate: tracedecay_usecases::semantic_runtime::SemanticEvaluationProfileCandidateV1,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> DaemonInvocationResponse {
        let control = SemanticInvocationControlV1::new(observed_at, deadline, cancellation);
        if let Some(problem) = control.interruption(current_micros()) {
            return application_problem(request_id, problem);
        }
        let Some(project_root) = project_root else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let registered = self.configuration_runtime(Some(project_root)).await;
        if let Some(problem) = control.interruption(current_micros()) {
            return application_problem(request_id, problem);
        }
        let Some(registered) = registered else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let operation = registered.semantic_operation.get().cloned();
        if let Some(problem) = control.interruption(current_micros()) {
            return application_problem(request_id, problem);
        }
        let Some(operation) = operation else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let canonical_root = project_root.canonicalize();
        let now = current_micros();
        if let Some(problem) = control.interruption(now) {
            return application_problem(request_id, problem);
        }
        let canonical_root = match canonical_root {
            Ok(root) => root,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            }
        };
        let remaining = match control.remaining(current_micros()) {
            Ok(remaining) => remaining,
            Err(problem) => return application_problem(request_id, problem),
        };
        let Some(worker_deadline) = tokio::time::Instant::now().checked_add(remaining) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        };
        let scope = registered.scope.clone();
        let scheduler = self.code_index_schedulers.clone();
        let workers = Arc::clone(&registered.semantic_evaluation_workers);
        let evaluation = workers
            .execute(worker_deadline, move |control| {
                let authority =
                    crate::daemon::semantic_evaluation::DaemonSemanticEvaluationSnapshotAuthorityV1::new(
                        canonical_root.clone(),
                        scope,
                        scheduler,
                        candidate.clone(),
                        control,
                    );
                async move {
                    operation
                        .evaluate_and_publish_profile(&authority, &canonical_root, candidate)
                        .await
                }
            })
            .await;
        semantic_evaluation_response(request_id, evaluation)
    }
}

fn semantic_evaluation_response(
    request_id: String,
    evaluation: Result<
        tracedecay_usecases::semantic_runtime::SemanticEvaluatedProfilePublicationV1,
        crate::daemon::semantic_evaluation::DaemonSemanticEvaluationExecutionErrorV1,
    >,
) -> DaemonInvocationResponse {
    use crate::daemon::semantic_evaluation::DaemonSemanticEvaluationExecutionErrorV1;

    match evaluation {
        Ok(publication) => DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::SemanticEvaluatedProfilePublished {
                scope: publication.snapshot.scope,
                profile_digest: publication.accepted_profile.profile_digest().clone(),
                report_digest: publication
                    .accepted_profile
                    .evaluation()
                    .report_digest()
                    .clone(),
                report: publication.report,
                source_generation: publication.snapshot.code_generation,
                snapshot_digest: publication.snapshot.code_snapshot_digest,
            },
        ),
        Err(DaemonSemanticEvaluationExecutionErrorV1::Cancelled) => application_problem(
            request_id,
            ApplicationProblem::Cancelled {
                stage: tracedecay_application::CancellationStage::DuringRead,
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            },
        ),
        Err(DaemonSemanticEvaluationExecutionErrorV1::TimedOut) => application_problem(
            request_id,
            ApplicationProblem::TimedOut {
                stage: tracedecay_application::CancellationStage::DuringRead,
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            },
        ),
        Err(DaemonSemanticEvaluationExecutionErrorV1::Coordination(
            error @ (SemanticActivationCoordinationErrorV1::Rejected
            | SemanticActivationCoordinationErrorV1::RejectedDetail(_)),
        )) => application_problem(request_id, semantic_evaluation_rejection_problem(&error)),
        Err(DaemonSemanticEvaluationExecutionErrorV1::Coordination(
            SemanticActivationCoordinationErrorV1::Conflict
            | SemanticActivationCoordinationErrorV1::Runtime(_)
            | SemanticActivationCoordinationErrorV1::Unavailable,
        )) => DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable),
    }
}

fn semantic_evaluation_rejection_problem(
    error: &SemanticActivationCoordinationErrorV1,
) -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "semantic_evaluation.rejected".to_owned(),
            message: semantic_evaluation_rejection_message(&error.to_string()),
        },
        retry: RetryDirective::Never,
        legal_actions: Vec::new(),
    }
}

fn semantic_evaluation_rejection_message(detail: &str) -> String {
    const MAX_MESSAGE_BYTES: usize = 512;
    let mut message: String = detail.chars().filter(|ch| !ch.is_control()).collect();
    let trimmed = message.trim();
    if trimmed.len() != message.len() {
        message = trimmed.to_owned();
    }
    if message.is_empty() {
        return "semantic activation input was rejected".to_owned();
    }
    if message.len() <= MAX_MESSAGE_BYTES {
        return message;
    }
    let mut truncated = String::new();
    for ch in message.chars() {
        if truncated.len() + ch.len_utf8() > MAX_MESSAGE_BYTES {
            break;
        }
        truncated.push(ch);
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_during_runtime_lookup_or_canonicalization_is_typed_as_timed_out() {
        let deadline = Deadline::new(UtcMicros(200)).expect("valid deadline");
        let cancellation =
            CancellationContext::active("semantic-evaluation-active").expect("context");

        let control = SemanticInvocationControlV1::new(UtcMicros(100), deadline, cancellation);
        let checkpoint_problem = control
            .interruption(UtcMicros(200))
            .expect("post-stage expiry must interrupt");
        let remaining_problem = control
            .remaining(UtcMicros(200))
            .expect_err("expired remaining budget must fail");

        assert_eq!(checkpoint_problem.kind(), ApplicationProblemKind::TimedOut);
        assert_eq!(remaining_problem.kind(), ApplicationProblemKind::TimedOut);
    }

    #[test]
    fn cancellation_remains_distinct_at_post_lookup_checkpoints() {
        let deadline = Deadline::new(UtcMicros(300)).expect("valid deadline");
        let cancellation =
            CancellationContext::cancelled("semantic-evaluation-cancelled", UtcMicros(150))
                .expect("context");

        let control = SemanticInvocationControlV1::new(UtcMicros(100), deadline, cancellation);
        let problem = control
            .interruption(UtcMicros(200))
            .expect("cancellation must interrupt");

        assert_eq!(problem.kind(), ApplicationProblemKind::Cancelled);
    }

    #[test]
    fn rejected_evaluation_includes_search_eval_detail_in_application_problem() {
        let response = semantic_evaluation_response(
            "req-semantic-eval".to_owned(),
            Err(
                crate::daemon::semantic_evaluation::DaemonSemanticEvaluationExecutionErrorV1::Coordination(
                    SemanticActivationCoordinationErrorV1::RejectedDetail(
                        "exact eligible chunks current expected 2170, measured 2184".to_owned(),
                    ),
                ),
            ),
        );
        match response.outcome {
            DaemonInvocationOutcome::ApplicationProblem { problem } => {
                assert_eq!(problem.kind(), ApplicationProblemKind::InvalidRequest);
                let diagnostic = problem
                    .diagnostic()
                    .expect("rejected evaluation must carry a diagnostic");
                assert_eq!(diagnostic.code, "semantic_evaluation.rejected");
                assert!(
                    diagnostic.message.contains("2184"),
                    "diagnostic must include the SearchEvalError detail: {}",
                    diagnostic.message
                );
                assert!(
                    diagnostic
                        .message
                        .contains("exact eligible chunks current expected 2170"),
                    "diagnostic must include the SearchEvalError detail: {}",
                    diagnostic.message
                );
            }
            other => panic!("expected application problem, got {other:?}"),
        }
    }
}
