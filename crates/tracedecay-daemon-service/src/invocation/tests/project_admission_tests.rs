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
                    request: tracedecay_contracts::git::GitReadRequestV1::Status,
                    max_entries: tracedecay_contracts::GIT_QUERY_DEFAULT_MAX_ENTRIES,
                    max_bytes: tracedecay_contracts::GIT_QUERY_DEFAULT_MAX_BYTES,
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
            tracedecay_contracts::retrieval::StorageStatusPrimitiveRequest {
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
    let _publication = service
        .project_runtimes
        .begin_publication(&project_root)
        .expect("begin warming publication");
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
    let publication = service
        .project_runtimes
        .begin_publication(&project_root)
        .expect("begin failed publication");
    assert!(
        service
            .project_runtimes
            .mark_publication_failed(&publication)
    );
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

fn memory_status_request(request_id: &str) -> DaemonInvocationRequest {
    DaemonInvocationRequest::retained_application(
        request_id,
        tracedecay_contracts::retained_surfaces::RetainedSurfaceRequestV1::MemoryStatus(
            tracedecay_contracts::retained_surfaces::MemoryStatusRequestV1 {
                memory_scope: None,
                project_selector: None,
            },
        ),
        UtcMicros(1),
        Deadline::new(UtcMicros(30_000_000)).expect("deadline"),
        CancellationContext::active(format!("cancel.{request_id}")).expect("cancellation"),
    )
}

/// Admits `project_root` the way core-route activation does: a project
/// runtime exists, so requests pass the front door, but the retained runtime
/// that the full server's owner phase registers is not there yet.
async fn admit_project_without_retained_runtime(
    service: &DaemonInvocationService,
    project_root: &Path,
    name: &str,
) -> crate::project_runtime::ProjectRuntimePublicationAttemptV1 {
    DaemonLspOwnerRegistrar::new(service)
        .register_factory_for_project(
            project_root.to_path_buf(),
            UserProfileId::new(format!("profile.test.{name}")).expect("test LSP profile"),
            ProjectId::new(format!("project.test.{name}")).expect("test LSP project"),
            unavailable_lsp_session_factory(),
        )
        .await
        .expect("register project runtime without a retained owner");
    service
        .project_runtimes
        .begin_publication(project_root)
        .expect("begin owner publication")
}

/// The retained runtime registers in the full server's owner phase, after the
/// core route is already admitted. A retained request that lands in that
/// window must read as the owner still mounting — retryable — never as a scope
/// with no retained runtime.
#[tokio::test]
async fn retained_request_stays_retryable_while_owners_are_warming() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/projects/retained-warming");
    let _publication =
        admit_project_without_retained_runtime(&service, &project_root, "retained-warming").await;
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));

    let problem = application_problem_from(
        service
            .invoke(
                &registry,
                Some(&project_root),
                None,
                None,
                None,
                memory_status_request("request.retained.warming"),
            )
            .await,
    );

    assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
    assert_eq!(problem.terminality(), ProblemTerminality::PreAdmission);
    assert_eq!(problem.retry(), RetryDirective::AfterDelay);
    let diagnostic = problem
        .diagnostic()
        .expect("a mounting retained owner carries a diagnostic");
    assert_eq!(diagnostic.code, "application.surface.unavailable");
    assert!(
        !diagnostic
            .message
            .contains("no retained runtime is registered"),
        "a warming route must not claim its scope has no retained runtime: {}",
        diagnostic.message
    );
}

#[tokio::test]
async fn retained_request_is_terminal_after_publication_failure() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/projects/retained-failed");
    let publication =
        admit_project_without_retained_runtime(&service, &project_root, "retained-failed").await;
    assert!(
        service
            .project_runtimes
            .mark_publication_failed(&publication)
    );
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));

    let problem = application_problem_from(
        service
            .invoke(
                &registry,
                Some(&project_root),
                None,
                None,
                None,
                memory_status_request("request.retained.failed"),
            )
            .await,
    );

    assert_eq!(problem.kind(), ApplicationProblemKind::ExecutionFailed);
    assert_eq!(problem.retry(), RetryDirective::Never);
    assert_eq!(
        problem
            .diagnostic()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some("application.runtime.owner_failed")
    );
}

