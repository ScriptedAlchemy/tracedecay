use super::*;
use tracedecay_runtime_core::cancellation::CancellationToken;

enum SemanticExecutionInputV1 {
    Qualify(String),
    EvaluateAndPublish(String),
}

enum SemanticExecutionOutcomeV1 {
    Qualified {
        qualification: tracedecay_daemon_protocol::CanonicalQualificationBlob,
    },
    Published(Box<tracedecay_usecases::semantic_runtime::SemanticEvaluatedProfilePublicationV1>),
}

#[derive(Clone)]
pub struct SemanticInvocationControlV1 {
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
}

impl SemanticInvocationControlV1 {
    pub fn new(
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

    pub fn from_request(
        request: &tracedecay_daemon_protocol::DaemonInvocationRequest,
    ) -> Option<Self> {
        let (observed_at, deadline, cancellation) = match &request.payload {
            DaemonInvocationPayload::SemanticEvaluateAndPublish {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::SemanticQualify {
                observed_at,
                deadline,
                cancellation,
                ..
            } => (observed_at, deadline, cancellation),
            _ => return None,
        };
        Some(Self::new(
            *observed_at,
            deadline.clone(),
            cancellation.clone(),
        ))
    }

    pub fn interruption(&self, now: UtcMicros) -> Option<ApplicationProblem> {
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

    pub fn remaining(&self, now: UtcMicros) -> Result<Duration, ApplicationProblem> {
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
    #[hotpath::skip]
    pub(super) async fn configuration_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<RegisteredConfigurationRuntime> {
        self.project_runtimes.get(project_root?).await
    }

    #[hotpath::skip]
    pub(super) async fn execute_semantic_qualification(
        &self,
        project_root: Option<&Path>,
        request_id: String,
        evaluated_profile_id: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
        request_cancellation: CancellationToken,
    ) -> DaemonInvocationResponse {
        self.execute_semantic_operation(
            project_root,
            request_id,
            observed_at,
            deadline,
            cancellation,
            request_cancellation,
            SemanticExecutionInputV1::Qualify(evaluated_profile_id),
        )
        .await
    }

    #[hotpath::skip]
    pub(super) async fn execute_semantic_evaluation(
        &self,
        project_root: Option<&Path>,
        request_id: String,
        evaluated_profile_id: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
        request_cancellation: CancellationToken,
    ) -> DaemonInvocationResponse {
        self.execute_semantic_operation(
            project_root,
            request_id,
            observed_at,
            deadline,
            cancellation,
            request_cancellation,
            SemanticExecutionInputV1::EvaluateAndPublish(evaluated_profile_id),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "daemon.service.semantic.execute", future = true)]
    async fn execute_semantic_operation(
        &self,
        project_root: Option<&Path>,
        request_id: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
        request_cancellation: CancellationToken,
        input: SemanticExecutionInputV1,
    ) -> DaemonInvocationResponse {
        let control = SemanticInvocationControlV1::new(observed_at, deadline, cancellation);
        if let Some(problem) = semantic_execution_interruption(&control, &request_cancellation) {
            return application_problem(request_id, problem);
        }
        let Some(project_root) = project_root else {
            hotpath::gauge!("daemon.service.semantic.unavailable.project_root_total").inc(1_u64);
            tracing::warn!(
                event = "semantic_evaluation_admission",
                outcome = "unavailable",
                reason = "project_root",
                "semantic evaluation has no routed project root"
            );
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let registered = self.configuration_runtime(Some(project_root)).await;
        if let Some(problem) = semantic_execution_interruption(&control, &request_cancellation) {
            return application_problem(request_id, problem);
        }
        let Some(registered) = registered else {
            hotpath::gauge!("daemon.service.semantic.unavailable.configuration_runtime_total")
                .inc(1_u64);
            tracing::warn!(
                event = "semantic_evaluation_admission",
                outcome = "unavailable",
                reason = "configuration_runtime",
                "semantic evaluation configuration runtime is not registered"
            );
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        let operation = match &input {
            SemanticExecutionInputV1::Qualify(_) => None,
            SemanticExecutionInputV1::EvaluateAndPublish(_) => {
                let operation = registered.semantic_operation.get().cloned();
                if let Some(problem) =
                    semantic_execution_interruption(&control, &request_cancellation)
                {
                    return application_problem(request_id, problem);
                }
                let Some(operation) = operation else {
                    hotpath::gauge!("daemon.service.semantic.unavailable.operation_total")
                        .inc(1_u64);
                    tracing::warn!(
                        event = "semantic_evaluation_admission",
                        outcome = "unavailable",
                        reason = "operation",
                        "semantic evaluation operation is not installed"
                    );
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                Some(operation)
            }
        };
        let canonical_root = project_root.canonicalize();
        if let Some(problem) = semantic_execution_interruption(&control, &request_cancellation) {
            return application_problem(request_id, problem);
        }
        let canonical_root = match canonical_root {
            Ok(root) => root,
            Err(_) => {
                hotpath::gauge!("daemon.service.semantic.unavailable.canonical_root_total")
                    .inc(1_u64);
                tracing::warn!(
                    event = "semantic_evaluation_admission",
                    outcome = "unavailable",
                    reason = "canonical_root",
                    "semantic evaluation project root cannot be canonicalized"
                );
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
        let execution = match input {
            SemanticExecutionInputV1::Qualify(evaluated_profile_id) => {
                workers
                    .execute(worker_deadline, request_cancellation, move |control| {
                        async move {
                            let candidate = tracedecay_code_index_runtime::semantic_evaluation::build_daemon_semantic_evaluation_candidate(
                                &canonical_root,
                                &scope,
                                &scheduler,
                                &evaluated_profile_id,
                                Arc::clone(&control),
                            )
                            .await?;
                            let authority = tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationSnapshotAuthorityV1::new(
                                canonical_root.clone(),
                                scope,
                                scheduler,
                                candidate.clone(),
                                control,
                            );
                            let qualification = tracedecay_usecases::semantic_runtime::ProductionSemanticConfigurationOperationV1::qualify_profile(
                                &authority,
                                &canonical_root,
                                candidate.clone(),
                            )
                            .await?;
                            if qualification.evaluated_profile_id() != candidate.evaluated_profile_id {
                                return Err(SemanticActivationCoordinationErrorV1::RejectedDetail(
                                    format!(
                                        "semantic qualification evaluated profile {} instead of the requested {}",
                                        qualification.evaluated_profile_id(),
                                        candidate.evaluated_profile_id,
                                    ),
                                ));
                            }
                            let snapshot = qualification.snapshot().clone();
                            let validated_candidate = qualification.candidate().clone();
                            let evaluation = qualification.into_evaluation();
                            let qualification_key = semantic_qualification_key(
                                &validated_candidate,
                                &snapshot,
                                evaluation.report(),
                            )?;
                            let qualification_bytes = tracedecay_query::search_quality::encode_packaged_native_qualification(
                                evaluation,
                                qualification_key,
                            )
                            .map_err(|error| {
                                SemanticActivationCoordinationErrorV1::RejectedDetail(
                                    error.to_string(),
                                )
                            })?;
                            let qualification_bytes = tracedecay_query::search_quality::encode_daemon_native_qualification_blob(
                                &qualification_bytes,
                            )
                            .map_err(|error| {
                                SemanticActivationCoordinationErrorV1::RejectedDetail(
                                    error.to_string(),
                                )
                            })?;
                            let qualification =
                                tracedecay_daemon_protocol::CanonicalQualificationBlob::new(
                                    qualification_bytes,
                                )
                                .map_err(|error| {
                                    SemanticActivationCoordinationErrorV1::RejectedDetail(
                                        error.to_string(),
                                    )
                                })?;
                            Ok(SemanticExecutionOutcomeV1::Qualified { qualification })
                        }
                    })
                    .await
            }
            SemanticExecutionInputV1::EvaluateAndPublish(evaluated_profile_id) => {
                let Some(operation) = operation else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                };
                workers
                    .execute(worker_deadline, request_cancellation, move |control| {
                        async move {
                            let candidate = tracedecay_code_index_runtime::semantic_evaluation::build_daemon_semantic_evaluation_candidate(
                                &canonical_root,
                                &scope,
                                &scheduler,
                                &evaluated_profile_id,
                                Arc::clone(&control),
                            )
                            .await?;
                            let snapshot =
                                tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationSnapshotAuthorityV1::new(
                                canonical_root.clone(),
                                scope,
                                scheduler,
                                candidate.clone(),
                                control,
                            );
                            let authority = tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationPublicationAuthorityV1::new(snapshot);
                            operation
                                .evaluate_and_publish_profile(&authority, &canonical_root, candidate)
                                .await
                                .map(|publication| {
                                    SemanticExecutionOutcomeV1::Published(Box::new(publication))
                                })
                        }
                    })
                    .await
            }
        };
        semantic_execution_response(request_id, execution)
    }
}

fn semantic_execution_interruption(
    control: &SemanticInvocationControlV1,
    request_cancellation: &CancellationToken,
) -> Option<ApplicationProblem> {
    request_cancellation
        .is_cancelled()
        .then(|| ApplicationProblem::Cancelled {
            stage: tracedecay_application::CancellationStage::DuringRead,
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        })
        .or_else(|| control.interruption(current_micros()))
}

fn semantic_qualification_key(
    candidate: &tracedecay_usecases::semantic_runtime::SemanticEvaluationProfileCandidateV1,
    snapshot: &tracedecay_usecases::semantic_runtime::SemanticEvaluationPublicationSnapshotV1,
    report: &tracedecay_query::search_quality::DirectEvaluationReportV1,
) -> Result<
    tracedecay_query::search_quality::NativeQualificationKeyV1,
    SemanticActivationCoordinationErrorV1,
> {
    let candidate_semantic = candidate
        .compatibility
        .semantic
        .as_ref()
        .ok_or(SemanticActivationCoordinationErrorV1::Rejected)?;
    let current_semantic = snapshot
        .runtime
        .semantic
        .as_ref()
        .ok_or(SemanticActivationCoordinationErrorV1::Rejected)?;
    let candidate_model =
        tracedecay_query::search_quality::NativeQualificationModelKeyV1::from_admitted_projection(
            &candidate_semantic.projection,
        );
    let current_model =
        tracedecay_query::search_quality::NativeQualificationModelKeyV1::from_admitted_projection(
            &current_semantic.projection,
        );
    if candidate_semantic.implementation_revision != current_semantic.implementation_revision
        || candidate_semantic.fusion_revision != current_semantic.fusion_revision
        || candidate_semantic.artifact_manifest_digest != current_semantic.artifact_manifest_digest
        || candidate_semantic.runtime_compatibility_digest
            != current_semantic.runtime_compatibility_digest
        || candidate_semantic.search_index_key != current_semantic.search_index_key
        || candidate_model != current_model
    {
        return Err(SemanticActivationCoordinationErrorV1::Rejected);
    }
    Ok(
        tracedecay_query::search_quality::NativeQualificationKeyV1::new(
            report,
            candidate.evaluated_profile_id.clone(),
            tracedecay_query::search_quality::NativeQualificationRuntimeKeyV1 {
                implementation_revision: current_semantic.implementation_revision.clone(),
                fusion_revision: current_semantic.fusion_revision.clone(),
                runtime_compatibility_digest: current_semantic.runtime_compatibility_digest.clone(),
                model: current_model,
                search_index_key: current_semantic.search_index_key.clone(),
                execution_resources:
                    tracedecay_query::search_quality::NativeQualificationExecutionResourceKeyV1 {
                        model_bytes: current_semantic.resources.model_bytes,
                        tokenizer_bytes: current_semantic.resources.tokenizer_bytes,
                        threads: current_semantic.resources.threads,
                        max_concurrent_sessions: current_semantic.resources.max_concurrent_sessions,
                        batch_size: current_semantic.resources.batch_size,
                        sequence_length: current_semantic.resources.sequence_length,
                        load_deadline_ms: current_semantic.resources.load_deadline_ms,
                    },
            },
            tracedecay_query::search_quality::NativeQualificationPlatformV1::current(),
        ),
    )
}

fn semantic_execution_response(
    request_id: String,
    execution: Result<
        SemanticExecutionOutcomeV1,
        tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationExecutionErrorV1,
    >,
) -> DaemonInvocationResponse {
    match execution {
        Ok(SemanticExecutionOutcomeV1::Qualified { qualification }) => {
            DaemonInvocationResponse::with_outcome(
                request_id,
                DaemonInvocationOutcome::SemanticEvaluatedProfileQualified { qualification },
            )
        }
        Ok(SemanticExecutionOutcomeV1::Published(publication)) => {
            semantic_evaluation_response(request_id, Ok(*publication))
        }
        Err(error) => semantic_evaluation_response(request_id, Err(error)),
    }
}

fn semantic_evaluation_response(
    request_id: String,
    evaluation: Result<
        tracedecay_usecases::semantic_runtime::SemanticEvaluatedProfilePublicationV1,
        tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationExecutionErrorV1,
    >,
) -> DaemonInvocationResponse {
    use tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationExecutionErrorV1;

    match evaluation {
        Ok(publication) => {
            let report = match serde_json::to_value(&publication.report) {
                Ok(report) => report,
                Err(_) => {
                    return application_problem(
                        request_id,
                        semantic_evaluation_rejection_problem(
                            &SemanticActivationCoordinationErrorV1::Rejected,
                        ),
                    );
                }
            };
            DaemonInvocationResponse::with_outcome(
                request_id,
                DaemonInvocationOutcome::SemanticEvaluatedProfilePublished {
                    scope: publication.snapshot.scope,
                    profile_digest: publication.accepted_profile.profile_digest().clone(),
                    report_digest: publication
                        .accepted_profile
                        .evaluation()
                        .report_digest()
                        .clone(),
                    report,
                    source_generation: publication.snapshot.code_generation,
                    snapshot_digest: publication.snapshot.code_snapshot_digest,
                },
            )
        }
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
            SemanticActivationCoordinationErrorV1::Conflict,
        )) => application_problem(
            request_id,
            ApplicationProblem::Conflict {
                diagnostic: SafeDiagnostic {
                    code: "semantic_evaluation.conflict".to_owned(),
                    message: "The semantic evaluation target changed before publication".to_owned(),
                },
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![tracedecay_application::LegalAction::Refresh],
            },
        ),
        Err(DaemonSemanticEvaluationExecutionErrorV1::Coordination(
            SemanticActivationCoordinationErrorV1::Runtime(_)
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
    let collected: String = detail.chars().filter(|ch| !ch.is_control()).collect();
    let message = collected.trim();
    if message.is_empty() {
        return "semantic activation input was rejected".to_owned();
    }
    if message.len() <= MAX_MESSAGE_BYTES {
        return message.to_owned();
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
                tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationExecutionErrorV1::Coordination(
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

    #[test]
    fn indexing_cannot_be_published_as_an_empty_semantic_candidate() {
        let state = tracedecay_usecases::semantic_runtime::SemanticRuntimeStateV1::Indexing {
            completed_units: 7,
            total_units: 11,
        };

        assert_eq!(
            tracedecay_code_index_runtime::semantic_evaluation::semantic_publication_generation(
                &state
            ),
            Err(SemanticActivationCoordinationErrorV1::Unavailable)
        );
    }

    #[test]
    fn stale_publication_conflict_remains_typed_across_the_daemon_boundary() {
        let response = semantic_evaluation_response(
            "req-semantic-conflict".to_owned(),
            Err(
                tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationExecutionErrorV1::Coordination(
                    SemanticActivationCoordinationErrorV1::Conflict,
                ),
            ),
        );

        match response.outcome {
            DaemonInvocationOutcome::ApplicationProblem { problem } => {
                assert_eq!(problem.kind(), ApplicationProblemKind::Conflict);
                assert_eq!(problem.retry(), RetryDirective::AfterRevalidate);
            }
            other => panic!("expected typed conflict, got {other:?}"),
        }
    }
}
