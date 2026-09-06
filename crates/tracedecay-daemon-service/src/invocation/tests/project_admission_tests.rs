//! Project-runtime request admission and quiescence coverage.

use super::*;
use tracedecay_daemon_protocol::GitReadSurfaceRequest;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

#[test]
fn retained_pre_reservation_admission_preserves_cancellation_and_timeout() {
    assert!(retained_request_admission_problem(RequestAdmission::Admitted).is_none());
    for (admission, expected) in [
        (
            RequestAdmission::Cancelled,
            ApplicationProblemKind::Cancelled,
        ),
        (RequestAdmission::TimedOut, ApplicationProblemKind::TimedOut),
    ] {
        let problem = retained_request_admission_problem(admission)
            .expect("refused admission must remain a typed application problem");
        assert_eq!(problem.kind(), expected);
        assert_eq!(problem.terminality(), ProblemTerminality::PreAdmission);
        assert_eq!(
            problem.cancellation_stage(),
            Some(CancellationStage::BeforeAdmission)
        );
    }
}

#[tokio::test]
async fn project_quiescence_denies_semantic_and_git_cached_routes() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/project-quiescence-dispatch");
    DaemonLspOwnerRegistrar::new(&service)
        .register_factory_for_project(
            project_root.clone(),
            UserProfileId::new("profile.test.lsp").expect("test LSP profile"),
            ProjectId::new("project.test.lsp").expect("test LSP project"),
            unavailable_lsp_session_factory(),
        )
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
            "query-fallback".to_owned(),
            now,
            deadline.clone(),
            CancellationContext::active("cancel.quiesced-semantic").expect("cancellation"),
        ),
        // Activation refuses at the same admission gate before any lifecycle
        // read, evaluation work, or configuration effect.
        DaemonInvocationRequest::semantic_activate(
            "request.quiesced-semantic-activate",
            "query-fallback".to_owned(),
            true,
            now,
            deadline.clone(),
            CancellationContext::active("cancel.quiesced-semantic-activate").expect("cancellation"),
        ),
        DaemonInvocationRequest {
            protocol: tracedecay_daemon_protocol::DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: tracedecay_daemon_protocol::DAEMON_INVOCATION_REVISION,
            request_id: "request.quiesced-git".to_owned(),
            delivery_route: None,
            payload: DaemonInvocationPayload::GitRead {
                surface_operation: ApplicationSurfaceOperation::GitStatus,
                request: GitReadSurfaceRequest {
                    request: tracedecay_application::git::GitReadRequestV1::Status,
                    max_entries: tracedecay_usecases::git_query::GIT_QUERY_DEFAULT_MAX_ENTRIES,
                    max_bytes: tracedecay_usecases::git_query::GIT_QUERY_DEFAULT_MAX_BYTES,
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

fn storage_status_request(request_id: &str) -> DaemonInvocationRequest {
    DaemonInvocationRequest::primitive(
        request_id,
        ApplicationSurfaceOperation::StorageStatus,
        PrimitiveRequest::StorageStatus(
            tracedecay_application::retrieval::StorageStatusPrimitiveRequest {
                include_details: false,
            },
        ),
        UtcMicros(1),
        Deadline::new(UtcMicros(30_000_000)).expect("deadline"),
        CancellationContext::active(format!("cancel.{request_id}")).expect("cancellation"),
    )
}

fn application_problem_from(response: DaemonInvocationResponse) -> ApplicationProblem {
    match response.outcome {
        DaemonInvocationOutcome::ApplicationProblem { problem } => problem,
        other => panic!("expected an application problem, got {other:?}"),
    }
}

#[tokio::test]
async fn admitted_storage_status_stays_retryable_while_owners_are_warming() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/projects/storage-status-warming");
    DaemonLspOwnerRegistrar::new(&service)
        .register_factory_for_project(
            project_root.clone(),
            UserProfileId::new("profile.test.storage-status-warming").expect("test LSP profile"),
            ProjectId::new("project.test.storage-status-warming").expect("test LSP project"),
            unavailable_lsp_session_factory(),
        )
        .await
        .expect("register warming project runtime");
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));

    let problem = application_problem_from(
        service
            .invoke(
                &registry,
                Some(&project_root),
                None,
                None,
                None,
                storage_status_request("request.storage-status.warming"),
            )
            .await,
    );

    assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
    assert_eq!(problem.terminality(), ProblemTerminality::PreAdmission);
    assert_eq!(
        problem
            .diagnostic()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("application.surface.unavailable")
    );
}

#[tokio::test]
async fn admitted_storage_status_is_terminal_after_publication_failure() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/projects/storage-status-failed");
    DaemonLspOwnerRegistrar::new(&service)
        .register_factory_for_project(
            project_root.clone(),
            UserProfileId::new("profile.test.storage-status-failed").expect("test LSP profile"),
            ProjectId::new("project.test.storage-status-failed").expect("test LSP project"),
            unavailable_lsp_session_factory(),
        )
        .await
        .expect("register project runtime before publication failure");
    service
        .project_runtimes
        .mark_publication_failed(&project_root);
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));

    let problem = application_problem_from(
        service
            .invoke(
                &registry,
                Some(&project_root),
                None,
                None,
                None,
                storage_status_request("request.storage-status.failed"),
            )
            .await,
    );

    assert_eq!(problem.kind(), ApplicationProblemKind::ExecutionFailed);
    assert_eq!(problem.terminality(), ProblemTerminality::AdmittedTerminal);
    assert_eq!(problem.retry(), RetryDirective::Never);
    assert_eq!(
        problem
            .diagnostic()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("application.runtime.owner_failed")
    );
}

#[tokio::test]
async fn storage_status_admits_an_owner_registered_under_a_windows_verbatim_root() {
    let service = DaemonInvocationService::default();
    let registered = PathBuf::from(r"\\?\C:\Users\test\storage-status");
    let request = PathBuf::from(r"C:\Users\test\storage-status");
    DaemonLspOwnerRegistrar::new(&service)
        .register_factory_for_project(
            registered,
            UserProfileId::new("profile.test.storage-status-verbatim").expect("test LSP profile"),
            ProjectId::new("project.test.storage-status-verbatim").expect("test LSP project"),
            unavailable_lsp_session_factory(),
        )
        .await
        .expect("register verbatim project runtime");
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));

    let problem = application_problem_from(
        service
            .invoke(
                &registry,
                Some(&request),
                None,
                None,
                None,
                storage_status_request("request.storage-status.verbatim"),
            )
            .await,
    );

    assert_eq!(
        problem.kind(),
        ApplicationProblemKind::Unavailable,
        "the ordinary Windows spelling must admit the verbatim-registered project instead of refusing at the front door"
    );
    assert_eq!(
        problem
            .diagnostic()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("application.surface.unavailable")
    );
}