/// A published route that mounted no retained runtime (a read-only project
/// database mounts none) is the one case where "no retained runtime is
/// registered for this scope" is the truthful answer.
#[tokio::test]
async fn retained_request_on_a_published_route_without_a_retained_owner_is_unavailable() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/projects/retained-unmounted");
    let publication =
        admit_project_without_retained_runtime(&service, &project_root, "retained-unmounted").await;
    assert!(
        service
            .project_runtimes
            .mark_publication_ready(&publication)
    );
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));

    let problem = application_problem_from(
        service
            .invoke(
                &registry,
                Some(&project_root),
                None,
                None,
                None,
                memory_status_request("request.retained.unmounted"),
            )
            .await,
    );

    assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
    let diagnostic = problem
        .diagnostic()
        .expect("an unmounted retained owner carries a diagnostic");
    assert_eq!(
        diagnostic.code,
        "application.retained.authority-unavailable"
    );
    assert!(
        diagnostic
            .message
            .contains("no retained runtime is registered for this scope"),
        "a settled route without a retained owner must say so: {}",
        diagnostic.message
    );
}

fn retained_scope(project: &str) -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new(project).expect("retained project"),
        tracedecay_domain::RepositoryId::new("repository.retained").expect("retained repository"),
        tracedecay_domain::WorktreeId::new("worktree.retained").expect("retained worktree"),
        None,
    )
    .expect("retained scope")
}

fn retained_grant(
    scope: &ResolvedScope,
    actor: &ActorId,
    revision: u64,
) -> CapabilityGrantSnapshot {
    // The digest folds this route's own configuration revision, exactly as
    // `project_open_retained_grant` does: it is per-route provenance, not
    // store authority, so a second route legitimately carries another one.
    CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.retained.test.{revision}")).expect("grant id"),
        revision,
        ManifestDigest::new(format!("sha256:{revision:064}")).expect("grant digest"),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        std::collections::BTreeSet::from([tracedecay_tool_catalog::CapabilityId::new(
            "capability.retained.test",
        )
        .expect("capability")]),
        std::collections::BTreeSet::from([tracedecay_tool_catalog::UseCaseId::new(
            "use-case.retained.test",
        )
        .expect("use case")]),
        DisclosureClass::Sensitive,
    )
    .expect("retained grant")
}

/// Two routes of one project — a linked worktree, or a reopen of a route whose
/// ports were rebuilt — must alias one retained runtime. Keying the
/// registration on the ports object instead refused every second route.
#[tokio::test]
async fn same_authority_routes_alias_one_retained_runtime() {
    let service = DaemonInvocationService::default();
    let registrar = DaemonRetainedRuntimeRegistrar::new(&service);
    let project_root = PathBuf::from("/project-retained-alias");
    let scope = retained_scope("project.retained.alias");
    let actor = ActorId::new("actor.retained.alias").expect("retained actor");
    let incumbent_ports =
        Arc::new(tracedecay_contracts::retained_surfaces::RetainedSurfacePortsV1::default());
    let (first, second) = tokio::join!(
        registrar.register(
            project_root.clone(),
            scope.clone(),
            actor.clone(),
            retained_grant(&scope, &actor, 1),
            Arc::clone(&incumbent_ports),
        ),
        registrar.register(
            project_root.clone(),
            scope.clone(),
            actor.clone(),
            retained_grant(&scope, &actor, 2),
            Arc::new(tracedecay_contracts::retained_surfaces::RetainedSurfacePortsV1::default()),
        ),
    );
    first.expect("first same-authority route must register");
    second.expect("second same-authority route must alias the incumbent");
    let registered = service
        .project_runtimes
        .get::<RegisteredRetainedRuntime>(&project_root)
        .await
        .expect("aliased retained runtime");
    assert!(
        Arc::ptr_eq(&registered.ports, &incumbent_ports),
        "both routes must be served by the one incumbent retained runtime"
    );

    let foreign = registrar
        .register(
            project_root.clone(),
            retained_scope("project.retained.foreign"),
            actor.clone(),
            retained_grant(&retained_scope("project.retained.foreign"), &actor, 3),
            Arc::new(tracedecay_contracts::retained_surfaces::RetainedSurfacePortsV1::default()),
        )
        .await;
    assert!(
        matches!(foreign, Err(TraceDecayError::Config { ref message })
            if message == "a different retained runtime is already registered for this project"),
        "a foreign authorized scope must still be refused, not aliased: {foreign:?}"
    );
}

