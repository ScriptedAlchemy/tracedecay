//! In-process owner for the daemon's production project composition.
//!
//! Test and `test-transport` builds use this to drive the same composition the
//! daemon runs, against one isolated profile-and-projects root.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

#[cfg(all(unix, any(test, feature = "test-transport")))]
use super::bootstrap::set_owner_only_permissions;
#[cfg(any(test, feature = "test-transport"))]
use super::project_server_lifecycle::{detach_project_servers, shutdown_detached_project_servers};
#[cfg(any(test, feature = "test-transport"))]
use super::*;

/// Captures the daemon's exact native Git transaction precondition for
/// transport-parity tests. This is not compiled into production builds.
#[cfg(all(unix, feature = "test-transport"))]
#[doc(hidden)]
pub fn capture_exact_git_snapshot_for_test(
    repository_root: &Path,
    project_id: tracedecay_domain::ProjectId,
    repository_id: tracedecay_domain::RepositoryId,
    worktree_id: tracedecay_domain::WorktreeId,
    captured_at: tracedecay_domain::UtcMicros,
) -> crate::errors::Result<tracedecay_domain::RepositoryStateSnapshotV1> {
    git_transactions::capture_exact_snapshot_for_test(
        repository_root,
        project_id,
        repository_id,
        worktree_id,
        captured_at,
    )
}

#[cfg(any(test, feature = "test-transport"))]
struct ProductionProjectHarnessResourcesV1 {
    store_administration: StoreAdministration,
    invocation: DaemonInvocationState,
    _project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    servers: HashMap<PathBuf, Arc<crate::mcp::McpServer>>,
    _database_scope: crate::db::DaemonDatabaseScope,
    _lifecycle_lease: crate::lifecycle_lease::LifecycleLease,
}

/// In-process owner for the same production project composition used by the
/// daemon. The caller supplies one isolated root containing both the profile
/// and every project; live profile paths are rejected before any store opens.
#[cfg(any(test, feature = "test-transport"))]
#[doc(hidden)]
pub struct ProductionProjectCompositionHarnessV1 {
    isolation_root: PathBuf,
    profile_root: PathBuf,
    semantic_auto_download_enabled: bool,
    resources: Option<ProductionProjectHarnessResourcesV1>,
}

