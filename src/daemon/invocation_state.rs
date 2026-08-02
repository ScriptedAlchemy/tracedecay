//! `DaemonInvocationState`: daemon-generation-local state for the closed
//! invocation protocol, shared by the Unix and portable brokers.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic,
//! signatures, or behavior changed. `use super::*` re-exposes every name the
//! parent `daemon` module had in scope (including the `multi_root_family_allows`
//! kill-switch call target) so the moved code resolves unchanged.

use std::sync::Arc;

use serde_json::Value;
use tracedecay_lsp::LspSessionRegistry;

use crate::errors::{Result, TraceDecayError};

use super::*;

/// Daemon-generation-local state for the closed invocation protocol.
///
/// The Unix and portable brokers share this state so an authenticated LSP
/// session remains daemon-owned across client connections until it is detached
/// or expires.
#[derive(Clone)]
pub(super) struct DaemonInvocationState {
    pub(super) lsp_session_registry: Arc<tokio::sync::Mutex<LspSessionRegistry>>,
    pub(super) service: DaemonInvocationService,
    pub(super) github_credential_lifecycle:
        github_credential_lifecycle::DaemonGitHubReadOnlyCredentialLifecycleV1,
    pub(super) code_index_schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
    query_authority_provider: query_authority_provider::DaemonQueryAuthorityProviderV1,
    semantic_projection_scheduler:
        crate::application::semantic_runtime::DaemonGlobalSemanticProjectionSchedulerV1,
}

impl Default for DaemonInvocationState {
    fn default() -> Self {
        let code_index_schedulers =
            code_index_scheduler::CodeIndexSchedulerRegistryV1::new(MAX_CACHED_PROJECT_SERVERS);
        let service =
            DaemonInvocationService::with_code_index_schedulers(code_index_schedulers.clone());
        Self {
            lsp_session_registry: Arc::new(tokio::sync::Mutex::new(
                LspSessionRegistry::default(),
            )),
            service,
            github_credential_lifecycle:
                github_credential_lifecycle::DaemonGitHubReadOnlyCredentialLifecycleV1::default(),
            code_index_schedulers,
            query_authority_provider:
                query_authority_provider::DaemonQueryAuthorityProviderV1::default(),
            semantic_projection_scheduler:
                crate::application::semantic_runtime::DaemonGlobalSemanticProjectionSchedulerV1::default(),
        }
    }
}

impl DaemonInvocationState {
    pub(super) fn configure_github_read_only_credentials(
        &self,
        identity: &profile_identity::LocalProfileIdentityAuthorityV1,
    ) {
        self.github_credential_lifecycle.configure_profile(identity);
    }

    pub(super) fn mount_github_read_only_credential_authority_for_project(
        &self,
        profile_id: &tracedecay_domain::UserProfileId,
        repository_owner: &str,
        repository_name: &str,
    ) -> crate::application::advisory::github_runtime::ProfileGitHubReadOnlyCredentialMountOutcomeV1
    {
        self.github_credential_lifecycle
            .mount(profile_id, repository_owner, repository_name)
    }

