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

use tracedecay_application::work::{
    WorkFederatedQueryAuthorityFutureV1, WorkFederatedQueryAuthorityPortV1,
};
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
use tracedecay_store_runtime::ShutdownStatus;

use super::*;
use tracedecay_runtime_core::logging::log_daemon_event;

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
    query_authority_provider: tracedecay_daemon_service::DaemonQueryAuthorityProviderV1,
    work_federated_query_authority: Arc<dyn WorkFederatedQueryAuthorityPortV1>,
    semantic_projection_scheduler:
        tracedecay_application::semantic_runtime::DaemonGlobalSemanticProjectionSchedulerV1,
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
            tracedecay_daemon_service::DaemonQueryAuthorityProviderV1::default();
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
                tracedecay_application::semantic_runtime::DaemonGlobalSemanticProjectionSchedulerV1::default(),
        }
    }

    pub(super) fn invocation_service(&self) -> DaemonInvocationService {
        self.service.clone()
    }

    pub(in crate::daemon) fn github_stack_coordinator(
        &self,
    ) -> Arc<tracedecay_application::stack_coordinator::DaemonGitHubStackCoordinatorV1> {
        self.service.github_stack_coordinator()
    }

    /// Mount the profile-owned background-worker plan before any projectless
    /// session or host-admission work can start. The exact `ProfileSessions`
    /// shard is the persisted user-profile authority; project configuration
    /// must never win this process-wide installation by opening first. The
    /// returned receipt carries the one process background CPU authority the
    /// plan installed, already mounted into session preparation.
    #[hotpath::skip]
    pub(super) async fn install_profile_worker_plan(
        &self,
        store_administration: &StoreAdministration,
        database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        profile_id: &tracedecay_domain::configuration::UserProfileId,
    ) -> Result<tracedecay_code_index::parallelism::InstalledCodeIndexWorkerPlanV1> {
        let configured = crate::config::read_or_initialize_profile_code_index_worker_selection(
            database, profile_id,
        )
        .await?;
        self.install_worker_selection(store_administration, configured)
    }

    /// Fixture composition for tests that drive a bare invocation state:
    /// installs the profile worker plan through a store administration bound
    /// to `profile_identity`, exactly as bootstrap does, so the plan's
    /// preparation resources are mounted alongside it.
    #[cfg(test)]
    pub(crate) async fn install_profile_worker_plan_for_test(
        &self,
        profile_identity: profile_identity::LocalProfileIdentityAuthorityV1,
        database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    ) -> Result<tracedecay_code_index::parallelism::InstalledCodeIndexWorkerPlanV1> {
        let profile_id = profile_identity.profile_id().clone();
        let store_administration =
            StoreAdministration::default().with_profile_identity(profile_identity);
        self.install_profile_worker_plan(&store_administration, database, &profile_id)
            .await
    }

    /// Charge one already-resolved worker selection against this daemon's own
    /// resident-memory authority, then mount the process resources session
    /// preparation meters against — that same resident-memory authority and
    /// the background CPU authority the plan installed — into the store
    /// administration's session runtimes. Keeping both steps here means the
    /// persisted-profile path, the production harness, and every in-process
    /// test engine install the exact same plan for the same selection and can
    /// never install a plan without its preparation resources.
    pub(super) fn install_worker_selection(
        &self,
        store_administration: &StoreAdministration,
        configured: tracedecay_domain::configuration::CodeIndexWorkerSelectionV1,
    ) -> Result<tracedecay_code_index::parallelism::InstalledCodeIndexWorkerPlanV1> {
        let resident_memory = self.code_index_schedulers.process_resident_memory();
        let resident_snapshot = resident_memory.snapshot();
        let installed = tracedecay_code_index::parallelism::install_worker_plan(
            configured,
            resident_snapshot
                .limit_bytes
                .saturating_sub(resident_snapshot.used_bytes),
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("code-index worker plan refused: {error}"),
        })?;
        store_administration
            .configure_codex_preparation_resources(
                resident_memory,
                Arc::clone(&installed.background_cpu),
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to configure Codex preparation resources: {error}"),
            })?;
        Ok(installed)
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
            drop(
                tracedecay_application::semantic_runtime::unregister_project_semantic_runtime(root),
            );
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
    ) -> tracedecay_application::advisory::github_runtime::ProfileGitHubReadOnlyCredentialMountOutcomeV1
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
        scope: &tracedecay_contracts::ResolvedScope,
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
        scope: &tracedecay_contracts::ResolvedScope,
        cursor_keys: &tracedecay_session_temporal_store::SessionTemporalCursorKeyProvider,
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
        scope: &tracedecay_contracts::ResolvedScope,
        expected_revision: &tracedecay_domain::configuration::ConfigurationRevisionId,
        cursor_keys: &tracedecay_session_temporal_store::SessionTemporalCursorKeyProvider,
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
    ) -> Arc<dyn WorkFederatedQueryAuthorityPortV1> {
        Arc::clone(&self.work_federated_query_authority)
    }

    pub(super) fn restore_initial_query_authority_for_project(
        &self,
        project_root: &Path,
        profile_id: tracedecay_domain::configuration::UserProfileId,
        scope: tracedecay_contracts::ResolvedScope,
        state: crate::config::retrieval::RetrievalProfileStateV1,
        cursor_keys: Arc<tracedecay_session_temporal_store::SessionTemporalCursorKeyProvider>,
    ) -> std::result::Result<
        tracedecay_daemon_service::QueryAuthorityProviderStatusV1,
        tracedecay_daemon_service::QueryAuthorityUpdateErrorV1,
    > {
        let status = self
            .query_authority_provider
            .install_evaluated_initial_state(profile_id, scope, state.clone(), cursor_keys)?;
        if !tracedecay_application::semantic_runtime::commit_project_initial_semantic_roots(
            project_root.to_path_buf(),
            &state,
        ) {
            return Err(
                tracedecay_daemon_service::QueryAuthorityUpdateErrorV1::ActivationNotCurrent,
            );
        }
        Ok(status)
    }

    pub(super) fn query_activation_registrar(
        &self,
        project_root: &Path,
        session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    ) -> Arc<dyn tracedecay_application::semantic_runtime::RetrievalProfileActivationObserverV1>
    {
        Arc::new(
            tracedecay_daemon_service::DaemonQueryActivationRegistrarV1::new(
                self.query_authority_provider.clone(),
                self.code_index_schedulers.clone(),
                project_root.to_path_buf(),
                session_db,
            ),
        )
    }

    #[hotpath::measure(label = "daemon.invocation_state.code_index_mount", future = true)]
    #[allow(
        clippy::too_many_arguments,
        reason = "Mount composition binds project identity, store, semantic lifetime and graph publication owners explicitly."
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "Code-index mount is one generation-bind and scheduler-attach sequence."
    )]
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
            dyn tracedecay_application::semantic_runtime::SemanticVectorGraphProviderV1,
        > = Arc::new(
            code_index_scheduler::semantic_vector_graph::DaemonSemanticVectorGraphProviderV1::new(
                project_id.clone(),
                canonical_project_root.clone(),
                self.code_index_schedulers.clone(),
                graph_runtime.code_graph_seat_port(),
                Arc::clone(&graph_publication_database),
                graph_runtime.semantic_vector_operation_task_owner(),
            ),
        );
        let semantic_schedule = semantic_runtime
            .zip(semantic_lifecycle.clone())
            .zip(semantic_resources)
            .zip(code_index_scheduler::identity::worktree_id_for(project_root).ok())
            .and_then(|(((handle, lifecycle), resources), worktree_id)| {
                let graph = Arc::clone(&vector_graph);
                tracedecay_application::semantic_runtime::production_saved_generation_schedule_hook(
                    tracedecay_application::semantic_runtime::SavedGenerationScheduleHookParametersV1 {
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
                // Composition resolves the resident ceiling against this host
                // before the runtime is offered, so this refusal means the
                // worktree mounts with no semantic scheduling at all rather
                // than with a fabricated memory budget.
                .inspect_err(|error| {
                    tracing::warn!(
                        event = "semantic_projection_schedule",
                        outcome = "hook_unavailable",
                        error = ?error,
                        "semantic projection hook could not be built for this worktree"
                    );
                })
                .ok()
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
                semantic_lifecycle,
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
    #[expect(
        clippy::too_many_lines,
        reason = "Multi-root execute is one scoped dispatch across the admitted root set."
    )]
    pub(super) async fn execute_multi_root_for_project(
        &self,
        store_administration: &StoreAdministration,
        active_project_root: &Path,
        request_id: String,
        request: tracedecay_contracts::MultiRootExecuteRequestV1,
        observed_at: tracedecay_domain::UtcMicros,
        deadline: tracedecay_contracts::Deadline,
        cancellation: tracedecay_contracts::CancellationContext,
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
        let continuation_valid = match request.continuation.as_ref() {
            None => request.page == 0,
            Some(continuation) => {
                continuation.validate().is_ok()
                    && continuation.scope_set_digest() == scope_set.digest()
                    && continuation.query_digest() == &query_digest
                    && continuation.order_digest() == &order_digest
                    && continuation.next_page() == request.page
                    && continuation.root_generations().len() == scope_set.roots().len()
                    && continuation.root_cursors().len() == scope_set.roots().len()
                    && scope_set.roots().iter().all(|root| {
                        continuation
                            .root_generation(&root.scope().scope_digest)
                            .is_some()
                            && continuation
                                .root_cursor(&root.scope().scope_digest)
                                .is_some()
                    })
            }
        };
        if !continuation_valid {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
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
                    tracedecay_contracts::ApplicationProblem::cancelled_before_admission(),
                );
            }
            if deadline.is_elapsed_at(observed_at)
                || deadline.is_elapsed_at(tracedecay_contracts::clock::now_micros())
            {
                return DaemonInvocationResponse::application_problem(
                    request_id,
                    tracedecay_contracts::ApplicationProblem::timed_out_before_admission(),
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
            let Some(_registry_context) = registry_context else {
                let Ok(generation) = denied_root_generation(scope) else {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::InvalidRequest,
                    );
                };
                generations.push(generation);
                continue;
            };
            // A registered project may contribute more than one exact linked
            // worktree. The persisted locator is the authorized root for this
            // scope; resolving only by project id aliases every linked scope
            // back to the project's primary checkout.
            let root = locator.canonical_root.clone();
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
            let query_operation = matches!(
                request.operation,
                tracedecay_contracts::MultiRootOperationV1::Query { .. }
            );
            let source_revision = match request.operation {
                tracedecay_contracts::MultiRootOperationV1::Git { .. } => {
                    match explicit_git_state(&root) {
                        Some(state) => Some(state),
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
                }
                tracedecay_contracts::MultiRootOperationV1::Query { .. } => self
                    .code_index_schedulers
                    .latest_generation_id(&root)
                    .await
                    .map(|generation| generation.as_str().to_owned()),
                tracedecay_contracts::MultiRootOperationV1::Work { .. }
                | tracedecay_contracts::MultiRootOperationV1::Feedback { .. }
                | tracedecay_contracts::MultiRootOperationV1::Impact { .. } => None,
            };
            let generation_outcome = match source_revision.as_deref() {
                Some(source_revision) => match frozen_root_generation(
                    scope,
                    scope_set.digest(),
                    source_revision,
                    &operation_value,
                ) {
                    Ok(generation) => tracedecay_domain::ScopeOutcome::Exact(Some(generation)),
                    Err(_) => {
                        return DaemonInvocationResponse::problem(
                            request_id,
                            DaemonInvocationProblem::InvalidRequest,
                        );
                    }
                },
                None => tracedecay_domain::ScopeOutcome::Exact(None),
            };
            if request.continuation.as_ref().is_some_and(|continuation| {
                continuation.root_generation(&scope.scope_digest) != Some(&generation_outcome)
            }) {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::InvalidRequest,
                );
            }
            let Ok(generation) = tracedecay_domain::RootScopeOutcomeV1::new(
                scope.scope_digest.clone(),
                generation_outcome.clone(),
            ) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::InvalidRequest,
                );
            };
            let child_cursor = request.continuation.as_ref().and_then(|continuation| {
                match continuation.root_cursor(&scope.scope_digest) {
                    Some(
                        tracedecay_domain::ScopeOutcome::Exact(cursor)
                        | tracedecay_domain::ScopeOutcome::Partial { value: cursor, .. },
                    ) => cursor.clone(),
                    _ => None,
                }
            });
            if request.continuation.is_some() && child_cursor.is_none() {
                contexts.push(context);
                generations.push(generation);
                outcomes.insert(
                    scope.scope_digest.clone(),
                    tracedecay_domain::ScopeOutcome::Exact(
                        tracedecay_contracts::MultiRootRootPageV1 {
                            value: Vec::new(),
                            next_cursor: None,
                        },
                    ),
                );
                continue;
            }
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
                        child_cursor,
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
                    tracedecay_contracts::ApplicationProblem::cancelled_before_admission(),
                );
            }
            if deadline.is_elapsed_at(tracedecay_contracts::clock::now_micros()) {
                return DaemonInvocationResponse::application_problem(
                    request_id,
                    tracedecay_contracts::ApplicationProblem::timed_out_before_admission(),
                );
            }
            let (outcome, served_revision) = match value {
                Ok((child_outcome, next_cursor)) => {
                    let served_revision = match child_outcome.as_ref() {
                        tracedecay_domain::ScopeOutcome::Exact(value)
                        | tracedecay_domain::ScopeOutcome::Partial { value, .. } => value
                            .get("generation")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        tracedecay_domain::ScopeOutcome::Denied
                        | tracedecay_domain::ScopeOutcome::Unavailable { .. } => None,
                    };
                    let outcome = if query_operation
                        && child_outcome.has_value()
                        && served_revision.is_none()
                    {
                        tracedecay_domain::ScopeOutcome::Unavailable {
                            reason:
                                tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
                        }
                    } else {
                        child_outcome.map(|value| tracedecay_contracts::MultiRootRootPageV1 {
                            value: vec![value],
                            next_cursor,
                        })
                    };
                    (outcome, served_revision)
                }
                Err(DaemonInvocationProblem::NotFoundOrNotAuthorized) => {
                    (tracedecay_domain::ScopeOutcome::Denied, None)
                }
                Err(_) => (
                    tracedecay_domain::ScopeOutcome::Unavailable {
                        reason: tracedecay_domain::ScopeUnavailableReasonV1::AuthorityUnavailable,
                    },
                    None,
                ),
            };
            let generation = if query_operation {
                match served_revision.as_deref() {
                    Some(served_revision) => {
                        let Ok(served_generation) = frozen_root_generation(
                            scope,
                            scope_set.digest(),
                            served_revision,
                            &operation_value,
                        ) else {
                            return DaemonInvocationResponse::problem(
                                request_id,
                                DaemonInvocationProblem::InvalidRequest,
                            );
                        };
                        let served_outcome =
                            tracedecay_domain::ScopeOutcome::Exact(Some(served_generation));
                        if served_outcome != generation_outcome {
                            return DaemonInvocationResponse::problem(
                                request_id,
                                DaemonInvocationProblem::InvalidRequest,
                            );
                        }
                        let Ok(generation) = tracedecay_domain::RootScopeOutcomeV1::new(
                            scope.scope_digest.clone(),
                            served_outcome,
                        ) else {
                            return DaemonInvocationResponse::problem(
                                request_id,
                                DaemonInvocationProblem::InvalidRequest,
                            );
                        };
                        generation
                    }
                    None => generation,
                }
            } else {
                generation
            };
            contexts.push(context);
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
        let query = tracedecay_contracts::MultiRootQueryRequestV1 {
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
        let page = match tracedecay_contracts::AuthorizedMultiRootQueryService::new(
            PrecomputedMultiRootQueryPort { outcomes },
        )
        .execute(query)
        {
            Ok(page) => page,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::InvalidRequest,
                );
            }
        };
        let Ok(application_request_id) = tracedecay_contracts::RequestId::new(request_id.clone())
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
        scope: &tracedecay_contracts::ResolvedScope,
        ordinal: usize,
        operation: &ParsedMultiRootOperationV1,
        observed_at: tracedecay_domain::UtcMicros,
        deadline: tracedecay_contracts::Deadline,
        cancellation: tracedecay_contracts::CancellationContext,
        project_admission: ProjectRuntimeRequestLeaseV1,
        request_cancellation: Option<CancellationToken>,
        child_cursor: Option<tracedecay_contracts::OpaqueCursor>,
    ) -> std::result::Result<
        (
            tracedecay_domain::ScopeOutcome<Value>,
            Option<tracedecay_contracts::OpaqueCursor>,
        ),
        DaemonInvocationProblem,
    > {
        match operation {
            ParsedMultiRootOperationV1::Work(request) => {
                let control_cancellation = tracedecay_contracts::CancellationSignal::active(
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
                    .map(|value| (tracedecay_domain::ScopeOutcome::Exact(value), None))
            }
            ParsedMultiRootOperationV1::Surface { operation, request } => {
                tracedecay_daemon_service::application_surface::invoke_multi_root_surface_request(
                    Arc::new(InProcessDaemonInvocationExecutor::with_project_admission(
                        self.clone(),
                        store_administration.clone(),
                        root.to_path_buf(),
                        scope.clone(),
                        project_admission,
                        request_cancellation,
                    )),
                    *operation,
                    tracedecay_contracts::RequestId::new(format!(
                        "request.multi-root.surface.{ordinal}"
                    ))
                    .map_err(|_| DaemonInvocationProblem::InvalidRequest)?,
                    tracedecay_contracts::PageRequest::new(100, child_cursor)
                        .map_err(|_| DaemonInvocationProblem::InvalidRequest)?,
                    deadline,
                    tracedecay_contracts::CancellationSignal::active(
                        cancellation.token_id.as_str(),
                    )
                    .map_err(|_| DaemonInvocationProblem::InvalidRequest)?,
                    request.clone(),
                )
                .await
                .map_err(|_| DaemonInvocationProblem::Unavailable)
                .and_then(|outcome| {
                    let next_cursor = match outcome.as_ref() {
                        tracedecay_domain::ScopeOutcome::Exact(value)
                        | tracedecay_domain::ScopeOutcome::Partial { value, .. } => {
                            value.get("next_cursor")
                        }
                        tracedecay_domain::ScopeOutcome::Denied
                        | tracedecay_domain::ScopeOutcome::Unavailable { .. } => None,
                    }
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|_| DaemonInvocationProblem::Unavailable)?;
                    Ok((outcome, next_cursor.flatten()))
                })
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
        // Code-index workers only observe `shutting_down` / closed admission
        // once cancel runs. Leaving this until the join lets an in-flight
        // reconcile (graph seat, follow-up pass) keep the worker alive for
        // the whole background-drain budget.
        self.code_index_schedulers.cancel();
    }

    #[hotpath::measure(label = "daemon.invocation_state.shutdown", future = true)]
    pub(super) async fn shutdown(&self) -> ShutdownStatus {
        let started = std::time::Instant::now();
        let step = |outcome: &str| {
            log_daemon_event(
                "daemon_shutdown",
                &[
                    ("outcome", outcome.to_string()),
                    ("owner", "invocation".to_string()),
                    ("elapsed_ms", started.elapsed().as_millis().to_string()),
                ],
            );
        };
        self.service.begin_shutdown().await;
        self.github_credential_lifecycle.shutdown();
        self.code_index_schedulers.cancel();
        step("invocation_admissions_closed");
        // The bounded wait may expire while a blocking reconcile is still
        // unwinding. The registry retains its worker until a retry joins it;
        // an incomplete sweep must keep the outer shutdown receipt unclean.
        let schedulers_timed_out = tokio::time::timeout(
            super::DAEMON_TASK_ABORT_DEADLINE,
            self.code_index_schedulers.shutdown(),
        )
        .await
        .is_err();
        step("code_index_schedulers_join_returned");
        if schedulers_timed_out {
            log_daemon_event(
                "daemon_shutdown",
                &[
                    ("outcome", "code_index_scheduler_pending".to_string()),
                    (
                        "reason",
                        "reconcile_join_exceeded_abort_deadline".to_string(),
                    ),
                ],
            );
        }
        self.lsp_session_registry.lock().await.expire_at(u64::MAX);
        step("lsp_sessions_expired");
        let expired = self.service.expire_all().await;
        step("invocation_service_expired");
        if !expired {
            hotpath::gauge!("daemon.invocation_state.shutdown_incomplete_total").inc(1_u64);
            ShutdownStatus::Failed("invocation runtime shutdown was incomplete".to_owned())
        } else if schedulers_timed_out {
            ShutdownStatus::TimedOut
        } else {
            ShutdownStatus::Clean
        }
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
    operation: &tracedecay_contracts::MultiRootOperationV1,
) -> std::result::Result<ParsedMultiRootOperationV1, DaemonInvocationProblem> {
    match operation {
        tracedecay_contracts::MultiRootOperationV1::Work { request } => {
            let request = serde_json::from_value::<WorkApplicationInvocationV1>(request.clone())
                .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
            if !matches!(request, WorkApplicationInvocationV1::Views(_)) {
                return Err(DaemonInvocationProblem::InvalidRequest);
            }
            Ok(ParsedMultiRootOperationV1::Work(Box::new(request)))
        }
        tracedecay_contracts::MultiRootOperationV1::Git { request }
        | tracedecay_contracts::MultiRootOperationV1::Feedback { request }
        | tracedecay_contracts::MultiRootOperationV1::Impact { request }
        | tracedecay_contracts::MultiRootOperationV1::Query { request } => {
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
    provider: tracedecay_daemon_service::DaemonQueryAuthorityProviderV1,
}

impl WorkFederatedQueryAuthorityPortV1 for DaemonWorkFederatedQueryAuthorityV1 {
    fn authority_for<'a>(
        &'a self,
        scope: &'a tracedecay_contracts::ResolvedScope,
    ) -> WorkFederatedQueryAuthorityFutureV1<'a> {
        Box::pin(async move {
            let mounted = self.schedulers.query_authority_for_scope(scope).await?;
            self.provider
                .federated_authority_for(scope, mounted.privacy_domain())
                .ok()
        })
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    #[tokio::test]
    async fn failed_lsp_lease_is_preserved_in_the_invocation_owner_receipt() {
        let state = DaemonInvocationState::default();
        let (started, observed) = tokio::sync::oneshot::channel();
        state
            .service
            .lsp_lease_tasks
            .start(
                tracedecay_daemon_protocol::LspSessionId::new("lsp-failed-shutdown")
                    .expect("lease identity"),
                async move {
                    let _ = started.send(());
                    panic!("lease worker failed");
                },
            )
            .await
            .expect("admit lease worker");
        observed.await.expect("lease worker was polled");
        let receipt = shutdown_coordination::join_shutdown_owners(
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            vec![shutdown_coordination::ShutdownOwner::with_deadline_status(
                "invocation",
                || {},
                move |_| async move { state.shutdown().await },
            )],
        )
        .await;
        assert_eq!(
            receipt.owners[0].status,
            ShutdownStatus::Failed("invocation runtime shutdown was incomplete".to_owned())
        );
        assert_eq!(receipt.unfinished(), &["invocation"]);
    }

    #[tokio::test]
    async fn cancel_admissions_then_empty_shutdown_is_prompt() {
        let state = DaemonInvocationState::default();
        state.cancel_admissions();
        state.cancel_admissions();
        let started = std::time::Instant::now();
        assert!(
            state.shutdown().await.is_clean(),
            "empty invocation shutdown must expire cleanly"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "empty code-index join must not spend the TERM grace"
        );
    }
}
