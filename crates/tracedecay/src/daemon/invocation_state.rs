//! `DaemonInvocationState`: daemon-generation-local state for the closed
//! invocation protocol, shared by the Unix and portable brokers.
//!
//! Request cancellation stays threaded through the invocation boundary
//! explicitly, including the `multi_root_family_allows` kill-switch.

use std::sync::Arc;

use serde_json::Value;
use tracedecay_code_index_runtime::code_index_scheduler;
use tracedecay_daemon_identity::profile_identity;
use tracedecay_lsp::LspSessionRegistry;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, detected_process_resident_memory_limit_v1,
};
use tracedecay_semantic_contracts::SemanticResourceCeilings;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use tracedecay_daemon_service::{
    DaemonAdvisoryRuntimeRegistrar, DaemonConfigurationRuntimeRegistrar,
    DaemonContextScoutRuntimeRegistrar, DaemonFeedbackRuntimeRegistrar, DaemonInvocationOutcome,
    DaemonInvocationProblem, DaemonInvocationService, DaemonLspOwnerRegistrar,
    DaemonPrimitiveRuntimeRegistrar, DaemonRetainedRuntimeRegistrar,
    DaemonSemanticOwnerRuntimeRegistrar, DaemonSemanticRuntimeRegistrar,
    DaemonWorkRuntimeRegistrar, ProjectRuntimeRequestLeaseV1, ProjectRuntimeRootQuiescenceV1,
    WorkApplicationInvocationV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::*;

mod project_invocation;

/// Daemon-generation-local state for the closed invocation protocol.
///
/// The Unix and portable brokers share this state so an authenticated LSP
/// session remains daemon-owned across client connections until it is detached
/// or expires.
#[derive(Clone)]
pub(crate) struct DaemonInvocationState {
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
        Self::with_progress_producer_incarnation(1)
    }
}

