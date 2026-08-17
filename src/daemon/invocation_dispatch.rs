//! Dispatch of one authenticated daemon invocation.
//!
//! Validates multi-root payloads before they cost a project admission,
//! resolves the roots they name, and runs the invocation on the Unix and
//! portable executors.
//!
//! Semantic execution controls are admitted before project routing and bound
//! around project-open waits so route failures cannot hide cancellation or
//! deadline expiry.

use super::project_open_admission::ProjectOpenWaitOutcome;
use super::*;
#[cfg(any(not(unix), test))]
use crate::daemon_contract::DaemonInvocationProblem;
use service::invocation::semantic_evaluation::SemanticInvocationControlV1;
use std::future::Future;
use tracedecay_runtime_core::cancellation::CancellationToken;

fn semantic_invocation_interruption_response(
    request_id: &str,
    control: Option<&SemanticInvocationControlV1>,
) -> Option<DaemonInvocationResponse> {
    control
        .and_then(|control| control.interruption(tracedecay_application::clock::now_micros()))
        .map(|problem| {
            DaemonInvocationResponse::application_problem(request_id.to_owned(), problem)
        })
}

async fn await_project_open_with_semantic_control<Output>(
    control: Option<&SemanticInvocationControlV1>,
    request_cancellation: Option<&CancellationToken>,
    project_open: impl Future<Output = Output>,
) -> std::result::Result<Output, tracedecay_application::ApplicationProblem> {
    let Some(control) = control else {
        return Ok(project_open.await);
    };
    if let Some(problem) = control.interruption(tracedecay_application::clock::now_micros()) {
        return Err(problem);
    }
    if request_cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Err(tracedecay_application::ApplicationProblem::cancelled_before_admission());
    }
    let remaining = control.remaining(tracedecay_application::clock::now_micros())?;
    let deadline = tokio::time::Instant::now()
        .checked_add(remaining)
        .ok_or_else(
            || tracedecay_application::ApplicationProblem::InvalidRequest {
                diagnostic: tracedecay_application::SafeDiagnostic {
                    code: "semantic_evaluation_deadline_out_of_range".to_owned(),
                    message: "The semantic evaluation deadline is outside the supported range"
                        .to_owned(),
                },
                retry: tracedecay_application::RetryDirective::Never,
                legal_actions: Vec::new(),
            },
        )?;
    tokio::pin!(project_open);
    tokio::select! {
        biased;
        () = async {
            match request_cancellation {
                Some(request_cancellation) => request_cancellation.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        } => {
            Err(tracedecay_application::ApplicationProblem::cancelled_before_admission())
        }
        () = tokio::time::sleep_until(deadline) => {
            Err(tracedecay_application::ApplicationProblem::TimedOut {
                stage: tracedecay_application::CancellationStage::BeforeAdmission,
                retry: tracedecay_application::RetryDirective::Never,
                legal_actions: Vec::new(),
            })
        }
        output = &mut project_open => {
            if request_cancellation.is_some_and(CancellationToken::is_cancelled) {
                Err(tracedecay_application::ApplicationProblem::cancelled_before_admission())
            } else if let Some(problem) =
                control.interruption(tracedecay_application::clock::now_micros())
            {
                Err(problem)
            } else {
                Ok(output)
            }
        }
    }
}

async fn await_lsp_project_open_upgrade(
    project_open_gates: &Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    route: &ProjectRouteKey,
    deadline: &tracedecay_application::Deadline,
    request_cancellation: &CancellationToken,
) -> ProjectOpenWaitOutcome {
    project_open_tasks(project_open_gates.as_ref())
        .await
        .wait_for_lsp_upgrade(route, deadline, request_cancellation)
        .await
}

