//! `DaemonEngine`: the per-daemon-generation state shared by every accepted
//! connection, plus its project publication and routing methods.
//!
//! Holds the store administration, invocation state, open gates, owner
//! registries and lifecycle handles that one daemon generation owns.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

#[cfg(unix)]
#[derive(Clone, Default)]
pub(super) struct DaemonEngine {
    pub(super) lifecycle: DaemonLifecycle,
    /// Closed post-handshake operations backed by daemon-owned session actors.
    /// Git and feedback remain unavailable until their authoritative request
    /// owners register daemon-minted handles; no client-side fallback exists.
    pub(super) invocation: DaemonInvocationState,
    /// Project-scoped canonical application routers served by the daemon's
    /// standalone authenticated loopback HTTP listener.
    pub(super) http_application_registry: http_application::DaemonHttpApplicationRegistry,
    /// Lightweight per-proxy leases keep one reconnecting client from
    /// consuming every bulk slot while preserving reserved control capacity.
    pub(super) per_client_admission: DaemonPerClientAdmission,
    /// One coordinator owns the project-server registry, scheduler registry,
    /// and the writer gate that orders all mutations of either identity map.
    pub(super) store_administration: StoreAdministration,
    /// Per-canonical-route gates plus a bounded, route-local warm-up task
    /// registry. Weak gates disappear after the last waiter; deterministic
    /// route failures remain only for their short retry backoff.
    pub(super) project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    /// Per-logical-owner transition guards. Task-map locks are released before
    /// stale owners are awaited; this guard alone spans retirement so a
    /// concurrent activation or rekey cannot publish a replacement early.
    maintenance_transition_gates: Arc<tokio::sync::Mutex<MaintenanceTransitionGates>>,
    #[cfg(test)]
    pub(super) project_open_attempts: Arc<AtomicUsize>,
    #[cfg(test)]
    pub(super) memory_repair_start_attempts: Arc<AtomicUsize>,
    #[cfg(test)]
    pub(super) automation_config_probe_attempts: Arc<AtomicUsize>,
    #[cfg(test)]
    pub(super) automation_configured_override: Arc<AtomicBool>,
    #[cfg(test)]
    pub(super) automation_scheduler_exit_barrier:
        Arc<tokio::sync::Mutex<Option<Arc<scheduler::AutomationSchedulerExitBarrier>>>>,
    #[cfg(test)]
    pub(super) automation_scheduler_state_changed: Arc<tokio::sync::Notify>,
    /// Client versions whose skew was already logged. Proxy clients reconnect
    /// per request, so without this the mismatch would flood the daemon log.
    logged_client_version_skews: Arc<tokio::sync::Mutex<HashSet<String>>>,
    /// Client processes already told to refresh their tool catalog during
    /// this daemon generation. The set is process-local by design: a daemon
    /// restart creates a new generation and permits one fresh notification.
    pub(super) catalog_refresh_notified_clients:
        Arc<tokio::sync::Mutex<HashSet<CatalogRefreshClientKey>>>,
    /// Prevents capacity exhaustion from flooding the daemon log.
    pub(super) catalog_refresh_saturation_logged: Arc<AtomicBool>,
    /// Git-metadata watcher (design D3/D5). Default-constructed inert; the real
    /// config-driven watcher is installed by `run_foreground_unix` via
    /// [`DaemonEngine::with_git_watcher`] before the accept loop starts.
    git_watcher: git_watch::GitWatcher,
    /// Platform-neutral retention owner. The Unix watcher may wake this task,
    /// but never owns its cadence or lifecycle.
    maintenance_coordinator: maintenance::MaintenanceCoordinator,
    /// PR reconciliation task, retained so shutdown never leaves it writing.
    pr_autotrack_task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

/// Retain one daemon-owned Git index transaction service for the project store
/// and reconcile any durable records before mutation owners become available.
/// Read-only core tools and edit previews do not depend on this service. The
/// service owns the store actor; constructing a second service for the same
/// database is rejected by the registry.
pub(super) async fn ensure_git_index_transactions_for_mutation_owners(
    store_administration: &StoreAdministration,
    session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    project_root: &Path,
    project_id: Option<&str>,
) -> Result<()> {
    let Some(project_id) = project_id else {
        // Linked/anonymous project opens without a durable project id cannot
        // own index-mutation authority; skip rather than invent an identity.
        return Ok(());
    };
    let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("git index transaction project identity is invalid: {error}"),
        }
    })?;
    let Some(repository_root) = crate::worktree::git_worktree_root(project_root) else {
        // Non-Git projects remain valid TraceDecay projects. They advertise no
        // Git mutation authority and must not fail project-open admission.
        return Ok(());
    };
    let observed_at = tracedecay_domain::UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_micros()),
        )
        .unwrap_or(i64::MAX),
    );
    store_administration
        .git_index_transaction_services()
        .ensure(session_db, repository_root, project_id, observed_at)
        .await
        .map(|_| ())
        .map_err(|error| TraceDecayError::Config {
            message: format!("git index transaction startup did not complete: {error}"),
        })
}