impl DaemonInvocationState {
    /// Construct one daemon-generation invocation state whose dashboard
    /// progress is ordered by the existing durable daemon-authority epoch.
    pub(super) fn with_progress_producer_incarnation(producer_incarnation: u64) -> Self {
        let resident_memory = Arc::new(ProcessResidentMemoryV1::new(
            detected_process_resident_memory_limit_v1(),
        ));
        let code_index_schedulers = code_index_scheduler::CodeIndexSchedulerRegistryV1::with_resident_memory_and_progress_producer_incarnation(
            MAX_CACHED_PROJECT_SERVERS,
            Arc::clone(&resident_memory),
            producer_incarnation,
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

    pub(super) fn invocation_service(&self) -> DaemonInvocationService {
        self.service.clone()
    }

    pub(in crate::daemon) fn github_stack_coordinator(
        &self,
    ) -> Arc<tracedecay_usecases::stack_coordinator::DaemonGitHubStackCoordinatorV1> {
        self.service.github_stack_coordinator()
    }

    /// Mount the profile-owned background-worker plan before any projectless
    /// session or host-admission work can start. The exact `ProfileSessions`
    /// shard is the persisted user-profile authority; project configuration
    /// must never win this process-wide installation by opening first.
    #[hotpath::skip]
    pub(crate) async fn install_profile_worker_plan(
        &self,
        database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        profile_id: &tracedecay_domain::configuration::UserProfileId,
    ) -> Result<tracedecay_domain::configuration::CodeIndexWorkerStatusV1> {
        let configured = crate::config::read_or_initialize_profile_code_index_worker_selection(
            database, profile_id,
        )
        .await?;
        self.install_worker_selection(configured)
    }

    /// Charge one already-resolved worker selection against this daemon's own
    /// resident-memory authority. Keeping the arithmetic here means the
    /// persisted-profile path and any other admitted caller install the exact
    /// same plan for the same selection instead of re-deriving the available
    /// byte budget from a second estimator.
    pub(crate) fn install_worker_selection(
        &self,
        configured: tracedecay_domain::configuration::CodeIndexWorkerSelectionV1,
    ) -> Result<tracedecay_domain::configuration::CodeIndexWorkerStatusV1> {
        let resident_memory = self.code_index_schedulers.process_resident_memory();
        let resident_snapshot = resident_memory.snapshot();
        tracedecay_code_index::parallelism::install_worker_plan(
            configured,
            resident_snapshot
                .limit_bytes
                .saturating_sub(resident_snapshot.used_bytes),
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("code-index worker plan refused: {error}"),
        })
    }

    #[hotpath::skip]
    pub(super) async fn retire_project_runtime_owners(
        &self,
        profile_id: &tracedecay_domain::configuration::UserProfileId,
        project_id: &tracedecay_domain::ProjectId,
        project_roots: &std::collections::BTreeSet<std::path::PathBuf>,
    ) -> Result<()> {
        self.drain_project_runtime_owners(profile_id, project_id, project_roots, false)
            .await
            .map(drop)
    }

    #[hotpath::skip]
    pub(super) async fn quiesce_project_runtime_owners(
        &self,
        profile_id: &tracedecay_domain::configuration::UserProfileId,
        project_id: &tracedecay_domain::ProjectId,
        project_roots: &std::collections::BTreeSet<std::path::PathBuf>,
    ) -> Result<ProjectRuntimeRootQuiescenceV1> {
        self.drain_project_runtime_owners(profile_id, project_id, project_roots, true)
            .await?
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "invocation runtime owners for capacity-retired project '{}' did not enter quiescence",
                    project_id.as_str()
                ),
            })
    }

    #[hotpath::measure(label = "daemon.invocation_state.project_drain", future = true)]
    async fn drain_project_runtime_owners(
        &self,
        profile_id: &tracedecay_domain::configuration::UserProfileId,
        project_id: &tracedecay_domain::ProjectId,
        project_roots: &std::collections::BTreeSet<std::path::PathBuf>,
        reopenable: bool,
    ) -> Result<Option<ProjectRuntimeRootQuiescenceV1>> {
        // Bounded static transition names: a project leaves the invocation
        // runtime either by capacity quiescence (reopenable) or by terminal
        // remote-deletion retirement.
        if reopenable {
            hotpath::gauge!("daemon.invocation_state.transition.quiesce_total").inc(1_u64);
        } else {
            hotpath::gauge!("daemon.invocation_state.transition.retire_total").inc(1_u64);
        }
        let retirement_kind = if reopenable {
            "capacity-retired"
        } else {
            "remote-deleted"
        };
        self.query_authority_provider
            .retire_project(profile_id, project_id);
        let worktree_ids = project_roots
            .iter()
            .filter_map(|root| code_index_scheduler::identity::worktree_id_for(root).ok())
            .collect::<std::collections::BTreeSet<_>>();
        if !self
            .code_index_schedulers
            .retire_project_roots(project_roots)
            .await
        {
            hotpath::gauge!("daemon.invocation_state.drain.code_index_refused_total").inc(1_u64);
            return Err(TraceDecayError::Config {
                message: format!(
                    "code-index workers for {retirement_kind} project '{}' did not drain",
                    project_id.as_str()
                ),
            });
        }
        let semantic_projection_retirements = worktree_ids
            .iter()
            .map(|worktree_id| {
                self.semantic_projection_scheduler
                    .begin_worktree_retirement(worktree_id)
            })
            .collect::<Vec<_>>();
        let runtime_quiescence = if reopenable {
            Some(
                self.service
                    .quiesce_project(
                        &self.lsp_session_registry,
                        profile_id,
                        project_id,
                        project_roots,
                    )
                    .await
                    .ok_or_else(|| {
                        hotpath::gauge!("daemon.invocation_state.drain.owners_refused_total")
                            .inc(1_u64);
                        TraceDecayError::Config {
                            message: format!(
                                "invocation runtime owners for {retirement_kind} project '{}' did not drain",
                                project_id.as_str()
                            ),
                        }
                    })?,
            )
        } else {
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
                hotpath::gauge!("daemon.invocation_state.drain.owners_refused_total").inc(1_u64);
                return Err(TraceDecayError::Config {
                    message: format!(
                        "invocation runtime owners for {retirement_kind} project '{}' did not drain",
                        project_id.as_str()
                    ),
                });
            }
            None
        };
        let semantic_projection_deadline =
            tokio::time::Instant::now() + super::DAEMON_TASK_ABORT_DEADLINE;
        for retirement in semantic_projection_retirements {
            if !retirement.wait_until(semantic_projection_deadline).await {
                hotpath::gauge!("daemon.invocation_state.drain.semantic_refused_total").inc(1_u64);
                return Err(TraceDecayError::Config {
                    message: format!(
                        "semantic projection work for {retirement_kind} project '{}' did not drain",
                        project_id.as_str()
                    ),
                });
            }
        }
        for root in project_roots {
            // Upstream also unregistered the redundancy authority separately.
            // At this tip `unregister_project_semantic_runtime` already drops
            // the project's retained generation, redundancy state, and
            // activation gate, so one call is the whole teardown.
            tracedecay_usecases::semantic_runtime::unregister_project_semantic_runtime(root);
        }
        Ok(runtime_quiescence)
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

    pub(super) fn semantic_owner_runtime_registrar(&self) -> DaemonSemanticOwnerRuntimeRegistrar {
        DaemonSemanticOwnerRuntimeRegistrar::new(&self.service)
    }

    pub(super) fn lsp_owner_registrar(&self) -> DaemonLspOwnerRegistrar {
        DaemonLspOwnerRegistrar::new(&self.service)
    }

    #[hotpath::skip]
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

    #[hotpath::skip]
    pub(super) async fn mount_core_query_authority_for_project(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        cursor_keys: &tracedecay_session_temporal_store::GlobalDbCursorKeyProvider,
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

    #[hotpath::skip]
    pub(super) async fn mount_core_query_authority_for_committed_fallback(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        expected_revision: &tracedecay_domain::configuration::ConfigurationRevisionId,
        cursor_keys: &tracedecay_session_temporal_store::GlobalDbCursorKeyProvider,
    ) -> std::result::Result<(), code_index_scheduler::query_runtime::QueryRuntimeMountErrorV1>
    {
        code_index_scheduler::query_runtime::
            mount_core_query_authority_for_committed_fallback_on_project_open(
                &self.code_index_schedulers,
                project_root,
                scope,
                expected_revision,
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
        cursor_keys: Arc<tracedecay_session_temporal_store::GlobalDbCursorKeyProvider>,
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
        session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
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

    #[hotpath::measure(label = "daemon.invocation_state.code_index_mount", future = true)]
    pub(super) async fn mount_code_index(
        &self,
        project_id: tracedecay_domain::ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        semantic_runtime: Option<&tracedecay_semantic::DaemonSemanticRuntimeHandleV1>,
        semantic_lifecycle: Option<Arc<tracedecay_semantic::SemanticModelLifecycleOwnerV1>>,
        semantic_resources: Option<SemanticResourceCeilings>,
        semantic_document_composition: tracedecay_domain::EmbeddingDocumentCompositionV1,
        native_graph_activation: bool,
        graph_runtime: Arc<tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>,
        graph_publication_database: Arc<tracedecay_runtime_core::db::Database>,
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
            hotpath::gauge!("daemon.invocation_state.code_index_mount.skipped_total").inc(1_u64);
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
                graph_runtime.code_graph_seat_port(),
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
                        document_composition: semantic_document_composition,
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
                graph_runtime.code_graph_seat_port(),
                graph_publication_database,
                code_index_scheduler::CodeGraphActivationPolicyV1::from_enabled(
                    native_graph_activation,
                ),
            )
            .await
            .map_err(|error| {
                hotpath::gauge!("daemon.invocation_state.code_index_mount.failed_total").inc(1_u64);
                TraceDecayError::Config {
                    message: format!("code-index scheduler could not be mounted: {error}"),
                }
            })?;
        if !self
            .code_index_schedulers
            .install_semantic_vector_graph_provider(&canonical_project_root, vector_graph)
            .await
        {
            hotpath::gauge!("daemon.invocation_state.code_index_mount.failed_total").inc(1_u64);
            return Err(TraceDecayError::Config {
                message: "semantic vector graph provider could not be installed in the mounted code-index authority".to_owned(),
            });
        }
        // The deferred code-index mount runs after the project-open delivery
        // mount that owns the producer; an absent producer leaves the
        // observability lane uninstalled and nothing records.
        match self
            .service
            .observability_producer(Some(&canonical_project_root))
            .await
        {
            Some(producer) => {
                if let Err(error) = self
                    .code_index_schedulers
                    .install_index_observability(
                        &canonical_project_root,
                        code_index_scheduler::observability::CodeIndexObservabilityV1::new(
                            producer,
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
    #[hotpath::measure(label = "daemon.invocation_state.multi_root_execute", future = true)]
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
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if scope_set.revision() != request.scope_set_revision
            || scope_set.digest() != &request.scope_set_digest
        {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        let operation_value = match serde_json::to_value(&request.operation) {
            Ok(value) => value,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::InvalidRequest,
                );
            }
        };
        // Parse and family-validate the operation once for the whole scope
        // set; per-root execution reuses the typed value instead of
        // re-deserializing the identical operation JSON for every root. A
        // failure stays per-root below, exactly as when each root parsed it.
        let parsed_operation = parse_multi_root_operation(&request.operation);
        let Ok(query_digest) = tracedecay_domain::canonical_sha256(&(
            "tracedecay.daemon.multi-root-query.v1",
            &operation_value,
        )) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
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
                DaemonInvocationProblem::InvalidRequest,
            );
        };
        let database = match store_administration.registered_profile_database().await {
            Ok(database) => database,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            }
        };
        // Items-processed: the multi_root_execute span is inclusive over every
        // admitted root, so per-request root counts are what divide its wall
        // time into per-root service demand.
        hotpath::gauge!("daemon.invocation_state.multi_root_roots_total")
            .inc(scope_set.roots().len() as u64);
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
                        DaemonInvocationProblem::InvalidRequest,
                    );
                };
                generations.push(generation);
                continue;
            };
            let Some(request_lease) = self.service.admit_project_request_resolved(
                &locator.canonical_root,
                Some(&locator.canonical_root),
            ) else {
                let Ok(generation) = unavailable_root_generation(
                    scope,
                    tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
                ) else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::InvalidRequest,
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
                            DaemonInvocationProblem::InvalidRequest,
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
                        DaemonInvocationProblem::InvalidRequest,
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
                        DaemonInvocationProblem::InvalidRequest,
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
                        DaemonInvocationProblem::InvalidRequest,
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
                                DaemonInvocationProblem::InvalidRequest,
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
                    DaemonInvocationProblem::InvalidRequest,
                );
            };
            let value = match parsed_operation.as_ref() {
                Ok(parsed) => {
                    self.execute_one_multi_root_operation(
                        store_administration,
                        &root,
                        scope,
                        ordinal,
                        parsed,
                        observed_at,
                        deadline.clone(),
                        cancellation.clone(),
                        request_lease,
                        request_cancellation.clone(),
                    )
                    .await
                }
                Err(problem) => Err(*problem),
            };
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
                Err(DaemonInvocationProblem::NotFoundOrNotAuthorized) => {
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
                    DaemonInvocationProblem::InvalidRequest,
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
                DaemonInvocationProblem::Unavailable,
            );
        };
        let Ok(use_case_id) = tracedecay_tool_catalog::UseCaseId::new(
            project_open_owners::LSP_WORKSPACE_USE_CASE_ID_V1,
        ) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
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
                    DaemonInvocationProblem::InvalidRequest,
                );
            }
        };
        let Ok(application_request_id) = tracedecay_application::RequestId::new(request_id.clone())
        else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
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
                DaemonInvocationProblem::Unavailable,
            );
        };
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::MultiRootQueryPage { scope, outcome },
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::skip]
    pub(super) async fn execute_one_multi_root_operation(
        &self,
        store_administration: &StoreAdministration,
        root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        ordinal: usize,
        operation: &ParsedMultiRootOperationV1,
        observed_at: tracedecay_domain::UtcMicros,
        deadline: tracedecay_application::Deadline,
        cancellation: tracedecay_application::CancellationContext,
        project_admission: ProjectRuntimeRequestLeaseV1,
        request_cancellation: Option<CancellationToken>,
    ) -> std::result::Result<Value, DaemonInvocationProblem> {
        match operation {
            ParsedMultiRootOperationV1::Work(request) => {
                let control_cancellation = tracedecay_application::CancellationSignal::active(
                    cancellation.token_id.as_str(),
                )
                .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
                let executor = InProcessDaemonInvocationExecutor::with_project_admission(
                    self.clone(),
                    store_administration.clone(),
                    root.to_path_buf(),
                    scope.clone(),
                    project_admission,
                    request_cancellation,
                );
                let response =
                    tracedecay_daemon_protocol::DaemonInvocationExecutor::invoke_controlled(
                        &executor,
                        DaemonInvocationRequest::work_application(
                            format!("request.multi-root.work.{ordinal}"),
                            request.as_ref().clone(),
                            observed_at,
                            deadline.clone(),
                            cancellation,
                        ),
                        deadline,
                        control_cancellation,
                        tracedecay_daemon_protocol::InvocationCancellationPolicy::ReadOnly,
                    )
                    .await
                    .map_err(|_| DaemonInvocationProblem::Unavailable)?;
                let DaemonInvocationOutcome::WorkApplication {
                    scope: actual_scope,
                    outcome,
                } = response.outcome
                else {
                    return Err(DaemonInvocationProblem::Unavailable);
                };
                if &actual_scope != scope {
                    return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
                }
                extract_work_application_payload(&outcome)
            }
            ParsedMultiRootOperationV1::Surface { operation, request } => {
                crate::application_surface::invoke_multi_root_surface_request(
                    Arc::new(InProcessDaemonInvocationExecutor::with_project_admission(
                        self.clone(),
                        store_administration.clone(),
                        root.to_path_buf(),
                        scope.clone(),
                        project_admission,
                        request_cancellation,
                    )),
                    *operation,
                    tracedecay_application::RequestId::new(format!(
                        "request.multi-root.surface.{ordinal}"
                    ))
                    .map_err(|_| DaemonInvocationProblem::InvalidRequest)?,
                    tracedecay_application::PageRequest::new(100, None)
                        .map_err(|_| DaemonInvocationProblem::InvalidRequest)?,
                    deadline,
                    tracedecay_application::CancellationSignal::active(
                        cancellation.token_id.as_str(),
                    )
                    .map_err(|_| DaemonInvocationProblem::InvalidRequest)?,
                    request.clone(),
                )
                .await
                .map_err(|_| DaemonInvocationProblem::Unavailable)
            }
        }
    }

    /// Close every invocation admission gate that can be closed without
    /// awaiting, so no new provider, code-index, or project-runtime work is
    /// admitted once shutdown has been *requested* — not merely once this
    /// owner's drain phase is reached.
    ///
    /// The invocation owner sits behind the producer phase in the daemon
    /// shutdown plan, so its join is not polled until those producers settle.
    /// Wiring this into the owner's synchronous `cancel` side closes the gates
    /// at prepare time and, critically, keeps them closed even if the
    /// coordinator later aborts the drain runner. Idempotent.
    pub(super) fn cancel_admissions(&self) {
        // Counts cancel *requests*, not distinct transitions: the owner's
        // synchronous cancel side is intentionally idempotent, and a repeat
        // request after a coordinator retry is itself worth observing.
        hotpath::gauge!("daemon.invocation_state.cancel_admissions_total").inc(1_u64);
        self.service.cancel_admissions();
        self.github_credential_lifecycle.shutdown();
    }

    #[hotpath::measure(label = "daemon.invocation_state.shutdown", future = true)]
    pub(super) async fn shutdown(&self) -> bool {
        self.service.begin_shutdown().await;
        self.github_credential_lifecycle.shutdown();
        self.code_index_schedulers.shutdown().await;
        self.lsp_session_registry.lock().await.expire_at(u64::MAX);
        let expired = self.service.expire_all().await;
        if !expired {
            // A false expire-all means invocation sessions survived the
            // drain; record the incomplete shutdown instead of hiding it
            // behind the boolean.
            hotpath::gauge!("daemon.invocation_state.shutdown_incomplete_total").inc(1_u64);
        }
        expired
    }
}