struct FixtureSourceEditRuntime {
    project_root: PathBuf,
    store_layout: tracedecay_runtime_core::storage::StoreLayout,
}

impl tracedecay_source_edit::SourceEditRuntimePort for FixtureSourceEditRuntime {
    fn project_root(&self) -> &Path {
        &self.project_root
    }

    fn store_layout(&self) -> &tracedecay_runtime_core::storage::StoreLayout {
        &self.store_layout
    }

    fn run_diagnostics<'a>(
        &'a self,
        _file: &'a str,
    ) -> tracedecay_source_edit::SourceEditFuture<
        'a,
        Vec<tracedecay_source_edit::EditDiagnosticRecord>,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }
}

struct UnavailableCodeGraph;

impl tracedecay_graph_query::CodeGraphProjectionReadPort for UnavailableCodeGraph {
    fn open<'a>(
        &'a self,
        _request: tracedecay_graph_query::CodeGraphReadRequest<'a>,
    ) -> tracedecay_graph_query::CodeGraphReadFuture<'a> {
        Box::pin(async { Err(tracedecay_graph_query::CodeGraphReadError::MissingRegistry) })
    }
}

/// Two routes of one project — a linked worktree, or a reopen through the
/// retained canonical runtime — each build their own source-edit owner. The
/// registration is keyed on the authorized scope, as the retained runtime is:
/// the same scope aliases the incumbent, a foreign scope is refused, and
/// neither replaces what is registered.
#[tokio::test]
async fn same_authority_source_edit_owners_alias_one_incumbent() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let directory = tempfile::TempDir::new().expect("project directory");
    let project_root = directory.path().to_path_buf();
    let profile_root =
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root");
    let project_id = ProjectId::new("project.source-edit.alias").expect("project id");
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        &profile_root,
        &project_root,
        project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let resolution = tracedecay_global_db::configuration::resolver::resolve_configuration(
        &tracedecay_global_db::configuration::registry::ConfigurationRegistry::core()
            .expect("configuration registry"),
        &[],
    )
    .expect("configuration resolution");
    let pinned = tracedecay_configuration::config::PinnedRuntimeConfiguration::new(
        tracedecay_configuration::config::RuntimeConfigurationTarget {
            project_id: project_id.clone(),
            project_root: project_root.clone(),
        },
        ConfigurationRevisionId::new("configuration-revision.source-edit.alias")
            .expect("revision id"),
        resolution.snapshot,
    )
    .expect("pinned configuration");
    let (configuration, _) = ProjectConfigurationRuntime::open(
        tracedecay_configuration::config::OpenedRuntimeConfiguration::new(
            pinned,
            runtime.project_database_arc().expect("project database"),
        ),
    )
    .expect("configuration runtime");
    let configuration = Arc::new(configuration);
    let catalog = Arc::new(
        tracedecay_contracts::catalog_composition::build_application_catalog_snapshot()
            .expect("catalog"),
    );
    let access: Arc<
        dyn tracedecay_application::source_authorization::ProjectSourceAccessSnapshotPort,
    > = Arc::new(
        tracedecay_application::project_open_authorization::ProjectOpenSourceAccessAuthorityV1::new(
            ActorId::new("actor.source-edit.alias").expect("actor"),
            std::collections::BTreeSet::new(),
            Duration::from_mins(1),
        ),
    );
    let store_layout = tracedecay_runtime_core::storage::default_profile_sharded_layout(
        &project_root,
        &profile_root,
    )
    .expect("store layout");
    let owner = |scope: ResolvedScope| {
        Arc::new(
            crate::project_owner_registration::ProjectSourceEditOwnerV1::new(
                Arc::new(FixtureSourceEditRuntime {
                    project_root: project_root.clone(),
                    store_layout: store_layout.clone(),
                }),
                Arc::new(UnavailableCodeGraph),
                crate::project_owner_registration::ProjectSourceEditAuthorizationV1::new(
                    project_root.clone(),
                    scope,
                    Arc::clone(&configuration),
                    Arc::clone(&catalog),
                    Arc::clone(&access),
                ),
                crate::project_owner_registration::SourceEditMutationGate::warming(),
            ),
        )
    };

    let service = DaemonInvocationService::default();
    let scope = retained_scope("project.source-edit.alias");
    let incumbent = owner(scope.clone());
    let (first, second) = tokio::join!(
        service.register_source_edit_owner(project_root.clone(), Arc::clone(&incumbent)),
        service.register_source_edit_owner(project_root.clone(), owner(scope.clone())),
    );
    first.expect("first same-authority route must register");
    second.expect("second same-authority route must alias the incumbent");
    let registered = service
        .project_runtimes
        .get::<Arc<crate::project_owner_registration::ProjectSourceEditOwnerV1>>(&project_root)
        .await
        .expect("aliased source-edit owner");
    assert!(
        Arc::ptr_eq(&registered, &incumbent),
        "both routes must be served by the one incumbent source-edit owner"
    );

    let foreign = service
        .register_source_edit_owner(
            project_root.clone(),
            owner(retained_scope("project.source-edit.foreign")),
        )
        .await;
    assert_eq!(
        foreign,
        Err(DaemonSourceEditOwnerRegistrationError::ForeignAuthority),
        "a foreign authorized scope must be refused, not aliased or replaced"
    );
    assert!(
        service
            .project_runtimes
            .read::<Arc<crate::project_owner_registration::ProjectSourceEditOwnerV1>, _, _>(
                &project_root,
                |served| Arc::ptr_eq(served, &incumbent),
            )
            .await
            .unwrap_or(false),
        "a refused foreign route must leave the incumbent in place"
    );
}