pub(super) fn ensure_context_scout_owner_before_advertising(
    project: &crate::tracedecay::TraceDecay,
) -> Result<()> {
    if project.store_layout().identity.project_id.is_none() {
        return Ok(());
    }
    let owner = project
        .context_scout_owner()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project Context Scout owner did not start".to_owned(),
        })?;
    if matches!(
        owner.startup_outcome(),
        crate::agents::context_scout_v2::ContextScoutDurableStartupOutcomeV1::Unavailable
    ) {
        return Err(TraceDecayError::Config {
            message: "project Context Scout durable owner is unavailable".to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
impl DaemonEngine {
    pub(super) fn with_profile_identity(
        mut self,
        profile_identity: profile_identity::LocalProfileIdentityAuthorityV1,
    ) -> Self {
        self.invocation
            .configure_github_read_only_credentials(&profile_identity);
        self.store_administration = self
            .store_administration
            .with_profile_identity(profile_identity);
        self
    }

    pub(super) fn with_http_application_registry(
        mut self,
        registry: http_application::DaemonHttpApplicationRegistry,
    ) -> Self {
        self.http_application_registry = registry;
        self
    }

    /// Installs the config-driven git-metadata watcher on this engine. Called
    /// once by `run_foreground_unix` before the accept loop.
    pub(super) fn with_git_watcher(mut self, watcher: git_watch::GitWatcher) -> Self {
        self.git_watcher = watcher;
        self
    }

    pub(super) fn with_maintenance_coordinator(
        mut self,
        coordinator: maintenance::MaintenanceCoordinator,
    ) -> Self {
        self.maintenance_coordinator = coordinator;
        self
    }

    pub(super) async fn with_pr_autotrack_task(self, task: JoinHandle<()>) -> Self {
        *self.pr_autotrack_task.lock().await = Some(task);
        self
    }

    pub(super) async fn maintenance_transition_gate(
        &self,
        key: &ProjectServerKey,
    ) -> Arc<MaintenanceTransitionGate> {
        maintenance_transition_gate(&self.maintenance_transition_gates, key).await
    }

    /// Runs destructive branch administration before any project server is
    /// opened for the request, under the daemon-wide store administration gate.
    pub(super) async fn execute_branch_admin(
        &self,
        handshake: &DaemonHandshake,
        action: crate::branch::BranchAdminAction,
    ) -> Result<crate::branch::BranchAdminReport> {
        self.store_administration
            .execute_branch_admin_for_handshake(handshake, action)
            .await
    }

    /// Returns the client version to log for this handshake, once per distinct
    /// skewed version; repeat connections from the same client return `None`.
    pub(super) async fn client_version_skew_to_log(
        &self,
        handshake: &DaemonHandshake,
    ) -> Option<String> {
        let skew = client_version_skew(&handshake.client_version, binary_version())?;
        let mut logged = self.logged_client_version_skews.lock().await;
        logged.insert(skew.clone()).then_some(skew)
    }

    /// Logs a `daemon_version_skew` event when this handshake's client runs a
    /// different binary version, deduped per distinct client version.
    pub(super) async fn log_client_version_skew(&self, handshake: &DaemonHandshake) {
        let Some(client_version) = self.client_version_skew_to_log(handshake).await else {
            return;
        };
        let hint = version_skew_action(binary_version(), &client_version).to_string();
        log_daemon_event(
            "daemon_version_skew",
            &[
                ("daemon_version", binary_version().to_string()),
                ("client_version", client_version),
                ("hint", hint),
            ],
        );
    }

    /// Claims the one catalog-refresh notification for this client in the
    /// current daemon generation. Only proxies that already advertised the
    /// capability are eligible. `initialize` and `tools/list` mark the client
    /// current without emitting because those requests already fetch the new
    /// generation's catalog.
    ///
    /// `catalog_is_provisional` marks a discovery answer served from the
    /// warming bootstrap route, before the project graph is open. That catalog
    /// is not the published one — its `tracedecay_context` budget is the
    /// conservative warming budget rather than the node-count budget — so it
    /// must not mark the client current. Leaving such a client unmarked is
    /// exactly what arms its notification for the first request after warm-up
    /// completes; marking it would strand the provisional catalog for the rest
    /// of the daemon's life, because this set is never otherwise cleared.
    pub(super) async fn claim_catalog_refresh(
        &self,
        handshake: &DaemonHandshake,
        request_line: &str,
        catalog_is_provisional: bool,
    ) -> Option<CatalogRefreshClientKey> {
        if !valid_client_instance_id(&handshake.client_instance_id) {
            return None;
        }
        let request = serde_json::from_str::<JsonRpcRequest>(request_line).ok()?;
        if request.method == HOOK_EVENT_METHOD {
            return None;
        }
        if catalog_is_provisional {
            return None;
        }
        let catalog_is_current = matches!(request.method.as_str(), "initialize" | "tools/list");
        if !catalog_is_current
            && (!handshake.tool_list_changed_capable || handshake.catalog_version.is_empty())
        {
            return None;
        }
        let key = CatalogRefreshClientKey::from_handshake(handshake);
        let mut notified_clients = self.catalog_refresh_notified_clients.lock().await;
        if notified_clients.contains(&key) {
            return None;
        }
        if notified_clients.len() >= MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION {
            drop(notified_clients);
            if !self
                .catalog_refresh_saturation_logged
                .swap(true, Ordering::Relaxed)
            {
                log_daemon_event(
                    "catalog_refresh",
                    &[
                        ("outcome", "skipped".to_string()),
                        ("reason", "client_capacity_reached".to_string()),
                        (
                            "capacity",
                            MAX_CATALOG_REFRESH_CLIENTS_PER_GENERATION.to_string(),
                        ),
                    ],
                );
            }
            return None;
        }
        notified_clients.insert(key.clone());
        drop(notified_clients);
        if catalog_is_current {
            return None;
        }
        Some(key)
    }

    pub(super) async fn release_catalog_refresh(&self, key: CatalogRefreshClientKey) {
        self.catalog_refresh_notified_clients
            .lock()
            .await
            .remove(&key);
    }

    #[cfg(test)]
    pub(super) async fn project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        let cancellation = CancellationToken::new();
        self.project_server_until_cancelled(handshake, &cancellation)
            .await
    }

    async fn project_server_until_cancelled(
        &self,
        handshake: &DaemonHandshake,
        cancellation: &CancellationToken,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        if let Some(server) = self.cached_project_server(handshake).await? {
            return Ok(server);
        }

        let cached = self
            .store_administration
            .with_writer_until_cancelled(cancellation, || {
                self.open_project_server_until_cancelled(handshake, cancellation)
            })
            .await
            .ok_or_else(project_open_cancellation_error)??;
        let (_key, project_path, server, _inserted) = cached;
        project_open_cancellation_checkpoint(cancellation)?;
        Ok(self.activate_project_server(project_path, server).await)
    }

    pub(super) async fn cached_project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Option<Arc<crate::mcp::McpServer>>> {
        self.cached_project_server_for_requirement(handshake, ProjectServerRequirement::Core)
            .await
    }

    async fn cached_project_server_for_requirement(
        &self,
        handshake: &DaemonHandshake,
        requirement: ProjectServerRequirement,
    ) -> Result<Option<Arc<crate::mcp::McpServer>>> {
        let (project_path, route) = Self::project_route(handshake)?;
        let cached = {
            let mut servers = self.store_administration.project_servers().lock().await;
            servers
                .get_route_and_touch_for(&route, requirement)
                .map(|(_, server)| Arc::clone(server))
        };
        let Some(server) = cached else {
            return Ok(None);
        };
        self.ensure_registered_project_route(&project_path, handshake.allow_init)
            .await?;
        Ok(Some(
            self.activate_project_server(project_path, server).await,
        ))
    }

    pub(super) async fn begin_project_open(
        &self,
        handshake: DaemonHandshake,
        initialize_request: Option<JsonRpcRequest>,
    ) -> Result<ProjectOpenTaskClaim> {
        let (project_path, route) = Self::project_route(&handshake)?;
        let tasks = project_open_tasks(&self.project_open_gates).await;
        let engine = self.clone();
        let open_handshake = handshake.clone();
        Ok(Box::pin(start_lifecycle_project_open(
            &tasks,
            self.lifecycle.clone(),
            route,
            project_path,
            initialize_request,
            move |cancellation| async move {
                engine
                    .project_server_until_cancelled(&open_handshake, &cancellation)
                    .await
            },
        ))
        .await)
    }

    /// Rejects ambient working directories before scheduling project warm-up.
    ///
    /// Host MCP clients may start from `$HOME` and include that directory in
    /// their handshake. Opening it as a project would perform graph and index
    /// work before session-store resolution eventually notices the missing
    /// enrollment. Registry alias and repository-identity lookups preserve
    /// linked-worktree routing without manufacturing path-derived authority.
    pub(super) async fn ensure_registered_project_route(
        &self,
        project_path: &Path,
        allow_init: bool,
    ) -> Result<()> {
        ensure_registered_project_route(&self.store_administration, project_path, allow_init).await
    }

    pub(super) async fn schedule_project_server_warmup(
        &self,
        handshake: DaemonHandshake,
        initialize_request: JsonRpcRequest,
    ) -> Result<()> {
        if self.cached_project_server(&handshake).await?.is_some() {
            return Ok(());
        }
        match Box::pin(self.begin_project_open(handshake, Some(initialize_request))).await? {
            ProjectOpenTaskClaim::InFlight(_) => Ok(()),
            ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
            ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
        }
    }

    pub(super) async fn project_server_for_request(
        &self,
        handshake: &DaemonHandshake,
        requirement: ProjectServerRequirement,
    ) -> Result<Arc<crate::mcp::McpServer>> {
        if let Some(server) = self
            .cached_project_server_for_requirement(handshake, requirement)
            .await?
        {
            return Ok(server);
        }
        let (project_path, _) = Self::project_route(handshake)?;
        // Foreground requests must never pin a connection while a cold project
        // warm-up runs. The open task remains tracked and continues in the
        // background after this bounded wait expires.
        let claim = Box::pin(self.begin_project_open(handshake.clone(), None)).await?;
        match claim {
            ProjectOpenTaskClaim::InFlight(mut state) => {
                let publication = async {
                    loop {
                        if let Some(server) = self
                            .cached_project_server_for_requirement(handshake, requirement)
                            .await?
                        {
                            return Ok(server);
                        }
                        let current = state.borrow().clone();
                        match current {
                            ProjectOpenTaskState::Opening => {
                                tokio::select! {
                                    changed = state.changed() => {
                                        changed.map_err(|_| TraceDecayError::Config {
                                            message: "project open task ended before reporting an outcome"
                                                .to_string(),
                                        })?;
                                    }
                                    () = tokio::time::sleep(Duration::from_millis(25)) => {}
                                }
                            }
                            ProjectOpenTaskState::Ready => {
                                return Err(TraceDecayError::Config {
                                    message: "project open completed without publishing a server"
                                        .to_string(),
                                });
                            }
                            ProjectOpenTaskState::Failed(failure) => {
                                return Err(failure.to_error());
                            }
                        }
                    }
                };
                // Riding out an open is a park, not work: the admission slot is
                // released for the wait's duration so a tool that needs no project
                // owner is never shed by a queue of warming clients. The wait stays
                // bounded by PROJECT_OPEN_REQUEST_DEADLINE inside the helper.
                park_admission(wait_for_project_open_publication(
                    &project_path,
                    publication,
                ))
                .await
            }
            ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
            ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
        }
    }

    pub(super) async fn cached_project_open_failure(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<Option<ProjectOpenFailure>> {
        let (_, route) = Self::project_route(handshake)?;
        let tasks = project_open_tasks(&self.project_open_gates).await;
        Ok(tasks.cached_failure(&route).await)
    }

    pub(super) async fn shutdown_project_open_tasks(&self) {
        project_open_tasks(&self.project_open_gates)
            .await
            .shutdown()
            .await;
    }

    /// Opens or resolves a project server while writer administration is held.
    /// Watcher and scheduler activation happen only after this returns so those
    /// components can acquire the same coordinator without recursive locking.
    #[cfg(test)]
    pub(super) async fn open_project_server(
        &self,
        handshake: &DaemonHandshake,
    ) -> Result<(ProjectServerKey, PathBuf, Arc<crate::mcp::McpServer>, bool)> {
        let cancellation = CancellationToken::new();
        self.open_project_server_until_cancelled(handshake, &cancellation)
            .await
    }

    pub(super) async fn open_project_server_until_cancelled(
        &self,
        handshake: &DaemonHandshake,
        cancellation: &CancellationToken,
    ) -> Result<(ProjectServerKey, PathBuf, Arc<crate::mcp::McpServer>, bool)> {
        let Some(project_path) = handshake.project_path.as_ref() else {
            return Err(TraceDecayError::Config {
                message: "project server requested without project_path".to_string(),
            });
        };
        let canonical_project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.clone());
        self.ensure_registered_project_route(&canonical_project_path, handshake.allow_init)
            .await?;
        let composition = production_project_server(
            &self.store_administration,
            self.project_open_gates.as_ref(),
            &self.invocation,
            &self.http_application_registry,
            &canonical_project_path,
            handshake,
            ProductionProjectCompositionRuntime::Unix(Box::new(self.clone())),
            cancellation,
            #[cfg(test)]
            Some(&self.project_open_attempts),
        )
        .await?;
        if composition.inserted {
            self.spawn_project_maintenance_activation(
                composition.key.clone(),
                composition.canonical_project_path.clone(),
                handshake.clone(),
                Arc::clone(&composition.server),
            );
        }
        Ok((
            composition.key,
            composition.canonical_project_path,
            composition.server,
            composition.inserted,
        ))
    }

    pub(super) fn project_route(handshake: &DaemonHandshake) -> Result<(PathBuf, ProjectRouteKey)> {
        project_route_for_handshake(handshake)
    }

    async fn activate_project_server(
        &self,
        project_path: PathBuf,
        server: Arc<crate::mcp::McpServer>,
    ) -> Arc<crate::mcp::McpServer> {
        // A freshly-handshaken project should be watched even on a cache hit
        // (the watcher may have started after this server was cached).
        self.git_watcher.ensure_watching(&project_path).await;
        server
    }

    fn spawn_project_maintenance_activation(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
        server: Arc<crate::mcp::McpServer>,
    ) {
        let repair_key = key.clone();
        let repair_project_path = project_path.clone();
        let repair_handshake = handshake.clone();
        let engine = self.clone();
        spawn_lifecycle_automation_scheduler_activation(self.lifecycle.clone(), async move {
            engine
                .activate_project_maintenance(repair_key, repair_project_path, repair_handshake)
                .await;
        });
        let engine = self.clone();
        spawn_lifecycle_automation_scheduler_activation(self.lifecycle.clone(), async move {
            let cg = server.cg().await;
            engine
                .activate_automation_scheduler_for_open_project(key, project_path, handshake, cg)
                .await;
        });
    }

    async fn activate_project_maintenance(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) {
        let transition = self.maintenance_transition_gate(&key).await;
        let _transition = transition.lock().await;
        self.store_administration
            .with_writer(|| async move {
                if self
                    .store_administration
                    .project_servers()
                    .lock()
                    .await
                    .get(&key)
                    .is_none()
                {
                    return;
                }
                self.start_memory_repair_scheduler(
                    key.clone(),
                    project_path.clone(),
                    handshake.clone(),
                )
                .await;
            })
            .await;
    }

    pub(super) async fn rekey_project_maintenance(
        &self,
        old_key: &ProjectServerKey,
        new_key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
        acquire_new: bool,
    ) -> MaintenanceRekeyOutcome {
        let transition = self.maintenance_transition_gate(old_key).await;
        let _transition = transition.lock().await;
        let repair_retirement = self.retire_memory_repair_scheduler_locked(old_key).await;
        let automation_retirement = self.retire_automation_scheduler_locked(old_key).await;
        let retired = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
            if let Some(retirement) = repair_retirement {
                retirement.wait().await;
            }
            if let Some(retirement) = automation_retirement {
                retirement.wait().await;
            }
        })
        .await
        .is_ok();
        if !retired {
            log_daemon_event(
                "maintenance_rekey",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "retirement_timeout".to_string()),
                ],
            );
            return MaintenanceRekeyOutcome::Retiring;
        }
        if !acquire_new || !self.lifecycle.accepting() {
            return MaintenanceRekeyOutcome::Completed;
        }
        let repair_outcome = self
            .reconcile_memory_repair_scheduler_locked(
                new_key.clone(),
                project_path.clone(),
                handshake.clone(),
            )
            .await;
        let automation_outcome = self
            .reconcile_automation_scheduler_locked(new_key, project_path, handshake)
            .await;
        if matches!(
            repair_outcome,
            memory_repair_scheduler::MemoryRepairSchedulerReconcileOutcome::Retiring
        ) || matches!(
            automation_outcome,
            crate::dashboard::AutomationSchedulerReconcileOutcome::Retiring
        ) {
            MaintenanceRekeyOutcome::Retiring
        } else {
            MaintenanceRekeyOutcome::Completed
        }
    }

    pub(super) fn database_owner_reconciler(
        &self,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        route_registered: Arc<AtomicBool>,
        handshake: DaemonHandshake,
    ) -> crate::mcp::DatabaseOwnerReconciler {
        let engine = self.clone();
        Arc::new(move |fresh| {
            let engine = engine.clone();
            let current_key = Arc::clone(&current_key);
            let current_project_path = Arc::clone(&current_project_path);
            let route_registered = Arc::clone(&route_registered);
            let handshake = handshake.clone();
            Box::pin(async move {
                let transition = engine
                    .store_administration
                    .with_writer(|| async {
                        if !route_registered.load(Ordering::Acquire) {
                            return None;
                        }
                        let new_key = match ProjectServerKey::from_open_project(&fresh, &handshake)
                        {
                            Ok(key) => key,
                            Err(error) => {
                                eprintln!(
                                    "[tracedecay] failed to rekey daemon database owner: {error}"
                                );
                                return None;
                            }
                        };
                        let mut current = current_key.lock().await;
                        if *current == new_key {
                            return None;
                        }
                        let old_key = current.clone();
                        let rekeyed = engine
                            .store_administration
                            .project_servers()
                            .lock()
                            .await
                            .rekey(&old_key, &new_key);
                        if !rekeyed {
                            route_registered.store(false, Ordering::Release);
                        }
                        let project_path = fresh.project_root().to_path_buf();
                        let new_session_db = match new_key.owner.project_id.as_deref() {
                            Some(_) => engine
                                .store_administration
                                .registered_project_session_database(
                                    fresh.project_root(),
                                    fresh.store_layout(),
                                )
                                .await
                                .ok(),
                            None => None,
                        };
                        *current_project_path.lock().await = project_path;
                        *current = new_key.clone();
                        Some((
                            old_key,
                            new_key,
                            new_session_db,
                            fresh.project_root().to_path_buf(),
                            rekeyed,
                        ))
                    })
                    .await;
                if let Some((old_key, new_key, new_session_db, project_path, acquire_new)) =
                    transition
                {
                    let old_owner = old_key.owner.clone();
                    let new_owner = new_key.owner.clone();
                    let outcome = engine
                        .rekey_project_maintenance(
                            &old_key,
                            new_key,
                            project_path,
                            handshake,
                            acquire_new,
                        )
                        .await;
                    if outcome == MaintenanceRekeyOutcome::Completed {
                        if acquire_new
                            && engine.lifecycle.accepting()
                            && let Some(new_session_db) = new_session_db
                        {
                            engine
                                .store_administration
                                .session_temporal_refresh_schedulers()
                                .rekey_project(&old_owner, new_owner, new_session_db)
                                .await;
                        } else {
                            engine
                                .store_administration
                                .session_temporal_refresh_schedulers()
                                .retire_project(&old_owner)
                                .await;
                        }
                    }
                }
            })
        })
    }

    pub(super) async fn shutdown_background_tasks(&self) {
        self.shutdown_project_open_tasks().await;
        self.invocation.shutdown().await;
        self.store_administration
            .session_temporal_refresh_schedulers()
            .shutdown()
            .await;
        self.shutdown_automation_schedulers().await;
        self.shutdown_memory_repair_schedulers().await;
        self.store_administration
            .shutdown_retirement_reapers()
            .await;
        self.store_administration
            .shutdown_host_admission_replay()
            .await;

        self.maintenance_coordinator.shutdown().await;
        self.git_watcher.shutdown().await;
        if let Some(handle) = self.pr_autotrack_task.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    pub(super) async fn shutdown_servers(&self) {
        shutdown_project_servers(&self.store_administration).await;
    }

    #[cfg(test)]
    pub(super) async fn shutdown_all(&self) {
        self.lifecycle.begin_draining();
        self.shutdown_background_tasks().await;
        self.shutdown_servers().await;
    }
}