async fn await_lsp_route_rejoin<Output>(
    deadline: &tracedecay_application::Deadline,
    request_cancellation: &CancellationToken,
    route_read: impl Future<Output = Output>,
) -> std::result::Result<Output, tracedecay_application::ApplicationProblem> {
    if request_cancellation.is_cancelled() {
        return Err(tracedecay_application::ApplicationProblem::cancelled_before_admission());
    }
    let now = tracedecay_application::clock::now_micros();
    let remaining_micros = deadline
        .expires_at
        .0
        .checked_sub(now.0)
        .filter(|remaining| *remaining > 0)
        .ok_or_else(tracedecay_application::ApplicationProblem::timed_out_before_admission)?;
    let remaining_micros = u64::try_from(remaining_micros)
        .map_err(|_| tracedecay_application::ApplicationProblem::timed_out_before_admission())?;
    let sleep = tokio::time::sleep(Duration::from_micros(remaining_micros));
    tokio::pin!(sleep);
    tokio::pin!(route_read);
    tokio::select! {
        biased;
        _ = request_cancellation.cancelled() => {
            Err(tracedecay_application::ApplicationProblem::cancelled_before_admission())
        }
        _ = &mut sleep => {
            Err(tracedecay_application::ApplicationProblem::timed_out_before_admission())
        }
        output = &mut route_read => {
            if request_cancellation.is_cancelled() {
                Err(tracedecay_application::ApplicationProblem::cancelled_before_admission())
            } else if deadline.is_elapsed_at(tracedecay_application::clock::now_micros()) {
                Err(tracedecay_application::ApplicationProblem::timed_out_before_admission())
            } else {
                Ok(output)
            }
        }
    }
}

fn lsp_project_open_wait_response(
    request_id: &str,
    outcome: ProjectOpenWaitOutcome,
    workflow_application: bool,
    git_operation: bool,
) -> Option<DaemonInvocationResponse> {
    match outcome {
        ProjectOpenWaitOutcome::Completed | ProjectOpenWaitOutcome::NotTracked => None,
        ProjectOpenWaitOutcome::Failed(error) => Some(DaemonInvocationResponse::problem(
            request_id.to_owned(),
            project_open_problem(&error, workflow_application, git_operation),
        )),
        ProjectOpenWaitOutcome::Cancelled => Some(DaemonInvocationResponse::application_problem(
            request_id.to_owned(),
            tracedecay_application::ApplicationProblem::cancelled_before_admission(),
        )),
        ProjectOpenWaitOutcome::TimedOut => Some(DaemonInvocationResponse::application_problem(
            request_id.to_owned(),
            tracedecay_application::ApplicationProblem::timed_out_before_admission(),
        )),
    }
}

/// Multi-root payloads are routed by `invoke_for_project`, which reaches the
/// executor without passing through `DaemonInvocationService::invoke`'s own
/// `validate` gate. Validating them here keeps a malformed multi-root request
/// from costing a project admission before it is rejected; authorization stays
/// with the `AuthorizedScopeSet` compare-and-swap on the executor side.
pub(super) fn invalid_multi_root_invocation_response(
    request: &DaemonInvocationRequest,
) -> Option<DaemonInvocationResponse> {
    let multi_root_payload = matches!(
        &request.payload,
        service::invocation::DaemonInvocationPayload::MultiRootScopeSetRead { .. }
            | service::invocation::DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap { .. }
            | service::invocation::DaemonInvocationPayload::MultiRootExecute { .. }
    );
    if !multi_root_payload {
        return None;
    }
    request
        .validate()
        .err()
        .map(|problem| DaemonInvocationResponse::problem(request.request_id.clone(), problem))
}