    pub(super) fn advisory_runtime_registrar(&self) -> DaemonAdvisoryRuntimeRegistrar {
        DaemonAdvisoryRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn feedback_runtime_registrar(&self) -> DaemonFeedbackRuntimeRegistrar {
        DaemonFeedbackRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn context_scout_runtime_registrar(&self) -> DaemonContextScoutRuntimeRegistrar {
        DaemonContextScoutRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn primitive_runtime_registrar(&self) -> DaemonPrimitiveRuntimeRegistrar {
        DaemonPrimitiveRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn configuration_runtime_registrar(&self) -> DaemonConfigurationRuntimeRegistrar {
        DaemonConfigurationRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn work_runtime_registrar(&self) -> DaemonWorkRuntimeRegistrar {
        DaemonWorkRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn semantic_runtime_registrar(&self) -> DaemonSemanticRuntimeRegistrar {
        DaemonSemanticRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn lsp_owner_registrar(&self) -> DaemonLspOwnerRegistrar {
        DaemonLspOwnerRegistrar::new(&self.service)
    }

    pub(super) async fn mount_query_authority_for_project(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
    ) -> std::result::Result<(), code_index_scheduler::query_runtime::QueryRuntimeMountErrorV1>
    {
        code_index_scheduler::query_runtime::mount_query_authority_on_project_open(
            &self.code_index_schedulers,
            project_root,
            scope,
            &self.query_authority_provider,
        )
        .await
    }

    pub(super) fn restore_initial_query_authority_for_project(
        &self,
        scope: tracedecay_application::ResolvedScope,
        state: crate::config::retrieval::RetrievalProfileStateV1,
        cursor_keys: Arc<crate::global_db::session_temporal::GlobalDbCursorKeyProvider>,
    ) -> std::result::Result<
        query_authority_provider::QueryAuthorityProviderStatusV1,
        query_authority_provider::QueryAuthorityUpdateErrorV1,
    > {
        self.query_authority_provider
            .install_evaluated_initial_state(scope, state, cursor_keys)
    }

    pub(super) fn query_activation_registrar(
        &self,
        project_root: &Path,
        session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    ) -> Arc<dyn crate::application::semantic_runtime::RetrievalProfileActivationObserverV1> {
        Arc::new(
            query_authority_provider::DaemonQueryActivationRegistrarV1::new(
                self.query_authority_provider.clone(),
                self.code_index_schedulers.clone(),
                project_root.to_path_buf(),
                session_db,
            ),
        )
    }

    pub(super) async fn mount_code_index(
        &self,
        project_id: tracedecay_domain::ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        semantic_runtime: Option<&crate::semantic_code::DaemonSemanticRuntimeHandleV1>,
        semantic_database: Option<Arc<crate::db::Database>>,
        semantic_lifecycle: Option<Arc<crate::semantic_code::SemanticModelLifecycleOwnerV1>>,
        semantic_resources: Option<crate::config::SemanticResourceCeilings>,
    ) -> Result<()> {
        // Code-index identity is anchored on the project root's own git
        // repository (`IndexingIdentityV1::resolve` uses `gix::open` on the
        // root, no upward discovery). A non-git project has no code-index
        // identity by design: skip mounting instead of failing project open —
        // every non-code-index surface stays available.
        let git_control = project_root.join(".git");
        if !git_control.is_dir() && !git_control.is_file() {
            tracing::warn!(
                event = "code_index_mount",
                outcome = "skipped",
                project = %project_root.display(),
                reason = "missing project-root .git control path",
                "project root is not a git repository; code index disabled"
            );
            return Ok(());
        }
        let canonical_project_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let scoped_code_index_store_root = code_index_scheduler::scoped_code_index_store_root(
            &store_root,
            &canonical_project_root,
        );
        let semantic_schedule = semantic_runtime
            .zip(semantic_database)
            .zip(semantic_lifecycle)
            .zip(semantic_resources)
            .zip(code_index_scheduler::identity::worktree_id_for(project_root).ok())
            .map(
                |((((handle, database), lifecycle), resources), worktree_id)| {
                    crate::application::semantic_runtime::production_saved_generation_schedule_hook(
                        crate::application::semantic_runtime::SavedGenerationScheduleHookParametersV1 {
                            project_root: project_root.to_path_buf(),
                            code_index_store_root: scoped_code_index_store_root.clone(),
                            worktree_id,
                            handle: handle.clone(),
                            database,
                            lifecycle,
                            resources,
                            fair_scheduler: self.semantic_projection_scheduler.clone(),
                        },
                    )
                },
            );
        self.code_index_schedulers
            .mount_worktree(project_id, project_root, store_root, semantic_schedule)
            .await
            .map(|_| ())
            .map_err(|error| {
                // A retryable admission timeout is a busy daemon, not a broken
                // store: say so, so the caller retries instead of reopening.
                if error.is_retryable() {
                    TraceDecayError::Config {
                        message: format!(
                            "code-index scheduler is warming and could not be mounted yet: \
                             {error}"
                        ),
                    }
                } else {
                    TraceDecayError::Config {
                        message: format!("code-index scheduler could not be mounted: {error}"),
                    }
                }
            })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_multi_root_for_project(
        &self,
        store_administration: &StoreAdministration,
        active_project_root: &Path,
        request_id: String,
        request: tracedecay_application::MultiRootExecuteRequestV1,
        observed_at: tracedecay_domain::UtcMicros,
        deadline: tracedecay_application::Deadline,
        cancellation: tracedecay_application::CancellationContext,
    ) -> DaemonInvocationResponse {
        let Some(scope_set) = self
            .service
            .persisted_scope_set(active_project_root, &request.scope_set_id)
            .await
        else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if scope_set.revision() != request.scope_set_revision
            || scope_set.digest() != &request.scope_set_digest
        {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        let operation_value = match serde_json::to_value(&request.operation) {
            Ok(value) => value,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            }
        };
        let Ok(query_digest) = tracedecay_domain::canonical_sha256(&(
            "tracedecay.daemon.multi-root-query.v1",
            &operation_value,
        )) else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::InvalidRequest,
            );
        };
        let Ok(order_digest) = tracedecay_domain::canonical_sha256(&(
            "tracedecay.daemon.multi-root-order.v1",
            scope_set
                .roots()
                .iter()
                .map(|scope| &scope.scope_digest)
                .collect::<Vec<_>>(),
        )) else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::InvalidRequest,
            );
        };
        let database = match store_administration.registered_profile_database().await {
            Ok(database) => database,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                );
            }
        };
        let mut contexts = Vec::new();
        let mut generations = Vec::with_capacity(scope_set.roots().len());
        let mut outcomes = BTreeMap::new();
        for (ordinal, scope) in scope_set.roots().iter().enumerate() {
            let registry_context = match database
                .project_registry_context_by_id(scope.project_id.as_str())
                .await
            {
                Ok(context) => context,
                Err(_) => {
                    let Ok(generation) = unavailable_root_generation(
                        scope,
                        tracedecay_domain::ScopeUnavailableReasonV1::StoreUnavailable,
                    ) else {
                        return DaemonInvocationResponse::problem(
                            request_id,
                            service::invocation::DaemonInvocationProblem::InvalidRequest,
                        );
                    };
                    generations.push(generation);
                    continue;
                }
            };
            let Some(registry_context) = registry_context else {
                let Ok(generation) = denied_root_generation(scope) else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        service::invocation::DaemonInvocationProblem::InvalidRequest,
                    );
                };
                generations.push(generation);
                continue;
            };
            let root = PathBuf::from(registry_context.project.canonical_root);
            if !root.is_absolute() || root.canonicalize().ok().as_ref() != Some(&root) {
                let Ok(generation) = unavailable_root_generation(
                    scope,
                    tracedecay_domain::ScopeUnavailableReasonV1::RootMissing,
                ) else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        service::invocation::DaemonInvocationProblem::InvalidRequest,
                    );
                };
                generations.push(generation);
                continue;
            }
            let Some((context, _authority_digest)) = self
                .service
                .multi_root_query_context(&root, scope, ordinal, observed_at)
                .await
            else {
                let Ok(generation) = denied_root_generation(scope) else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        service::invocation::DaemonInvocationProblem::InvalidRequest,
                    );
                };
                generations.push(generation);
                continue;
            };
            let source_revision = if matches!(
                request.operation,
                tracedecay_application::MultiRootOperationV1::Git { .. }
            ) {
                match explicit_git_state(&root) {
                    Some(head) => head,
                    None => {
                        let Ok(generation) = unavailable_root_generation(
                            scope,
                            tracedecay_domain::ScopeUnavailableReasonV1::RootMissing,
                        ) else {
                            return DaemonInvocationResponse::problem(
                                request_id,
                                service::invocation::DaemonInvocationProblem::InvalidRequest,
                            );
                        };
                        generations.push(generation);
                        continue;
                    }
                }
            } else {
                scope.scope_digest.as_str().to_owned()
            };
            let Ok(generation) = frozen_root_generation(
                scope,
                scope_set.digest(),
                &source_revision,
                &operation_value,
            ) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            };
            let value = self
                .execute_one_multi_root_operation(
                    store_administration,
                    &root,
                    scope,
                    ordinal,
                    &request.operation,
                    observed_at,
                    deadline.clone(),
                    cancellation.clone(),
                )
                .await;
            let outcome = match value {
                Ok(value) => tracedecay_domain::ScopeOutcome::Exact(vec![value]),
                Err(service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized) => {
                    tracedecay_domain::ScopeOutcome::Denied
                }
                Err(_) => tracedecay_domain::ScopeOutcome::Unavailable {
                    reason: tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
                },
            };
            contexts.push(context);
            let Ok(generation) = tracedecay_domain::RootScopeOutcomeV1::new(
                scope.scope_digest.clone(),
                tracedecay_domain::ScopeOutcome::Exact(generation),
            ) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            };
            generations.push(generation);
            outcomes.insert(scope.scope_digest.clone(), outcome);
        }
        let Ok(capability_id) = tracedecay_tool_catalog::CapabilityId::new(
            project_open_owners::LSP_WORKSPACE_CAPABILITY_ID_V1,
        ) else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::Unavailable,
            );
        };
        let Ok(use_case_id) = tracedecay_tool_catalog::UseCaseId::new(
            project_open_owners::LSP_WORKSPACE_USE_CASE_ID_V1,
        ) else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::Unavailable,
            );
        };
        let query = tracedecay_application::MultiRootQueryRequestV1 {
            scope_set,
            contexts,
            root_generations: generations,
            capability_id,
            use_case_id,
            observed_at,
            query: operation_value,
            query_digest,
            order_digest,
            page: request.page,
            continuation: request.continuation,
        };
        let page = match self
            .service
            .execute_multi_root_query(PrecomputedMultiRootQueryPort { outcomes }, query)
        {
            Ok(page) => page,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            }
        };
        let Ok(application_request_id) = tracedecay_application::RequestId::new(request_id.clone())
        else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::InvalidRequest,
            );
        };
        let Some((scope, outcome)) = self
            .service
            .multi_root_evidence(
                active_project_root,
                application_request_id,
                "execute",
                page,
                observed_at,
                deadline,
                cancellation,
            )
            .await
        else {
            return DaemonInvocationResponse::problem(
                request_id,
                service::invocation::DaemonInvocationProblem::Unavailable,
            );
        };
        DaemonInvocationResponse::with_outcome(
            request_id,
            service::invocation::DaemonInvocationOutcome::MultiRootQueryPage { scope, outcome },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_one_multi_root_operation(
        &self,
        store_administration: &StoreAdministration,
        root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        ordinal: usize,
        operation: &tracedecay_application::MultiRootOperationV1,
        observed_at: tracedecay_domain::UtcMicros,
        deadline: tracedecay_application::Deadline,
        cancellation: tracedecay_application::CancellationContext,
    ) -> std::result::Result<Value, service::invocation::DaemonInvocationProblem> {
        match operation {
            tracedecay_application::MultiRootOperationV1::Work { request } => {
                let request = serde_json::from_value::<
                    service::invocation::WorkApplicationInvocationV1,
                >(request.clone())
                .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?;
                if !matches!(
                    request,
                    service::invocation::WorkApplicationInvocationV1::Snapshot(_)
                        | service::invocation::WorkApplicationInvocationV1::Delta(_)
                ) {
                    return Err(service::invocation::DaemonInvocationProblem::InvalidRequest);
                }
                let response = Box::pin(self.invoke_for_project(
                    store_administration,
                    Some(root),
                    DaemonInvocationRequest::work_application(
                        format!("request.multi-root.work.{ordinal}"),
                        request,
                        observed_at,
                        deadline,
                        cancellation,
                    ),
                ))
                .await;
                let service::invocation::DaemonInvocationOutcome::WorkApplication {
                    scope: actual_scope,
                    outcome,
                } = response.outcome
                else {
                    return Err(service::invocation::DaemonInvocationProblem::Unavailable);
                };
                if &actual_scope != scope {
                    return Err(
                        service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                    );
                }
                extract_work_application_payload(&outcome)
            }
            tracedecay_application::MultiRootOperationV1::Git { request }
            | tracedecay_application::MultiRootOperationV1::Feedback { request }
            | tracedecay_application::MultiRootOperationV1::Impact { request }
            | tracedecay_application::MultiRootOperationV1::Query { request } => {
                let wire = serde_json::from_value::<FederatedSurfaceRequestV1>(request.clone())
                    .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?;
                if !multi_root_family_allows(operation, wire.operation) {
                    return Err(service::invocation::DaemonInvocationProblem::InvalidRequest);
                }
                crate::application_surface::invoke_multi_root_surface_request(
                    Arc::new(InProcessDaemonInvocationExecutor::new(
                        self.clone(),
                        store_administration.clone(),
                        root.to_path_buf(),
                        scope.clone(),
                    )),
                    wire.operation,
                    tracedecay_application::RequestId::new(format!(
                        "request.multi-root.surface.{ordinal}"
                    ))
                    .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?,
                    tracedecay_application::PageRequest::new(100, None).map_err(|_| {
                        service::invocation::DaemonInvocationProblem::InvalidRequest
                    })?,
                    deadline,
                    tracedecay_application::CancellationSignal::active(
                        cancellation.token_id.as_str(),
                    )
                    .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?,
                    wire.request,
                )
                .await
                .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)
            }
        }
    }

    pub(super) async fn shutdown(&self) {
        self.github_credential_lifecycle.shutdown();
        self.code_index_schedulers.shutdown().await;
        self.lsp_session_registry.lock().await.expire_at(u64::MAX);
        self.service.expire_all().await;
    }

    pub(super) async fn invoke_for_project(
        &self,
        store_administration: &StoreAdministration,
        project_path: Option<&Path>,
        request: DaemonInvocationRequest,
    ) -> DaemonInvocationResponse {
        if let Some(response) = invalid_multi_root_invocation_response(&request) {
            return response;
        }
        let request_project_path = request.requires_project().then_some(project_path).flatten();
        if let service::invocation::DaemonInvocationPayload::MultiRootScopeSetRead {
            request: scope_set_request,
            observed_at,
            deadline,
            cancellation,
        } = &request.payload
        {
            let Some(active_project_root) = request_project_path else {
                return DaemonInvocationResponse::problem(
                    request.request_id.clone(),
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            let scope_set = self
                .service
                .persisted_scope_set(active_project_root, &scope_set_request.scope_set_id)
                .await;
            let Ok(application_request_id) =
                tracedecay_application::RequestId::new(request.request_id.clone())
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            };
            let Some((scope, outcome)) = self
                .service
                .multi_root_evidence(
                    active_project_root,
                    application_request_id,
                    "scope_set_read",
                    scope_set,
                    *observed_at,
                    deadline.clone(),
                    cancellation.clone(),
                )
                .await
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                );
            };
            return DaemonInvocationResponse::with_outcome(
                request.request_id,
                service::invocation::DaemonInvocationOutcome::MultiRootScopeSetRead {
                    scope,
                    outcome,
                },
            );
        }
        if let service::invocation::DaemonInvocationPayload::MultiRootScopeSetCompareAndSwap {
            request: scope_set_request,
            observed_at,
            deadline,
            cancellation,
        } = &request.payload
        {
            let Some(active_project_root) = request_project_path else {
                return DaemonInvocationResponse::problem(
                    request.request_id.clone(),
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            let roots = match resolve_multi_root_projects(
                store_administration,
                &self.service,
                &scope_set_request.project_ids,
            )
            .await
            {
                Ok(roots) => roots,
                Err(problem) => {
                    return DaemonInvocationResponse::problem(request.request_id.clone(), problem);
                }
            };
            let Ok(application_request_id) =
                tracedecay_application::RequestId::new(request.request_id.clone())
            else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::InvalidRequest,
                );
            };
            return match self
                .service
                .compare_and_swap_scope_set(
                    active_project_root,
                    scope_set_request.clone(),
                    roots,
                    *observed_at,
                )
                .await
            {
                Some((_scope, result)) => {
                    let Some((scope, outcome)) = self
                        .service
                        .multi_root_evidence(
                            active_project_root,
                            application_request_id,
                            "scope_set_compare_and_swap",
                            result,
                            *observed_at,
                            deadline.clone(),
                            cancellation.clone(),
                        )
                        .await
                    else {
                        return DaemonInvocationResponse::problem(
                            request.request_id,
                            service::invocation::DaemonInvocationProblem::Unavailable,
                        );
                    };
                    DaemonInvocationResponse::with_outcome(
                        request.request_id,
                        service::invocation::DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap {
                            scope,
                            outcome,
                        },
                    )
                }
                None => DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::Unavailable,
                ),
            };
        }
        if let service::invocation::DaemonInvocationPayload::MultiRootExecute {
            request: execute_request,
            observed_at,
            deadline,
            cancellation,
        } = &request.payload
        {
            let Some(active_project_root) = request_project_path else {
                return DaemonInvocationResponse::problem(
                    request.request_id,
                    service::invocation::DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            };
            return self
                .execute_multi_root_for_project(
                    store_administration,
                    active_project_root,
                    request.request_id,
                    execute_request.clone(),
                    *observed_at,
                    deadline.clone(),
                    cancellation.clone(),
                )
                .await;
        }
        let lsp_workspace =
            if request.operation() == service::invocation::DaemonInvocationOperation::LspOpen {
                match request_project_path {
                    Some(project_path) => {
                        admitted_lsp_workspace_for_request(
                            store_administration,
                            &self.service,
                            project_path,
                            &request,
                        )
                        .await
                    }
                    None => None,
                }
            } else {
                None
            };
        let git_service = if invocation_is_git_operation(request.operation()) {
            git_service_for_project_path(store_administration, request_project_path).await
        } else {
            None
        };
        self.service
            .invoke(
                &self.lsp_session_registry,
                request_project_path,
                lsp_workspace,
                git_service,
                request,
            )
            .await
    }
}