/// One multi-root operation parsed and family-validated once per request.
/// Per-root execution clones the typed value instead of re-deserializing the
/// identical operation JSON for every admitted root.
pub(super) enum ParsedMultiRootOperationV1 {
    /// Boxed: the work invocation is ~1KiB against a ~33-byte sibling, and one
    /// of these is cloned per admitted root.
    Work(Box<WorkApplicationInvocationV1>),
    Surface {
        operation: ApplicationSurfaceOperation,
        request: Value,
    },
}

fn parse_multi_root_operation(
    operation: &tracedecay_application::MultiRootOperationV1,
) -> std::result::Result<ParsedMultiRootOperationV1, DaemonInvocationProblem> {
    match operation {
        tracedecay_application::MultiRootOperationV1::Work { request } => {
            let request = serde_json::from_value::<WorkApplicationInvocationV1>(request.clone())
                .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
            if !matches!(request, WorkApplicationInvocationV1::Views(_)) {
                return Err(DaemonInvocationProblem::InvalidRequest);
            }
            Ok(ParsedMultiRootOperationV1::Work(Box::new(request)))
        }
        tracedecay_application::MultiRootOperationV1::Git { request }
        | tracedecay_application::MultiRootOperationV1::Feedback { request }
        | tracedecay_application::MultiRootOperationV1::Impact { request }
        | tracedecay_application::MultiRootOperationV1::Query { request } => {
            let wire = serde_json::from_value::<FederatedSurfaceRequestV1>(request.clone())
                .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
            if !multi_root_family_allows(operation, wire.operation) {
                return Err(DaemonInvocationProblem::InvalidRequest);
            }
            Ok(ParsedMultiRootOperationV1::Surface {
                operation: wire.operation,
                request: wire.request,
            })
        }
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
        let state_memory = state.code_index_schedulers.process_resident_memory();
        let cloned_memory = cloned.code_index_schedulers.process_resident_memory();

        assert!(Arc::ptr_eq(&state_memory, &cloned_memory));
        assert_eq!(
            state_memory.snapshot().limit_bytes,
            tracedecay_runtime_core::resident_memory::detected_process_resident_memory_limit_v1()
                .get()
        );
    }
}