#[cfg(any(test, feature = "test-transport"))]
impl ProductionProjectCompositionHarnessV1 {
    pub async fn open(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self> {
        let live_profile_root = crate::config::user_data_dir().filter(|path| path.exists());
        Self::open_with_live_profile_root(isolation_root, project_roots, live_profile_root).await
    }

    async fn open_with_live_profile_root(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
        live_profile_root: Option<PathBuf>,
    ) -> Result<Self> {
        std::fs::create_dir_all(isolation_root.as_ref()).map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "failed to create production-composition isolation root '{}': {error}",
                    isolation_root.as_ref().display()
                ),
            }
        })?;
        let isolation_root = std::fs::canonicalize(isolation_root.as_ref()).map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "failed to canonicalize production-composition isolation root '{}': {error}",
                    isolation_root.as_ref().display()
                ),
            }
        })?;
        if let Some(live_profile_root) =
            live_profile_root.and_then(|path| std::fs::canonicalize(path).ok())
        {
            let overlaps_live_profile = isolation_root == live_profile_root
                || isolation_root.starts_with(&live_profile_root)
                || live_profile_root.starts_with(&isolation_root);
            if overlaps_live_profile {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "production-composition isolation root '{}' overlaps live profile '{}'",
                        isolation_root.display(),
                        live_profile_root.display()
                    ),
                });
            }
        }

        let profile_root = isolation_root.join("profile");
        std::fs::create_dir_all(&profile_root).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to create isolated production-composition profile '{}': {error}",
                profile_root.display()
            ),
        })?;
        #[cfg(unix)]
        set_owner_only_permissions(&profile_root, 0o700)?;

        let project_roots = project_roots
            .into_iter()
            .map(|project_root| {
                std::fs::canonicalize(&project_root).map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "failed to canonicalize production-composition project '{}': {error}",
                        project_root.display()
                    ),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if project_roots.is_empty() {
            return Err(TraceDecayError::Config {
                message: "production-composition harness requires at least one project".to_owned(),
            });
        }
        for project_root in &project_roots {
            if !project_root.starts_with(&isolation_root) || project_root.starts_with(&profile_root)
            {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "production-composition project '{}' must be inside isolation root '{}' and outside its profile",
                        project_root.display(),
                        isolation_root.display()
                    ),
                });
            }
        }

        let profile_identity = profile_identity::load_or_create(&profile_root)?;
        let lifecycle_lease = crate::lifecycle_lease::acquire_shared_for_profile(
            &profile_root,
            "in-process production composition",
        )?;
        let database_scope = crate::db::enter_daemon_database_scope(
            &profile_root,
            1,
            "in-process-production-composition",
        )?;
        let store_administration =
            StoreAdministration::default().with_profile_identity(profile_identity.clone());
        let invocation = DaemonInvocationState::default();
        invocation.configure_github_read_only_credentials(&profile_identity);
        let http_application_registry = http_application::DaemonHttpApplicationRegistry::default();
        install_http_application_cold_resolver(
            &http_application_registry,
            store_administration.clone(),
        )?;
        let project_open_gates = Arc::new(tokio::sync::Mutex::new(ProjectOpenGates::default()));
        let client_identity = DaemonClientIdentity {
            profile_root: profile_root.clone(),
            global_db_path: profile_root.join("global.db"),
        };
        let mut servers = HashMap::new();
        let mut semantic_auto_download_enabled = false;

        for (index, project_root) in project_roots.into_iter().enumerate() {
            let handshake = DaemonHandshake {
                client_version: binary_version().to_owned(),
                client_instance_id: format!("production-composition-harness-{index}"),
                client_identity: client_identity.clone(),
                scope_prefix: None,
                project_path: Some(project_root.clone()),
                timings: false,
                allow_init: true,
                allow_initialize_root_routing: false,
                tool_list_changed_capable: false,
                catalog_version: String::new(),
            };
            let (canonical_project_path, _) = project_route_for_handshake(&handshake)?;
            let composition = store_administration
                .with_writer(|| async {
                    let cancellation = CancellationToken::new();
                    production_project_server(
                        &store_administration,
                        &project_open_gates,
                        &invocation,
                        &http_application_registry,
                        &canonical_project_path,
                        &handshake,
                        ProductionProjectCompositionRuntime::Portable {
                            semantic_auto_download: false,
                            startup_catch_up: false,
                        },
                        &cancellation,
                        #[cfg(test)]
                        None,
                    )
                    .await
                })
                .await?;
            wait_for_production_composition_code_index(
                &invocation,
                &composition.canonical_project_path,
            )
            .await?;
            semantic_auto_download_enabled |= composition
                .semantic_auto_download_enabled
                .ok_or_else(|| TraceDecayError::Config {
                    message: "production-composition harness reused an unobserved semantic runtime"
                        .to_owned(),
                })?;
            servers.insert(composition.canonical_project_path, composition.server);
        }

        Ok(Self {
            isolation_root,
            profile_root,
            semantic_auto_download_enabled,
            resources: Some(ProductionProjectHarnessResourcesV1 {
                store_administration,
                invocation,
                _project_open_gates: project_open_gates,
                servers,
                _database_scope: database_scope,
                _lifecycle_lease: lifecycle_lease,
            }),
        })
    }

    #[cfg(test)]
    pub(super) async fn open_with_live_profile_root_for_test(
        isolation_root: impl AsRef<Path>,
        project_roots: impl IntoIterator<Item = PathBuf>,
        live_profile_root: PathBuf,
    ) -> Result<Self> {
        Self::open_with_live_profile_root(isolation_root, project_roots, Some(live_profile_root))
            .await
    }

    pub fn isolation_root(&self) -> &Path {
        &self.isolation_root
    }

    pub fn profile_root(&self) -> &Path {
        &self.profile_root
    }

    pub fn semantic_auto_download_enabled(&self) -> bool {
        self.semantic_auto_download_enabled
    }

    pub async fn read_profile_analytics_events(
        &self,
        query: &crate::global_db::AnalyticsEventQuery,
    ) -> Result<Vec<crate::global_db::AnalyticsEventRecord>> {
        let resources = self
            .resources
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "production-composition harness is shut down".to_owned(),
            })?;
        resources
            .store_administration
            .registered_profile_database()
            .await?
            .query_analytics_events(query)
            .await
            .map_err(|message| TraceDecayError::Database {
                message,
                operation: "read retained production profile analytics".to_owned(),
            })
    }

    pub fn server(&self, project_root: impl AsRef<Path>) -> Result<Arc<crate::mcp::McpServer>> {
        let canonical_project_path =
            std::fs::canonicalize(project_root.as_ref()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!(
                        "failed to canonicalize production-composition project '{}': {error}",
                        project_root.as_ref().display()
                    ),
                }
            })?;
        self.resources
            .as_ref()
            .and_then(|resources| resources.servers.get(&canonical_project_path))
            .cloned()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "project '{}' is not mounted in this production composition",
                    canonical_project_path.display()
                ),
            })
    }

    pub async fn project_data_root(&self, project_root: impl AsRef<Path>) -> Result<PathBuf> {
        Ok(self
            .server(project_root)?
            .cg()
            .await
            .store_layout()
            .data_root
            .clone())
    }

    pub async fn track_worktree_branch(
        &self,
        project_root: impl AsRef<Path>,
        worktree_root: impl AsRef<Path>,
        branch: &str,
    ) -> Result<crate::branch::BranchAddOutcome> {
        self.server(project_root)?
            .cg()
            .await
            .track_worktree_branch(worktree_root.as_ref(), branch)
            .await
    }

    pub async fn sync_tracked_worktree_branch(
        &self,
        project_root: impl AsRef<Path>,
        worktree_root: impl AsRef<Path>,
        branch: &str,
        query: &str,
    ) -> Result<(Option<String>, Option<String>, bool, bool)> {
        let graph = self.server(project_root)?.cg().await;
        let (database_path, _, _) = crate::tracedecay::TraceDecay::resolve_db_for_branch(
            graph.project_root(),
            &graph.store_layout().data_root,
            Some(branch),
        );
        let branch_graph = graph
            .sync_retained_worktree_branch(worktree_root.as_ref(), branch, &database_path)
            .await?;
        let contains_query = !branch_graph.search(query, 10).await?.is_empty();
        Ok((
            branch_graph.active_branch().map(str::to_owned),
            branch_graph.serving_branch().map(str::to_owned),
            branch_graph.is_fallback(),
            contains_query,
        ))
    }

    pub async fn call_tool(
        &self,
        project_root: impl AsRef<Path>,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<JsonRpcResponse> {
        let request = serde_json::from_value::<JsonRpcRequest>(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            },
        }))
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to construct production-composition tool request: {error}"),
        })?;
        self.server(project_root)?
            .handle_request(&request)
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "production-composition server returned no response for '{tool_name}'"
                ),
            })
    }

    pub async fn shutdown(mut self) {
        if let Some(resources) = self.resources.take() {
            shutdown_production_project_harness(resources).await;
        }
    }
}

