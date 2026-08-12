//! Project-runtime request admission and quiescence coverage.

use super::*;

fn semantic_evaluation_candidate()
-> tracedecay_usecases::semantic_runtime::SemanticEvaluationProfileCandidateV1 {
    let material = crate::search_eval::load_default_evaluated_profile_material("query-fallback")
        .expect("checked-in query fallback profile");
    tracedecay_usecases::semantic_runtime::SemanticEvaluationProfileCandidateV1 {
        evaluated_profile_id: "query-fallback".to_owned(),
        profile: tracedecay_usecases::semantic_runtime::SemanticEvaluationFusionCandidateV1 {
            profile_id: material.profile.profile_id.clone(),
            calibrations: material.profile.calibrations.clone(),
            score_domain_calibrations: material.profile.score_domain_calibrations.clone(),
            weights_micros: material.profile.weights_micros.clone(),
            diversity_policy_id: material.profile.diversity_policy_id.clone(),
            rerank_policy_id: material.profile.rerank_policy_id.clone(),
            retrieval_budget: material.profile.retrieval_budget,
        },
        diversity: tracedecay_usecases::semantic_runtime::SemanticEvaluationDiversityCandidateV1 {
            policy_id: material.diversity.policy_id.clone(),
            per_source_namespace: material.diversity.per_source_namespace,
            per_source_instance: material.diversity.per_source_instance,
            per_repository: material.diversity.per_repository,
            per_file: material.diversity.per_file,
            per_session_or_thread: material.diversity.per_session_or_thread,
            per_copy_cluster: material.diversity.per_copy_cluster,
            per_evidence_role: material.diversity.per_evidence_role,
        },
        rerank: None,
        compatibility:
            tracedecay_usecases::config::retrieval::RetrievalCompatibilityPinsV1::default(),
    }
}

#[tokio::test]
async fn project_quiescence_denies_semantic_and_git_cached_routes() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/project-quiescence-dispatch");
    DaemonLspOwnerRegistrar::new(&service)
        .register_factory(project_root.clone(), unavailable_lsp_session_factory())
        .await
        .expect("register project runtime");
    let quiescence = service
        .project_runtimes
        .quiesce_roots(&std::collections::BTreeSet::from([project_root.clone()]))
        .await
        .expect("quiesce project runtime");
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let now = current_micros();
    let deadline = Deadline::new(UtcMicros(now.0.saturating_add(30_000_000))).expect("deadline");
    let requests = [
        DaemonInvocationRequest::semantic_evaluate_and_publish(
            "request.quiesced-semantic",
            semantic_evaluation_candidate(),
            now,
            deadline.clone(),
            CancellationContext::active("cancel.quiesced-semantic").expect("cancellation"),
        ),
        DaemonInvocationRequest {
            protocol: crate::daemon_contract::DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: crate::daemon_contract::DAEMON_INVOCATION_REVISION,
            request_id: "request.quiesced-git".to_owned(),
            delivery_route: None,
            payload: DaemonInvocationPayload::GitRead {
                surface_operation:
                    crate::application_surface::ApplicationSurfaceOperation::GitStatus,
                request: crate::application_surface::GitReadSurfaceRequest {
                    request: tracedecay_usecases::git_reads::GitReadRequestV1::Status,
                    max_entries: crate::git_query::GIT_QUERY_DEFAULT_MAX_ENTRIES,
                    max_bytes: crate::git_query::GIT_QUERY_DEFAULT_MAX_BYTES,
                },
                observed_at: now,
                deadline,
                cancellation: CancellationContext::active("cancel.quiesced-git")
                    .expect("cancellation"),
            },
        },
    ];

    for request in requests {
        let response = service
            .invoke(&registry, Some(&project_root), None, None, None, request)
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::Unavailable
            }
        ));
    }

    drop(quiescence);
}