#[cfg(any(not(unix), test))]
pub(super) async fn execute_portable_daemon_invocation(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    handshake: &DaemonHandshake,
    invocation: &DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    request: DaemonInvocationRequest,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> DaemonInvocationResponse {
    if let Some(response) = invalid_multi_root_invocation_response(&request) {
        return response;
    }
    let request_id = request.request_id.clone();
    let semantic_control = SemanticInvocationControlV1::from_request(&request);
    if let Some(response) =
        semantic_invocation_interruption_response(&request_id, semantic_control.as_ref())
    {
        return response;
    }
    let semantic_cancellation_lease = if semantic_control.is_some() {
        match crate::daemon::request_cancellation::register(&request_id) {
            Some(lease) => Some(lease),
            None => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::InvalidRequest,
                );
            }
        }
    } else {
        None
    };
    let semantic_cancellation = semantic_cancellation_lease
        .as_ref()
        .map(crate::daemon::request_cancellation::Lease::token);
    let lsp_cancellation_lease =
        if request.operation() == service::invocation::DaemonInvocationOperation::LspOpen {
            match crate::daemon::request_cancellation::register(&request_id) {
                Some(lease) => Some(lease),
                None => {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::InvalidRequest,
                    );
                }
            }
        } else {
            None
        };
    let lsp_cancellation = lsp_cancellation_lease
        .as_ref()
        .map(crate::daemon::request_cancellation::Lease::token);
    let request_cancellation = semantic_cancellation.clone().or(lsp_cancellation.clone());
    let lsp_project_open_gates = Arc::clone(&project_open_gates);
    #[cfg(test)]
    let lsp_project_open_attempts = project_open_attempts.clone();
    let git_operation = invocation_is_git_operation(request.operation());
    let workflow_application = request.is_workflow_application();
    let mut project_path = None;
    if request.requires_project() {
        let project_server = await_project_open_with_semantic_control(
            semantic_control.as_ref(),
            semantic_cancellation.as_ref(),
            Box::pin(portable_project_server_for_request(
                lifecycle.clone(),
                store_administration.clone(),
                project_open_gates,
                invocation.clone(),
                http_application_registry.clone(),
                handshake,
                ProjectServerRequirement::Core,
                #[cfg(test)]
                project_open_attempts.clone(),
            )),
        )
        .await;
        let project_server = match project_server {
            Ok(project_server) => project_server,
            Err(problem) => {
                return DaemonInvocationResponse::application_problem(request_id, problem);
            }
        };
        if let Err(error) = project_server {
            return DaemonInvocationResponse::problem(
                request_id,
                project_open_problem(&error, workflow_application, git_operation),
            );
        }
        let project_route = project_route_for_handshake(handshake);
        if let Some(response) =
            semantic_invocation_interruption_response(&request_id, semantic_control.as_ref())
        {
            return response;
        }
        let Ok((mut resolved_project_path, route)) = project_route else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if let (Some((deadline, _)), Some(request_cancellation)) =
            (request.lsp_open_control(), lsp_cancellation.as_ref())
        {
            let wait = await_lsp_project_open_upgrade(
                &lsp_project_open_gates,
                &route,
                deadline,
                request_cancellation,
            )
            .await;
            if let Some(response) = lsp_project_open_wait_response(
                &request_id,
                wait,
                workflow_application,
                git_operation,
            ) {
                return response;
            }

            // Core publication may have been visible before the dependent LSP
            // owner finished. Re-enter the canonical route lookup after the
            // wait instead of carrying the pre-upgrade root/owner snapshot.
            let project_server = await_lsp_route_rejoin(
                deadline,
                request_cancellation,
                portable_project_server_for_request(
                    lifecycle.clone(),
                    store_administration.clone(),
                    lsp_project_open_gates,
                    invocation.clone(),
                    http_application_registry.clone(),
                    handshake,
                    ProjectServerRequirement::Core,
                    #[cfg(test)]
                    lsp_project_open_attempts,
                ),
            )
            .await;
            let project_server = match project_server {
                Ok(project_server) => project_server,
                Err(problem) => {
                    return DaemonInvocationResponse::application_problem(request_id, problem);
                }
            };
            if let Err(error) = project_server {
                return DaemonInvocationResponse::problem(
                    request_id,
                    project_open_problem(&error, workflow_application, git_operation),
                );
            }
            let Ok((canonical_project_path, _)) = project_route_for_handshake(handshake) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            resolved_project_path = canonical_project_path;
        }
        let admitted_root = admitted_lsp_root_for_project_path(&resolved_project_path);
        if let Some(response) =
            semantic_invocation_interruption_response(&request_id, semantic_control.as_ref())
        {
            return response;
        }
        if admitted_root.is_none() {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        project_path = Some(resolved_project_path);
    }
    invocation
        .invoke_for_project(
            &store_administration,
            project_path.as_deref(),
            request,
            request_cancellation,
        )
        .await
}

