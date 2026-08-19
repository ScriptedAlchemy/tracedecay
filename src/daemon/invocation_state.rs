//! `DaemonInvocationState`: daemon-generation-local state for the closed
//! invocation protocol, shared by the Unix and portable brokers.
//!
//! `use super::*` re-exposes the daemon authorities needed by this state (including
//! the `multi_root_family_allows` kill-switch call target) while request
//! cancellation remains threaded through the invocation boundary explicitly.

use std::sync::Arc;

use serde_json::Value;
use tracedecay_lsp::LspSessionRegistry;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1,
};

use crate::errors::{Result, TraceDecayError};

use super::service::invocation::{DaemonAdvisoryRuntimeRegistrar, DaemonRetainedRuntimeRegistrar};
use super::*;

mod project_invocation;

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
    work_federated_query_authority:
        Arc<dyn crate::daemon::work_evidence_retrieval::WorkFederatedQueryAuthorityPortV1>,
    semantic_projection_scheduler:
        tracedecay_usecases::semantic_runtime::DaemonGlobalSemanticProjectionSchedulerV1,
}

impl Default for DaemonInvocationState {
    fn default() -> Self {
        let resident_memory = Arc::new(ProcessResidentMemoryV1::new(
            DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
        ));
        let code_index_schedulers =
            code_index_scheduler::CodeIndexSchedulerRegistryV1::with_resident_memory(
                MAX_CACHED_PROJECT_SERVERS,
                Arc::clone(&resident_memory),
            );
        let service =
            DaemonInvocationService::with_code_index_schedulers(code_index_schedulers.clone());
        let query_authority_provider =
            query_authority_provider::DaemonQueryAuthorityProviderV1::default();
        let work_federated_query_authority = Arc::new(DaemonWorkFederatedQueryAuthorityV1 {
            schedulers: code_index_schedulers.clone(),
            provider: query_authority_provider.clone(),
        });
        Self {
            lsp_session_registry: Arc::new(tokio::sync::Mutex::new(
                LspSessionRegistry::default(),
            )),
            service,
            github_credential_lifecycle:
                github_credential_lifecycle::DaemonGitHubReadOnlyCredentialLifecycleV1::default(),
            code_index_schedulers,
            query_authority_provider,
            work_federated_query_authority,
            semantic_projection_scheduler:
                tracedecay_usecases::semantic_runtime::DaemonGlobalSemanticProjectionSchedulerV1::default(),
        }
    }
}

impl DaemonInvocationState {
    pub(super) fn invocation_service(&self) -> DaemonInvocationService {
        self.service.clone()
    }