#[cfg(any(test, feature = "test-transport"))]
async fn wait_for_production_composition_code_index(
    invocation: &DaemonInvocationState,
    project_root: &Path,
) -> Result<()> {
    timeout(Duration::from_secs(20), async {
        loop {
            if invocation
                .code_index_schedulers
                .latest_generation_id(project_root)
                .await
                .is_some()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| TraceDecayError::Config {
        message: format!(
            "production-composition code index did not publish for '{}'",
            project_root.display()
        ),
    })
}

#[cfg(any(test, feature = "test-transport"))]
impl Drop for ProductionProjectCompositionHarnessV1 {
    fn drop(&mut self) {
        let Some(resources) = self.resources.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(shutdown_production_project_harness(resources));
        }
    }
}

#[cfg(any(test, feature = "test-transport"))]
async fn shutdown_production_project_harness(mut resources: ProductionProjectHarnessResourcesV1) {
    resources
        .store_administration
        .join_project_server_retirements()
        .await;
    let servers = detach_project_servers(&resources.store_administration).await;
    resources.servers.clear();
    for server in &servers {
        server.ledger_writes_settled().await;
        server.shutdown_background_tasks().await;
    }
    resources
        .store_administration
        .session_temporal_refresh_schedulers()
        .shutdown()
        .await;
    resources
        .store_administration
        .shutdown_host_admission_replay()
        .await;
    resources.invocation.shutdown().await;
    shutdown_detached_project_servers(servers).await;
    drop(resources);
}