pub(super) async fn git_service_for_project_path(
    store_administration: &StoreAdministration,
    project_path: Option<&Path>,
) -> Option<git_transactions::DaemonGitInvocationOwner> {
    let project_path = project_path?;
    let repository_root = crate::worktree::git_worktree_root(project_path)
        .unwrap_or_else(|| project_path.to_path_buf());
    store_administration
        .git_index_transaction_services()
        .for_repository_root(&repository_root)
        .await
        .ok()
        .flatten()
}

pub(super) async fn native_integration_service_for_project_path(
    store_administration: &StoreAdministration,
    project_path: Option<&Path>,
) -> Option<native_integration::DaemonNativeIntegrationOwner> {
    let project_path = project_path?;
    let repository_root = crate::worktree::git_worktree_root(project_path)
        .unwrap_or_else(|| project_path.to_path_buf());
    store_administration
        .native_integration_services()
        .for_repository_root(&repository_root)
        .await
        .ok()
        .flatten()
}

#[cfg(unix)]
pub(super) async fn write_tool_list_changed_notification(
    transport: &mut impl McpTransport,
) -> Result<()> {
    let notification = json!({
        "jsonrpc": "2.0",
        "method": TOOL_LIST_CHANGED_METHOD,
    });
    transport
        .write_line(&format!("{}\n", serde_json::to_string(&notification)?))
        .await?;
    transport.flush().await?;
    Ok(())
}

pub(super) async fn resolve_multi_root_projects(
    store_administration: &StoreAdministration,
    service: &service::invocation::DaemonInvocationService,
    selectors: &[tracedecay_application::RegisteredRootSelectorV1],
) -> std::result::Result<
    Vec<(
        PathBuf,
        tracedecay_application::ResolvedScope,
        tracedecay_application::RegisteredRootLocatorV1,
    )>,
    service::invocation::DaemonInvocationProblem,