#[tokio::test]
async fn missing_work_owner_stops_retrying_after_publication() {
    let service = DaemonInvocationService::default();
    let root = PathBuf::from("/projects/work-unmounted");
    let publication =
        admit_project_without_retained_runtime(&service, &root, "work-unmounted").await;
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    for ready in [false, true] {
        if ready {
            assert!(
                service
                    .project_runtimes
                    .mark_publication_ready(&publication)
            );
        }
        let now = current_micros();
        let request = DaemonInvocationRequest::work_application(
            "request.work.unmounted",
            tracedecay_daemon_protocol::WorkApplicationInvocationV1::Topology(
                tracedecay_contracts::WorkTopologyViewRequestV1 {
                    page_size: 1,
                    cursor: None,
                },
            ),
            now,
            Deadline::new(UtcMicros(now.0 + 30_000_000)).unwrap(),
            CancellationContext::active("cancel.work.unmounted").unwrap(),
        );
        let problem = application_problem_from(
            service
                .invoke(&registry, Some(&root), None, None, None, request)
                .await,
        );
        if ready {
            assert_eq!(problem.retry(), RetryDirective::Never);
            assert_eq!(
                problem.kind(),
                ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never).kind()
            );
        } else {
            assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
            assert_eq!(problem.retry(), RetryDirective::AfterDelay);
        }
    }
}