    pub(super) async fn retire_remote_deleted_project(
        &self,
        profile_id: &tracedecay_domain::configuration::UserProfileId,
        project_id: &tracedecay_domain::ProjectId,
        project_roots: &std::collections::BTreeSet<std::path::PathBuf>,
    ) -> Result<()> {
        self.query_authority_provider
            .retire_project(profile_id, project_id);
        if !self
            .code_index_schedulers
            .retire_project_roots(project_roots)
            .await
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "code-index workers for remote-deleted project '{}' did not drain",
                    project_id.as_str()
                ),
            });
        }
        if !self
            .service
            .expire_project(
                &self.lsp_session_registry,
                profile_id,
                project_id,
                project_roots,
            )
            .await
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "invocation runtime owners for remote-deleted project '{}' did not drain",
                    project_id.as_str()
                ),
            });
        }
        for root in project_roots {
            // Upstream also unregistered the redundancy authority separately.
            // At this tip `unregister_project_semantic_runtime` already drops
            // the project's retained generation, redundancy state, and
            // activation gate, so one call is the whole teardown.
            tracedecay_usecases::semantic_runtime::unregister_project_semantic_runtime(root);
        }
        Ok(())
    }

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
    ) -> tracedecay_usecases::advisory::github_runtime::ProfileGitHubReadOnlyCredentialMountOutcomeV1
    {
        self.github_credential_lifecycle
            .mount(profile_id, repository_owner, repository_name)
    }

    pub(super) fn feedback_runtime_registrar(&self) -> DaemonFeedbackRuntimeRegistrar {
        DaemonFeedbackRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn advisory_runtime_registrar(&self) -> DaemonAdvisoryRuntimeRegistrar {
        DaemonAdvisoryRuntimeRegistrar::new(&self.service)
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

    pub(super) fn retained_runtime_registrar(&self) -> DaemonRetainedRuntimeRegistrar {
        DaemonRetainedRuntimeRegistrar::new(&self.service)
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
        profile_id: &tracedecay_domain::configuration::UserProfileId,
        scope: &tracedecay_application::ResolvedScope,
    ) -> std::result::Result<(), code_index_scheduler::query_runtime::QueryRuntimeMountErrorV1>
    {
        let provider = self
            .query_authority_provider
            .for_profile(profile_id.clone());
        code_index_scheduler::query_runtime::mount_query_authority_on_project_open(
            &self.code_index_schedulers,
            project_root,
            scope,
            &provider,
        )
        .await
    }

    pub(super) async fn mount_core_query_authority_for_project(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        cursor_keys: &crate::global_db::session_temporal::GlobalDbCursorKeyProvider,
    ) -> std::result::Result<(), code_index_scheduler::query_runtime::QueryRuntimeMountErrorV1>
    {
        code_index_scheduler::query_runtime::mount_core_query_authority_on_project_open(
            &self.code_index_schedulers,
            project_root,
            scope,
            cursor_keys,
        )
        .await
    }

    pub(super) fn work_federated_query_authority(
        &self,
    ) -> Arc<dyn crate::daemon::work_evidence_retrieval::WorkFederatedQueryAuthorityPortV1> {
        Arc::clone(&self.work_federated_query_authority)
    }

    pub(super) fn restore_initial_query_authority_for_project(
        &self,
        project_root: &Path,
        profile_id: tracedecay_domain::configuration::UserProfileId,
        scope: tracedecay_application::ResolvedScope,
        state: crate::config::retrieval::RetrievalProfileStateV1,
        cursor_keys: Arc<crate::global_db::session_temporal::GlobalDbCursorKeyProvider>,
    ) -> std::result::Result<
        query_authority_provider::QueryAuthorityProviderStatusV1,
        query_authority_provider::QueryAuthorityUpdateErrorV1,
    > {
        let status = self
            .query_authority_provider
            .install_evaluated_initial_state(profile_id, scope, state.clone(), cursor_keys)?;
        if !tracedecay_usecases::semantic_runtime::commit_project_initial_semantic_roots(
            project_root.to_path_buf(),
            &state,
        ) {
            return Err(
                query_authority_provider::QueryAuthorityUpdateErrorV1::ActivationNotCurrent,
            );
        }
        Ok(status)
    }

    pub(super) fn query_activation_registrar(
        &self,
        project_root: &Path,
        session_db: crate::global_db::RegisteredGlobalDbLeaseV1,
    ) -> Arc<dyn tracedecay_usecases::semantic_runtime::RetrievalProfileActivationObserverV1> {
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
        semantic_lifecycle: Option<Arc<crate::semantic_code::SemanticModelLifecycleOwnerV1>>,
        semantic_resources: Option<crate::config::SemanticResourceCeilings>,
        graph_runtime: Arc<
            crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1,
        >,
        graph_publication_database: Arc<crate::db::Database>,
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
        // The vector graph provider is registered unconditionally: retention
        // and Doctor must resolve published vectors through the mounted code
        // graph even when the semantic runtime itself is not configured.
        let vector_graph: Arc<
            dyn tracedecay_usecases::semantic_runtime::SemanticVectorGraphProviderV1,
        > = Arc::new(
            code_index_scheduler::semantic_vector_graph::DaemonSemanticVectorGraphProviderV1::new(
                project_id.clone(),
                canonical_project_root.clone(),
                self.code_index_schedulers.clone(),
                Arc::clone(&graph_runtime),
                Arc::clone(&graph_publication_database),
            ),
        );
        let semantic_schedule = semantic_runtime
            .zip(semantic_lifecycle)
            .zip(semantic_resources)
            .zip(code_index_scheduler::identity::worktree_id_for(project_root).ok())
            .map(|(((handle, lifecycle), resources), worktree_id)| {
                let graph = Arc::clone(&vector_graph);
                tracedecay_usecases::semantic_runtime::production_saved_generation_schedule_hook(
                    tracedecay_usecases::semantic_runtime::SavedGenerationScheduleHookParametersV1 {
                        project_root: project_root.to_path_buf(),
                        code_index_store_root: scoped_code_index_store_root.clone(),
                        worktree_id,
                        handle: handle.clone(),
                        graph,
                        lifecycle,
                        resources,
                        fair_scheduler: self.semantic_projection_scheduler.clone(),
                    },
                )
            });
        self.code_index_schedulers
            .mount_worktree_with_graph_runtime(
                project_id,
                project_root,
                store_root,
                semantic_schedule,
                graph_runtime,
                graph_publication_database,
            )
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("code-index scheduler could not be mounted: {error}"),
            })?;
        if !self
            .code_index_schedulers
            .install_semantic_vector_graph_provider(&canonical_project_root, vector_graph)
            .await
        {
            return Err(TraceDecayError::Config {
                message: "semantic vector graph provider could not be installed in the mounted code-index authority".to_owned(),
            });
        }
        // Canonical Plan 26 observability lane. The deferred code-index mount
        // runs after the project-open delivery mount that owns the producer;
        // an absent producer leaves the lane uninstalled and nothing records.
        match self
            .service
            .observability_producer_with_database(Some(&canonical_project_root))
            .await
        {
            Some((session_db, producer)) => {
                if let Err(error) = self
                    .code_index_schedulers
                    .install_index_observability(
                        &canonical_project_root,
                        code_index_scheduler::observability::CodeIndexObservabilityV1::new(
                            session_db, producer,
                        ),
                    )
                    .await
                {
                    tracing::warn!(
                        event = "code_index_observability_mount",
                        outcome = "unavailable",
                        error = %error,
                        "code-index observability lane could not be installed"
                    );
                }
            }
            None => {
                tracing::debug!(
                    event = "code_index_observability_mount",
                    outcome = "unavailable",
                    reason = "producer_unmounted",
                    "code-index observability lane has no mounted project producer"
                );
            }
        }
        Ok(())
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
        request_cancellation: Option<CancellationToken>,
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
                .map(|root| &root.scope().scope_digest)
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
        let mut _project_request_leases = Vec::with_capacity(scope_set.roots().len());
        for (ordinal, root) in scope_set.roots().iter().enumerate() {
            let scope = root.scope();
            if cancellation.is_cancelled()
                || request_cancellation
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
            {
                return DaemonInvocationResponse::application_problem(
                    request_id,
                    tracedecay_application::ApplicationProblem::cancelled_before_admission(),
                );
            }
            if deadline.is_elapsed_at(observed_at)
                || deadline.is_elapsed_at(tracedecay_application::clock::now_micros())
            {
                return DaemonInvocationResponse::application_problem(
                    request_id,
                    tracedecay_application::ApplicationProblem::timed_out_before_admission(),
                );
            }
            let Some(locator) = root.locator() else {
                let Ok(generation) = denied_root_generation(scope) else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        service::invocation::DaemonInvocationProblem::InvalidRequest,
                    );
                };
                generations.push(generation);
                continue;
            };
            let Some(request_lease) = self.service.admit_project_request(&locator.canonical_root)
            else {
                let Ok(generation) = unavailable_root_generation(
                    scope,
                    tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
                ) else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        service::invocation::DaemonInvocationProblem::InvalidRequest,
                    );
                };
                generations.push(generation);
                continue;
            };
            _project_request_leases.push(request_lease.clone());
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
                    request_lease,
                    request_cancellation.clone(),
                )
                .await;
            if request_cancellation
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                return DaemonInvocationResponse::application_problem(
                    request_id,
                    tracedecay_application::ApplicationProblem::cancelled_before_admission(),
                );
            }
            if deadline.is_elapsed_at(tracedecay_application::clock::now_micros()) {
                return DaemonInvocationResponse::application_problem(
                    request_id,
                    tracedecay_application::ApplicationProblem::timed_out_before_admission(),
                );
            }
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
        project_admission: crate::daemon::service::project_runtime::ProjectRuntimeRequestLeaseV1,
        request_cancellation: Option<CancellationToken>,
    ) -> std::result::Result<Value, service::invocation::DaemonInvocationProblem> {
        match operation {
            tracedecay_application::MultiRootOperationV1::Work { request } => {
                let request = serde_json::from_value::<
                    service::invocation::WorkApplicationInvocationV1,
                >(request.clone())
                .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?;
                if !matches!(
                    request,
                    service::invocation::WorkApplicationInvocationV1::Views(_)
                ) {
                    return Err(service::invocation::DaemonInvocationProblem::InvalidRequest);
                }
                let control_cancellation = tracedecay_application::CancellationSignal::active(
                    cancellation.token_id.as_str(),
                )
                .map_err(|_| service::invocation::DaemonInvocationProblem::InvalidRequest)?;
                let executor = InProcessDaemonInvocationExecutor::with_project_admission(
                    self.clone(),
                    store_administration.clone(),
                    root.to_path_buf(),
                    scope.clone(),
                    project_admission,
                    request_cancellation,
                );
                let response = crate::daemon_client::DaemonInvocationExecutor::invoke_controlled(
                    &executor,
                    DaemonInvocationRequest::work_application(
                        format!("request.multi-root.work.{ordinal}"),
                        request,
                        observed_at,
                        deadline.clone(),
                        cancellation,
                    ),
                    deadline,
                    control_cancellation,
                    crate::daemon_client::InvocationCancellationPolicy::ReadOnly,
                )
                .await
                .map_err(|_| service::invocation::DaemonInvocationProblem::Unavailable)?;
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
                    Arc::new(InProcessDaemonInvocationExecutor::with_project_admission(
                        self.clone(),
                        store_administration.clone(),
                        root.to_path_buf(),
                        scope.clone(),
                        project_admission,
                        request_cancellation,
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
        self.service.begin_shutdown().await;
        self.github_credential_lifecycle.shutdown();
        self.code_index_schedulers.shutdown().await;
        self.lsp_session_registry.lock().await.expire_at(u64::MAX);
        self.service.expire_all().await;
    }
}

#[derive(Clone)]
struct DaemonWorkFederatedQueryAuthorityV1 {
    schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
    provider: query_authority_provider::DaemonQueryAuthorityProviderV1,
}

impl crate::daemon::work_evidence_retrieval::WorkFederatedQueryAuthorityPortV1
    for DaemonWorkFederatedQueryAuthorityV1
{
    fn authority_for<'a>(
        &'a self,
        scope: &'a tracedecay_application::ResolvedScope,
    ) -> crate::daemon::work_evidence_retrieval::WorkFederatedQueryAuthorityFutureV1<'a> {
        Box::pin(async move {
            let mounted = self.schedulers.query_authority_for_scope(scope).await?;
            self.provider
                .federated_authority_for(scope, mounted.privacy_domain())
                .ok()
        })
    }
}

#[cfg(test)]
mod resident_memory_tests {
    use super::*;

    #[test]
    fn invocation_state_and_code_index_registry_share_one_process_resident_authority() {
        let state = DaemonInvocationState::default();
        let cloned = state.clone();

        assert!(Arc::ptr_eq(
            state.code_index_schedulers.resident_memory(),
            cloned.code_index_schedulers.resident_memory(),
        ));
        assert_eq!(
            state
                .code_index_schedulers
                .resident_memory()
                .snapshot()
                .limit_bytes,
            tracedecay_runtime_core::resident_memory::DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1
                .get(),
        );
    }
}