> {
    let database = store_administration
        .registered_profile_database()
        .await
        .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
    let profile_id = store_administration
        .profile_identity()
        .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?
        .profile_id()
        .clone();
    let mut roots = Vec::with_capacity(selectors.len());
    for selector in selectors {
        let context = database
            .project_registry_context_by_id(selector.project_id.as_str())
            .await
            .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?
            .ok_or(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
        if context.project.project_id != selector.project_id.as_str() {
            return Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        let mut stores = context
            .stores
            .iter()
            .filter(|store| store.store.project_id == selector.project_id.as_str());
        let Some(store) = stores.next() else {
            return Err(service::invocation::DaemonInvocationProblem::Unavailable);
        };
        if stores.next().is_some() {
            return Err(service::invocation::DaemonInvocationProblem::Unavailable);
        }
        let registered_root = PathBuf::from(context.project.canonical_root);
        if !registered_root.is_absolute()
            || registered_root.canonicalize().ok().as_ref() != Some(&registered_root)
        {
            return Err(service::invocation::DaemonInvocationProblem::Unavailable);
        }
        let root = selector
            .root
            .canonicalize()
            .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
        tracedecay_usecases::context::RegisteredScopeResolver::resolve(
            &registered_root,
            &root,
            &selector.project_id,
        )
        .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
        let scope = project_open_owners::resolved_scope_for_project(&root, &selector.project_id)
            .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
        if !service.lsp_owner_matches_scope(&root, &scope).await {
            return Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        let locator = tracedecay_application::RegisteredRootLocatorV1::new(
            selector.project_id.clone(),
            profile_id.clone(),
            store.store.store_id.clone(),
            root.clone(),
        )
        .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
        roots.push((root, scope, locator));
    }
    roots.sort_by(|left, right| left.1.scope_digest.cmp(&right.1.scope_digest));
    if roots
        .windows(2)
        .any(|pair| pair[0].1.scope_digest == pair[1].1.scope_digest)
    {
        return Err(service::invocation::DaemonInvocationProblem::InvalidRequest);
    }
    Ok(roots)
}

#[cfg(unix)]
pub(super) async fn execute_daemon_invocation(
    engine: &DaemonEngine,
    handshake: &DaemonHandshake,
    request: DaemonInvocationRequest,
) -> DaemonInvocationResponse {
    if let Some(response) = invalid_multi_root_invocation_response(&request) {
        return response;
    }
    let request_id = request.request_id.clone();
    let semantic_control = SemanticInvocationControlV1::from_request(&request);
    if let Some(response) =
        semantic_invocation_interruption_response(&request_id, semantic_control.as_ref())
    {
        return response;
    }
    let semantic_cancellation_lease = if semantic_control.is_some() {
        match crate::daemon::request_cancellation::register(&request_id) {
            Some(lease) => Some(lease),
            None => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            }
        }
    } else {
        None
    };
    let semantic_cancellation = semantic_cancellation_lease
        .as_ref()
        .map(crate::daemon::request_cancellation::Lease::token);
    let lsp_cancellation_lease =
        if request.operation() == service::invocation::DaemonInvocationOperation::LspOpen {
            match crate::daemon::request_cancellation::register(&request_id) {
                Some(lease) => Some(lease),
                None => {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        service::invocation::DaemonInvocationProblem::InvalidRequest,
                    );
                }
            }
        } else {
            None
        };
    let lsp_cancellation = lsp_cancellation_lease
        .as_ref()
        .map(crate::daemon::request_cancellation::Lease::token);
    let request_cancellation = semantic_cancellation.clone().or(lsp_cancellation.clone());
    let git_operation = invocation_is_git_operation(request.operation());
    let workflow_application = request.is_workflow_application();
    let mut project_path = None;
    if request.requires_project() {
        let project_server = await_project_open_with_semantic_control(
            semantic_control.as_ref(),
            semantic_cancellation.as_ref(),
            engine.project_server_for_request(handshake, ProjectServerRequirement::Core),
        )
        .await;
        let project_server = match project_server {
            Ok(project_server) => project_server,
            Err(problem) => {
                return DaemonInvocationResponse::application_problem(request_id, problem);
            }
        };
        if let Err(error) = project_server {
            return DaemonInvocationResponse::problem(
                request_id,
                project_open_problem(&error, workflow_application, git_operation),
            );
        }
        let project_route = DaemonEngine::project_route(handshake);
        if let Some(response) =
            semantic_invocation_interruption_response(&request_id, semantic_control.as_ref())
        {
            return response;
        }
        let Ok((mut resolved_project_path, route)) = project_route else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if let (Some((deadline, _)), Some(request_cancellation)) =
            (request.lsp_open_control(), lsp_cancellation.as_ref())
        {
            let wait = await_lsp_project_open_upgrade(
                &engine.project_open_gates,
                &route,
                deadline,
                request_cancellation,
            )
            .await;
            if let Some(response) = lsp_project_open_wait_response(
                &request_id,
                wait,
                workflow_application,
                git_operation,
            ) {
                return response;
            }

            // Core publication may have been visible before the dependent LSP
            // owner finished. Re-enter the canonical route lookup after the
            // wait instead of carrying the pre-upgrade root/owner snapshot.
            let project_server = await_lsp_route_rejoin(
                deadline,
                request_cancellation,
                engine.project_server_for_request(handshake, ProjectServerRequirement::Core),
            )
            .await;
            let project_server = match project_server {
                Ok(project_server) => project_server,
                Err(problem) => {
                    return DaemonInvocationResponse::application_problem(request_id, problem);
                }
            };
            if let Err(error) = project_server {
                return DaemonInvocationResponse::problem(
                    request_id,
                    project_open_problem(&error, workflow_application, git_operation),
                );
            }
            let Ok((canonical_project_path, _)) = DaemonEngine::project_route(handshake) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            resolved_project_path = canonical_project_path;
        }
        let admitted_root = admitted_lsp_root_for_project_path(&resolved_project_path);
        if let Some(response) =
            semantic_invocation_interruption_response(&request_id, semantic_control.as_ref())
        {
            return response;
        }
        if admitted_root.is_none() {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::Unavailable,
            );
        }
        project_path = Some(resolved_project_path);
    }
    Box::pin(engine.invocation.invoke_for_project(
        &engine.store_administration,
        project_path.as_deref(),
        request,
        request_cancellation,
    ))
    .await
}

fn project_open_problem(
    error: &crate::errors::TraceDecayError,
    workflow_application: bool,
    git_operation: bool,
) -> service::invocation::DaemonInvocationProblem {
    // A store that refuses to open until it is explicitly reset is a terminal
    // state for *every* operation, not only the workflow application. Scoping
    // this to `authority == "workflow"` sent every other caller down the
    // retryable-unavailable branch below, so a project whose relational shape
    // this binary refuses answered "the application service is unavailable,
    // retry after 250ms" — and clients dutifully retried it until their whole
    // budget was gone, never learning that the only legal action is `reset`.
    if matches!(
        error,
        crate::errors::TraceDecayError::ResetRequired { authority, .. }
            if workflow_application || authority != "workflow"
    ) {
        service::invocation::DaemonInvocationProblem::ResetRequired
    } else if crate::daemon::error_message_is_project_open_retryable(&error.to_string()) {
        // A still-warming project open (or a saturated open queue) is a
        // retryable state for every operation. Mapping it to the git branch's
        // terminal not-found/not-authorized would misreport an authorized
        // worktree that merely has not finished opening.
        service::invocation::DaemonInvocationProblem::Unavailable
    } else if git_operation {
        service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized
    } else {
        service::invocation::DaemonInvocationProblem::Unavailable
    }
}

#[cfg(test)]
mod semantic_control_tests {
    use super::*;
    use tracedecay_application::ApplicationProblemKind;

    fn active_control(deadline_offset_micros: i64) -> SemanticInvocationControlV1 {
        let observed_at = tracedecay_application::clock::now_micros();
        SemanticInvocationControlV1::new(
            observed_at,
            tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                observed_at
                    .0
                    .checked_add(deadline_offset_micros)
                    .expect("test deadline"),
            ))
            .expect("valid deadline"),
            tracedecay_application::CancellationContext::active("semantic-project-open-active")
                .expect("active cancellation"),
        )
    }

    fn cancelled_control() -> SemanticInvocationControlV1 {
        let observed_at = tracedecay_application::clock::now_micros();
        SemanticInvocationControlV1::new(
            observed_at,
            tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                observed_at.0.checked_add(1_000_000).expect("test deadline"),
            ))
            .expect("valid deadline"),
            tracedecay_application::CancellationContext::cancelled(
                "semantic-project-open-cancelled",
                observed_at,
            )
            .expect("cancelled cancellation"),
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_dispatch_admits_controls_before_and_during_project_open() {
        let cancelled = cancelled_control();
        let cancelled_problem =
            await_project_open_with_semantic_control(Some(&cancelled), None, async {
                panic!("pre-cancelled project open must not be polled");
            })
            .await
            .expect_err("pre-cancelled request");
        assert_eq!(cancelled_problem.kind(), ApplicationProblemKind::Cancelled);

        let expired = active_control(0);
        let expired_problem =
            await_project_open_with_semantic_control(Some(&expired), None, async {
                panic!("pre-expired project open must not be polled");
            })
            .await
            .expect_err("pre-expired request");
        assert_eq!(expired_problem.kind(), ApplicationProblemKind::TimedOut);

        let expiring = active_control(2_000);
        let during_open_problem = await_project_open_with_semantic_control(
            Some(&expiring),
            None,
            std::future::pending::<()>(),
        )
        .await
        .expect_err("project open must observe deadline");
        assert_eq!(during_open_problem.kind(), ApplicationProblemKind::TimedOut);

        let request_cancellation = CancellationToken::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let cancelled_open = {
            let request_cancellation = request_cancellation.clone();
            tokio::spawn(async move {
                let control = active_control(1_000_000);
                await_project_open_with_semantic_control(
                    Some(&control),
                    Some(&request_cancellation),
                    async move {
                        let _ = started_tx.send(());
                        std::future::pending::<()>().await
                    },
                )
                .await
            })
        };
        started_rx.await.expect("project open started");
        request_cancellation.cancel();
        assert_eq!(
            cancelled_open
                .await
                .expect("project-open task")
                .expect_err("request cancellation must interrupt project open")
                .kind(),
            ApplicationProblemKind::Cancelled
        );
    }

    #[tokio::test]
    async fn portable_dispatch_admits_controls_before_and_during_project_open() {
        let cancelled = cancelled_control();
        let cancelled_problem =
            await_project_open_with_semantic_control(Some(&cancelled), None, async {
                panic!("pre-cancelled project open must not be polled");
            })
            .await
            .expect_err("pre-cancelled request");
        assert_eq!(cancelled_problem.kind(), ApplicationProblemKind::Cancelled);

        let expired = active_control(0);
        let expired_problem =
            await_project_open_with_semantic_control(Some(&expired), None, async {
                panic!("pre-expired project open must not be polled");
            })
            .await
            .expect_err("pre-expired request");
        assert_eq!(expired_problem.kind(), ApplicationProblemKind::TimedOut);

        let expiring = active_control(2_000);
        let during_open_problem = await_project_open_with_semantic_control(
            Some(&expiring),
            None,
            std::future::pending::<()>(),
        )
        .await
        .expect_err("project open must observe deadline");
        assert_eq!(during_open_problem.kind(), ApplicationProblemKind::TimedOut);

        let request_cancellation = CancellationToken::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let cancelled_open = {
            let request_cancellation = request_cancellation.clone();
            tokio::spawn(async move {
                let control = active_control(1_000_000);
                await_project_open_with_semantic_control(
                    Some(&control),
                    Some(&request_cancellation),
                    async move {
                        let _ = started_tx.send(());
                        std::future::pending::<()>().await
                    },
                )
                .await
            })
        };
        started_rx.await.expect("project open started");
        request_cancellation.cancel();
        assert_eq!(
            cancelled_open
                .await
                .expect("project-open task")
                .expect_err("request cancellation must interrupt project open")
                .kind(),
            ApplicationProblemKind::Cancelled
        );
    }
}

#[cfg(test)]
mod workflow_reset_tests {
    use super::*;

    #[test]
    fn workflow_project_open_reset_remains_a_daemon_reset_problem() {
        let error =
            crate::errors::TraceDecayError::reset_required("workflow", "partial workflow schema");
        assert_eq!(
            project_open_problem(&error, true, false),
            service::invocation::DaemonInvocationProblem::ResetRequired
        );
        assert_eq!(
            project_open_problem(&error, false, false),
            service::invocation::DaemonInvocationProblem::Unavailable
        );
    }

    #[test]
    fn warming_project_open_is_retryable_unavailable_for_every_operation() {
        let warming = project_warming_error(Path::new("/tmp/surface-fixture"));
        assert_eq!(
            project_open_problem(&warming, false, true),
            service::invocation::DaemonInvocationProblem::Unavailable,
            "a warming open must not answer git reads as not found or unauthorized"
        );
        assert_eq!(
            project_open_problem(&warming, false, false),
            service::invocation::DaemonInvocationProblem::Unavailable
        );
    }

    #[test]
    fn failed_project_open_keeps_the_terminal_problem_split() {
        let failed = crate::errors::TraceDecayError::Config {
            message: "project store rejected".to_owned(),
        };
        assert_eq!(
            project_open_problem(&failed, false, true),
            service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized
        );
        assert_eq!(
            project_open_problem(&failed, false, false),
            service::invocation::DaemonInvocationProblem::Unavailable
        );
    }
}
